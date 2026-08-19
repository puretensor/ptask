//! pTask MCP server (v2.4.0) — the agent-native surface.
//!
//! Mounted two ways, same handler:
//!   - streamable-HTTP at `/mcp` inside `pt serve` (ptve's exact pattern),
//!     gated to the **hal** named token (bearer) — HAL is the consumer this
//!     surface exists for; other agents use the scoped REST API. Every
//!     mutation is journaled `actor=hal, source=mcp`.
//!   - stdio via `pt mcp` for local registration without a network hop;
//!     actor comes from `$PTASK_ACTOR` (config), source=mcp.
//!
//! Tools return compact JSON text — the consumer is a model, not a human.

use ptask_core::Db;
use ptask_core::event_log::EventCtx;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
};
use serde::Serialize;

fn json_ok<T: Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let payload = serde_json::to_string(value)
        .map_err(|e| McpError::internal_error(format!("serialize: {e}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(payload)]))
}

fn domain_err(e: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(format!("{e}"), None)
}

fn task_json(t: &ptask_core::tasks::Task) -> serde_json::Value {
    serde_json::json!({
        "id": t.id, "pt_id": t.pt_id, "title": t.title,
        "description": t.description, "priority": t.priority,
        "status": t.status, "created_at": t.created_at,
        "updated_at": t.updated_at, "deadline": t.deadline,
        "source_type": t.source_type,
    })
}

// ------------------------------------------------------------------ args

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IdArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct NextArg {
    /// Max tasks to return (default 10).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ListArg {
    /// Filter DSL, e.g. "(today | overdue) & p4" or "#infra & @ops". Omit for all pending.
    #[serde(default)]
    pub filter: Option<String>,
    /// Max tasks (default 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddArg {
    /// Task text. Quick-add tokens parse inline: p1-p5, @label, #project,
    /// ~30m, due:YYYY-MM-DD, deadline phrases ("by friday").
    pub text: String,
    /// Why the task exists (journaled as ai_reasoning).
    #[serde(default)]
    pub reason: Option<String>,
    /// PT-N/uuid of the task this was discovered while working on — records
    /// a discovered_from link.
    #[serde(default)]
    pub discovered_from: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EditArg {
    /// Task handle: PT-N, uuid, or title substring.
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// 1 (low) … 5 (critical).
    #[serde(default)]
    pub priority: Option<i64>,
    /// ISO date to set; empty string clears.
    #[serde(default)]
    pub deadline: Option<String>,
    /// Labels to add, e.g. ["domain:mgmt"].
    #[serde(default)]
    pub labels_add: Vec<String>,
    /// Labels to remove.
    #[serde(default)]
    pub labels_remove: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CaptureArg {
    /// Raw commitment/idea/incident text for the distillation inbox.
    pub text: String,
    /// Logical source (defaults to "mcp").
    #[serde(default)]
    pub source: Option<String>,
    /// Severity >= 3 takes the critical fast lane (immediate task).
    #[serde(default)]
    pub severity: Option<i64>,
    /// Stable client key for idempotent federation (re-sends dedupe).
    #[serde(default)]
    pub client_key: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchArg {
    /// FTS5 query over titles + descriptions.
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct DigestArg {
    /// Lookback window in days (default 7).
    #[serde(default)]
    pub days: Option<i64>,
}

// --------------------------------------------------------------- handler

#[derive(Clone)]
pub struct PtaskMcp {
    db: Db,
    actor: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<PtaskMcp>,
}

#[tool_router]
impl PtaskMcp {
    pub fn new(db: Db, actor: String) -> Self {
        Self {
            db,
            actor,
            tool_router: Self::tool_router(),
        }
    }

    fn ctx(&self) -> EventCtx {
        EventCtx {
            actor: self.actor.clone(),
            source: "mcp".into(),
            event_uuid: None,
        }
    }

    fn rescore(&self) {
        if let Err(e) = ptask_core::scoring::run_once(&self.db, false) {
            tracing::warn!(target: "ptask::mcp", error = %e, "post-mutation rescore failed");
        }
    }

    #[tool(
        description = "DAG-ready tasks in priority order (every dependency done, not snoozed). THE call for 'what should I work on'. Returns compact task JSON."
    )]
    async fn task_next(
        &self,
        Parameters(NextArg { limit }): Parameters<NextArg>,
    ) -> Result<CallToolResult, McpError> {
        let tasks = ptask_core::dag::next_ready(&self.db, limit.unwrap_or(10).clamp(1, 100))
            .map_err(domain_err)?;
        json_ok(&tasks.iter().map(task_json).collect::<Vec<_>>())
    }

    #[tool(
        description = "List pending tasks, optionally filtered by the pt filter DSL (e.g. '(today | overdue) & p4', '#infra', '@ops & p5')."
    )]
    async fn task_list(
        &self,
        Parameters(ListArg { filter, limit }): Parameters<ListArg>,
    ) -> Result<CallToolResult, McpError> {
        let expr = match filter.as_deref().filter(|f| !f.trim().is_empty()) {
            Some(f) => Some(ptask_core::filter::parse(f).map_err(domain_err)?),
            None => None,
        };
        let tasks = ptask_core::tasks::list_with_filter(
            &self.db,
            expr.as_ref(),
            Some("pending"),
            None,
            limit.unwrap_or(50).clamp(1, 500),
        )
        .map_err(domain_err)?;
        json_ok(&tasks.iter().map(task_json).collect::<Vec<_>>())
    }

    #[tool(
        description = "Create a task. Quick-add tokens parse inline (p4, @label, #project, ~30m, due:/deadline phrases). Pass discovered_from to link provenance."
    )]
    async fn task_add(
        &self,
        Parameters(AddArg {
            text,
            reason,
            discovered_from,
        }): Parameters<AddArg>,
    ) -> Result<CallToolResult, McpError> {
        let q = ptask_core::quickadd::parse(&text).map_err(domain_err)?;
        let new = ptask_core::NewTask {
            title: q.title.clone(),
            description: q.description.clone(),
            priority: q.priority.unwrap_or(2),
            deadline: q.deadline.clone(),
            source_type: "mcp".into(),
            ai_confidence: 1.0,
            ai_reasoning: reason.unwrap_or_default(),
        };
        let ext = ptask_core::Extensions {
            labels: q.labels.clone(),
            project: q.project.clone(),
            duration_min: q.duration_min,
            planned_at: None,
            energy: None,
            recurrence: q.recurrence.clone(),
            due_at: q.due.clone(),
        };
        let discovered_parent = discovered_from
            .as_deref()
            .map(|parent| ptask_core::tasks::resolve_for_lookup(&self.db, parent, true))
            .transpose()
            .map_err(domain_err)?;
        let t = ptask_core::tasks::create_with_extensions(&self.db, new, ext, &self.ctx())
            .map_err(domain_err)?;
        if let Some(parent) = discovered_parent {
            self.db
                .with_conn(|c| {
                    c.execute(
                        "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
                         VALUES (?1, ?2, 'discovered_from',
                                 strftime('%Y-%m-%dT%H:%M:%f','now') || '+00:00')",
                        rusqlite::params![t.id, parent.id],
                    )?;
                    Ok(())
                })
                .map_err(domain_err)?;
        }
        self.rescore();
        let mut v = task_json(&t);
        if !q.warnings.is_empty() {
            // Non-fatal quick-add caveats (e.g. a date phrase that resolved to
            // the past). PT-1267 backdated a deadline silently because these
            // were dropped on the MCP path — agents must see them.
            v["warnings"] = serde_json::json!(q.warnings);
        }
        json_ok(&v)
    }

    #[tool(description = "Full detail for one task: fields + attributed journal history.")]
    async fn task_show(
        &self,
        Parameters(IdArg { id }): Parameters<IdArg>,
    ) -> Result<CallToolResult, McpError> {
        let t = ptask_core::tasks::resolve_for_lookup(&self.db, &id, true).map_err(domain_err)?;
        let hist = ptask_core::event_log::history_for_task(&self.db, &t.id, 50)
            .map_err(domain_err)?
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "ts": e.ts, "event_type": e.event_type, "actor": e.actor,
                })
            })
            .collect::<Vec<_>>();
        let mut v = task_json(&t);
        v["history"] = serde_json::json!(hist);
        json_ok(&v)
    }

    #[tool(description = "Mark a task done.")]
    async fn task_done(
        &self,
        Parameters(IdArg { id }): Parameters<IdArg>,
    ) -> Result<CallToolResult, McpError> {
        let t = ptask_core::tasks::resolve_for_lookup(&self.db, &id, false).map_err(domain_err)?;
        ptask_core::tasks::mark_done(&self.db, &t, &self.ctx()).map_err(domain_err)?;
        self.rescore();
        json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id, "status": "done"}))
    }

    #[tool(description = "Dismiss a task (won't-do; distill won't resurrect it).")]
    async fn task_dismiss(
        &self,
        Parameters(IdArg { id }): Parameters<IdArg>,
    ) -> Result<CallToolResult, McpError> {
        let t = ptask_core::tasks::resolve_for_lookup(&self.db, &id, false).map_err(domain_err)?;
        ptask_core::tasks::dismiss(&self.db, &t.id, &self.ctx()).map_err(domain_err)?;
        self.rescore();
        json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id, "status": "dismissed"}))
    }

    #[tool(
        description = "Edit task fields (title/description/priority/deadline; empty-string deadline clears; labels_add/labels_remove edit labels, e.g. domain:eng / domain:mgmt)."
    )]
    async fn task_edit(
        &self,
        Parameters(EditArg {
            id,
            title,
            description,
            priority,
            deadline,
            labels_add,
            labels_remove,
        }): Parameters<EditArg>,
    ) -> Result<CallToolResult, McpError> {
        let t = ptask_core::tasks::resolve_for_lookup(&self.db, &id, true).map_err(domain_err)?;
        let ctx = self.ctx();
        if title.is_none()
            && description.is_none()
            && priority.is_none()
            && deadline.is_none()
            && labels_add.is_empty()
            && labels_remove.is_empty()
        {
            return Err(McpError::invalid_params("no fields to edit", None));
        }
        if title.is_some() || description.is_some() {
            ptask_core::tasks::update_text(
                &self.db,
                &t.id,
                title.as_deref(),
                description.as_deref(),
                &ctx,
            )
            .map_err(domain_err)?;
        }
        if let Some(p) = priority {
            if !(1..=5).contains(&p) {
                return Err(McpError::invalid_params("priority must be 1..5", None));
            }
            ptask_core::tasks::update_priority(&self.db, &t.id, p, &ctx).map_err(domain_err)?;
        }
        if let Some(dl) = deadline {
            let val = if dl.trim().is_empty() {
                None
            } else {
                Some(dl.as_str())
            };
            ptask_core::tasks::update_deadline(&self.db, &t.id, val, &ctx).map_err(domain_err)?;
        }
        if !labels_add.is_empty() || !labels_remove.is_empty() {
            ptask_core::tasks::modify_labels(&self.db, &t.id, &labels_add, &labels_remove, &ctx)
                .map_err(domain_err)?;
        }
        self.rescore();
        json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id}))
    }

    #[tool(
        description = "Atomically claim a task before working on it (todo/backlog/triage → in_progress). Errors if already claimed — the check-and-set is one SQL statement, so two agents can't both win."
    )]
    async fn task_claim(
        &self,
        Parameters(IdArg { id }): Parameters<IdArg>,
    ) -> Result<CallToolResult, McpError> {
        let t = ptask_core::tasks::resolve_for_lookup(&self.db, &id, false).map_err(domain_err)?;
        ptask_core::tasks::claim(&self.db, &t.id, &self.ctx()).map_err(domain_err)?;
        json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id, "claimed_by": self.actor}))
    }

    #[tool(
        description = "Capture raw text into the distillation inbox. severity>=3 creates a task immediately (incident fast lane). Pass a stable client_key to make re-sends idempotent (federation)."
    )]
    async fn task_capture(
        &self,
        Parameters(CaptureArg {
            text,
            source,
            severity,
            client_key,
        }): Parameters<CaptureArg>,
    ) -> Result<CallToolResult, McpError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(McpError::invalid_params("text must be non-empty", None));
        }
        let source = source.unwrap_or_else(|| "mcp".into());
        let source_file = client_key
            .clone()
            .unwrap_or_else(|| format!("mcp://{}", self.actor));
        // Idempotent federation: same client_key + text = the same fact.
        if client_key.is_some() {
            let dup: Option<i64> = self
                .db
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT id FROM raw_items WHERE source_file = ?1 AND text = ?2
                         ORDER BY id DESC LIMIT 1",
                        rusqlite::params![source_file, text],
                        |r| r.get(0),
                    )
                    .optional()?)
                })
                .map_err(domain_err)?;
            if let Some(id) = dup {
                return json_ok(&serde_json::json!({"id": id, "duplicate": true}));
            }
        }
        let row = ptask_core::raw_items::insert(&self.db, &text, &source, &source_file)
            .map_err(domain_err)?;
        let mut out = serde_json::json!({"id": row.id, "duplicate": false});
        if severity.is_some_and(|s| s >= 3) {
            let sev = severity.unwrap();
            let new = ptask_core::NewTask {
                title: text
                    .lines()
                    .next()
                    .unwrap_or(&text)
                    .chars()
                    .take(200)
                    .collect(),
                description: text.clone(),
                priority: if sev >= 4 { 5 } else { 4 },
                deadline: None,
                source_type: "incident".into(),
                ai_confidence: 1.0,
                ai_reasoning: format!("mcp fast-lane capture severity {sev}"),
            };
            let t = ptask_core::tasks::create_with_extensions(
                &self.db,
                new,
                ptask_core::Extensions::default(),
                &self.ctx(),
            )
            .map_err(domain_err)?;
            ptask_core::raw_items::mark_processed(&self.db, row.id).map_err(domain_err)?;
            self.rescore();
            out["task_uuid"] = serde_json::json!(t.id);
            out["pt_id"] = serde_json::json!(t.pt_id);
        }
        json_ok(&out)
    }

    #[tool(description = "Full-text search (FTS5) over task titles + descriptions, any status.")]
    async fn task_search(
        &self,
        Parameters(SearchArg { query, limit }): Parameters<SearchArg>,
    ) -> Result<CallToolResult, McpError> {
        let q = query.trim().to_string();
        if q.is_empty() {
            return Err(McpError::invalid_params("query must be non-empty", None));
        }
        let limit = limit.unwrap_or(20).clamp(1, 100) as i64;
        let rows: Vec<serde_json::Value> = self
            .db
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT t.id, t.pt_id, t.title, t.status_v2, t.priority
                     FROM tasks_fts f JOIN tasks t ON t.rowid = f.rowid
                     WHERE tasks_fts MATCH ?1
                     ORDER BY rank LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map((&q, limit), |r| {
                        Ok(serde_json::json!({
                            "id": r.get::<_, String>(0)?,
                            "pt_id": r.get::<_, Option<String>>(1)?,
                            "title": r.get::<_, String>(2)?,
                            "status": r.get::<_, String>(3)?,
                            "priority": r.get::<_, i64>(4)?,
                        }))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .map_err(domain_err)?;
        json_ok(&rows)
    }

    #[tool(
        description = "Session-priming digest: counts + recently done/dismissed/created over a lookback window, plus the current top of the ready queue. Call at session start to load task context."
    )]
    async fn task_digest(
        &self,
        Parameters(DigestArg { days }): Parameters<DigestArg>,
    ) -> Result<CallToolResult, McpError> {
        let v = ptask_core::digest::build(&self.db, days.unwrap_or(7)).map_err(domain_err)?;
        json_ok(&v)
    }
}

use rusqlite::OptionalExtension;

#[tool_handler]
impl ServerHandler for PtaskMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "pTask — PureTensor's sovereign task manager. Start sessions with \
                 task_digest (recent context) or task_next (what to work on). \
                 task_claim before starting work so parallel agents don't collide; \
                 task_add with discovered_from records provenance; task_capture \
                 (severity>=3) fast-lanes incidents into tasks."
                    .to_string(),
            )
    }
}

/// Serve the MCP handler over stdio — `pt mcp`. Blocks until the client
/// disconnects.
pub async fn serve_stdio(db: Db, actor: String) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let service = PtaskMcp::new(db, actor)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn task_add_rejects_invalid_provenance_before_creating() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());

        let result = mcp
            .task_add(Parameters(AddArg {
                text: "task that must not survive".into(),
                reason: None,
                discovered_from: Some("PT-999999".into()),
            }))
            .await;

        assert!(result.is_err());
        db.with_conn(|c| {
            let count: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
            assert_eq!(count, 0);
            Ok(())
        })
        .unwrap();
        assert_eq!(ptask_core::event_log::current_cursor(&db).unwrap(), 0);
    }

    #[tokio::test]
    async fn task_add_keeps_valid_provenance_link() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let parent = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("parent task"),
            &EventCtx::test(),
        )
        .unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());

        let result = mcp
            .task_add(Parameters(AddArg {
                text: "discovered child".into(),
                reason: None,
                discovered_from: parent.pt_id.clone(),
            }))
            .await;

        assert!(result.is_ok());
        db.with_conn(|c| {
            let links: i64 = c.query_row(
                "SELECT COUNT(*) FROM task_links
                 WHERE to_uuid = ?1 AND kind = 'discovered_from'",
                [&parent.id],
                |r| r.get(0),
            )?;
            assert_eq!(links, 1);
            Ok(())
        })
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // PT-1687 HAL CONTRACT — FROZEN, round 2. Added after review of the first
    // implementation, which fixed the raw_items race and opened a new one a
    // layer up.
    //
    // The old handler RETURNED EARLY on a duplicate, so the severity>=3
    // fast-lane never ran twice. Replacing that early return with a `duplicate`
    // flag removed the guard: a retried sev3 capture now re-enters the fast lane
    // and creates ANOTHER task every time. That is the same defect this ticket
    // exists to close, moved from raw_items to tasks — and worse, because a task
    // is operator-visible.
    //
    // Idempotency has to hold for the WHOLE capture, not just its first table.
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn pt1687_repeated_severity_capture_does_not_create_a_second_task() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());

        let arg = || {
            Parameters(CaptureArg {
                text: "[puresentinel sev3] ceph reports HEALTH_ERR".into(),
                source: Some("mcp".into()),
                severity: Some(3),
                client_key: Some("mcp://sentinel/incident-1".into()),
            })
        };

        mcp.task_capture(arg()).await.expect("first capture");
        mcp.task_capture(arg()).await.expect("retried capture");
        mcp.task_capture(arg()).await.expect("second retry");

        db.with_conn(|c| {
            let raws: i64 = c.query_row("SELECT COUNT(*) FROM raw_items", [], |r| r.get(0))?;
            let tasks: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
            assert_eq!(raws, 1, "three identical captures must leave one raw_item");
            assert_eq!(
                tasks, 1,
                "three identical sev3 captures must leave ONE task — a retry that \
                 re-enters the severity fast-lane recreates the incident the \
                 operator already has"
            );
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn pt1687_a_first_severity_capture_still_creates_its_task() {
        // The guard must not overshoot: a genuine first capture keeps its fast lane.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());

        let result = mcp
            .task_capture(Parameters(CaptureArg {
                text: "[puresentinel sev4] arx2 osd down".into(),
                source: Some("mcp".into()),
                severity: Some(4),
                client_key: Some("mcp://sentinel/incident-2".into()),
            }))
            .await
            .expect("capture");

        db.with_conn(|c| {
            let tasks: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
            assert_eq!(tasks, 1, "a first sev4 capture must still fast-lane a task");
            Ok(())
        })
        .unwrap();
        let _ = result;
    }
}
