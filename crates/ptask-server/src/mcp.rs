//! pTask MCP server (v2.4.0) — the agent-native surface.
//!
//! Mounted two ways, same handler:
//!   - streamable-HTTP at `/mcp` inside `pt serve` (ptve's exact pattern),
//!     gated to the **hal** named token (bearer) — HAL is the consumer this
//!     surface exists for; other agents use the scoped REST API. Every
//!     mutation is journaled `actor=hal, source=mcp`.
//!   - stdio via `pt mcp` for local registration without a network hop;
//!     actor comes from `$PTASK_MCP_ACTOR`, else `$PTASK_ACTOR` (config,
//!     default "mcp"), source=mcp.
//!
//! Tools return compact JSON text — the consumer is a model, not a human.

use ptask_core::Db;
use ptask_core::config::DispatchCfg;
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
    Ok(CallToolResult::success(vec![ContentBlock::text(payload)]))
}

fn domain_err(e: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(format!("{e}"), None)
}

async fn on_blocking<T, F>(f: F) -> Result<T, McpError>
where
    F: FnOnce() -> Result<T, McpError> + Send + 'static,
    T: Send + 'static,
{
    crate::blocking::db_value(f)
        .await
        .map_err(|e| McpError::internal_error(format!("blocking task: {e}"), None))?
}

fn rescore_db(db: &Db) {
    if let Err(e) = ptask_core::scoring::run_once(db, false) {
        tracing::warn!(target: "ptask::mcp", error = %e, "post-mutation rescore failed");
    }
}

fn task_json(t: &ptask_core::tasks::Task) -> serde_json::Value {
    serde_json::json!({
        "id": t.id, "pt_id": t.pt_id, "title": t.title,
        "description": t.description, "priority": t.priority,
        "status": t.status, "created_at": t.created_at,
        "updated_at": t.updated_at, "deadline": t.deadline,
        "source_type": t.source_type,
        "kind": t.kind, "deliverable": t.deliverable,
    })
}

fn with_goals(
    db: &Db,
    t: &ptask_core::tasks::Task,
    mut v: serde_json::Value,
) -> Result<serde_json::Value, McpError> {
    let eg = ptask_core::goals::effective_goal(db, &t.id).map_err(domain_err)?;
    v["goal_chain"] = ptask_core::goals::chain_json(&eg.chain);
    v["goal_source"] = serde_json::json!(eg.source.as_str());
    Ok(v)
}

// ------------------------------------------------------------------ args

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DoneArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
    /// The deadline you last saw for this task. When set, the call is
    /// refused if the task has moved on (another agent or the operator
    /// already completed that occurrence), so a retried or duplicate
    /// task_done never advances a recurring task twice. "" = it had none.
    #[serde(default)]
    pub expected_deadline: Option<String>,
    /// Closure evidence: what was done and how it was verified (commit, PR,
    /// test run, readback). Journaled with the completion, attributed to you.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DismissArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
    /// Why it is not being done (duplicate of PT-N, superseded, obsolete).
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoteArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring
    /// (a done or dismissed task by PT-N or uuid).
    pub id: String,
    /// The note: findings, evidence, a handover for the next worker.
    pub text: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IdArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClaimArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
    /// Lease in minutes (1..=1440). Keep it alive with task_heartbeat; when
    /// it runs out the operator's reclaim returns the task to todo. Omit for
    /// a claim that never expires on its own.
    #[serde(default)]
    pub lease_minutes: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HeartbeatArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
    /// Claim instance token returned by task_claim. Required: a heartbeat
    /// without one, or with a stale one, fails with "claim lost".
    #[serde(default)]
    pub claim_token: Option<String>,
    /// New lease from now, in minutes (1..=1440; default 30).
    #[serde(default)]
    pub lease_minutes: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReleaseArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
    /// Claim instance token returned by task_claim. Required: there is no
    /// force over MCP, so a missing or stale token is refused.
    #[serde(default)]
    pub claim_token: Option<String>,
    /// Why you are handing it back (blocked on X, out of scope, ...).
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DependArg {
    /// The dependent task (PT-N, number, uuid, or title substring).
    pub task: String,
    /// The prerequisite it depends on.
    pub on: String,
    /// Remove the edge instead of adding it.
    #[serde(default)]
    pub remove: bool,
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
    /// Task shape: "scout" (investigation, deliverable a report) or "ship"
    /// (implementation, deliverable a PR — the default).
    #[serde(default)]
    pub kind: Option<String>,
    /// What finishing it produces: "report" | "pr" | "none". Defaults to
    /// the kind's deliverable.
    #[serde(default)]
    pub deliverable: Option<String>,
    /// Acceptance criteria (definition of done), one checkable condition
    /// each: the task will not close until every one is checked.
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// When a near-certain duplicate exists (an open task, or one closed in
    /// the last 14 days, scoring at least 0.75 with the same identifier-like
    /// words), create nothing and return the candidates instead (`ok` is
    /// false, `created` is false, `skipped` is true). Without it, or below
    /// that score, the task is created and any candidates come back as
    /// possible_duplicates.
    #[serde(default)]
    pub skip_if_duplicate: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CriteriaArg {
    /// Task handle: PT-N, bare number, task uuid, or a title substring.
    pub id: String,
    /// Criteria to add.
    #[serde(default)]
    pub add: Vec<String>,
    /// Numbers of criteria to check (they hold now).
    #[serde(default)]
    pub check: Vec<i64>,
    /// Numbers of criteria to uncheck.
    #[serde(default)]
    pub uncheck: Vec<i64>,
    /// Evidence journaled with each check (the command, the readback).
    #[serde(default)]
    pub evidence: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MergeArg {
    /// The duplicate (open): PT-N, bare number, task uuid, or title substring.
    pub duplicate: String,
    /// The task it duplicates.
    pub into: String,
    /// Why (journaled on both tasks).
    #[serde(default)]
    pub reason: Option<String>,
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
    /// Free text over titles + descriptions: every word must match;
    /// punctuation and AND/OR/NOT are plain text; a trailing * matches a prefix.
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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ApprovalRequestArg {
    /// email | ebay | spend | destroy | external | budget | other
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub note: Option<String>,
    /// UTF-8 payload stored as a file.
    #[serde(default)]
    pub payload: Option<String>,
    /// JSON object, canonicalised (sorted keys, compact, Python json.dumps bytes) and stored;
    /// the digest covers the canonical bytes. Integers beyond 64 bits and non-integers of
    /// magnitude 2^53 or more are refused; send them as strings. Advertised as an object so every MCP client can see and fill it.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
    pub payload_json: Option<serde_json::Value>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub payload_name: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub expires_in: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ApprovalListArg {
    /// pending (default) | approved | rejected | withdrawn | expired | all
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ApprovalIdArg {
    /// AP-n or the row uuid.
    pub id: String,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct GoalListArg {
    /// Include achieved and abandoned goals (default: active only).
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GoalIdArg {
    /// G-n or the row uuid.
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GoalLinkArg {
    /// Task handle: PT-N, uuid, or title substring.
    pub task: String,
    /// Goal handle: G-n or uuid.
    pub goal: String,
}

// --------------------------------------------------------------- handler

/// Telegram notify settings for MCP-originated approval requests.
#[derive(Clone, Default)]
pub struct McpNotify {
    pub cfg: DispatchCfg,
    pub dash_url: Option<String>,
    pub tg_buttons: bool,
}

#[derive(Clone)]
pub struct PtaskMcp {
    db: Db,
    actor: String,
    notify: McpNotify,
    #[allow(dead_code)]
    tool_router: ToolRouter<PtaskMcp>,
}

#[tool_router]
impl PtaskMcp {
    pub fn new(db: Db, actor: String) -> Self {
        Self {
            db,
            actor,
            notify: McpNotify::default(),
            tool_router: Self::tool_router(),
        }
    }

    pub fn with_notify(mut self, notify: McpNotify) -> Self {
        self.notify = notify;
        self
    }

    fn ctx(&self) -> EventCtx {
        EventCtx {
            actor: self.actor.clone(),
            source: "mcp".into(),
            event_uuid: None,
            command: None,
        }
    }

    #[tool(
        description = "DAG-ready tasks in priority order (every dependency done, not snoozed). THE call for 'what should I work on'. Returns compact task JSON."
    )]
    async fn task_next(
        &self,
        Parameters(NextArg { limit }): Parameters<NextArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let tasks = ptask_core::dag::next_ready(&db, limit.unwrap_or(10).clamp(1, 100))
                .map_err(domain_err)?;
            let mut out = Vec::with_capacity(tasks.len());
            for t in &tasks {
                out.push(with_goals(&db, t, task_json(t))?);
            }
            json_ok(&out)
        })
        .await
    }

    #[tool(
        description = "List pending tasks, optionally filtered by the pt filter DSL (e.g. '(today | overdue) & p4', '#infra', '@ops & p5')."
    )]
    async fn task_list(
        &self,
        Parameters(ListArg { filter, limit }): Parameters<ListArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let expr = match filter.as_deref().filter(|f| !f.trim().is_empty()) {
                Some(f) => Some(ptask_core::filter::parse(f).map_err(domain_err)?),
                None => None,
            };
            let tasks = ptask_core::tasks::list_with_filter(
                &db,
                expr.as_ref(),
                Some("pending"),
                None,
                limit.unwrap_or(50).clamp(1, 500),
            )
            .map_err(domain_err)?;
            json_ok(&tasks.iter().map(task_json).collect::<Vec<_>>())
        })
        .await
    }

    #[tool(
        description = "Create a task. Quick-add tokens parse inline (p4, @label, #project, ~30m, due:/deadline phrases). Pass discovered_from to link provenance. Pass acceptance: the definition of done as checkable conditions; the task will not close until each is checked (task_criteria). The reply lists possible_duplicates (open, or closed in the last 14 days, with a similar title): if one is the same work, work or note that task and task_merge the new one into it. Pass skip_if_duplicate=true to create nothing when a near-certain duplicate (score >= 0.75 and the same identifier-like words) exists (the reply then has ok=false, created=false, skipped=true)."
    )]
    async fn task_add(
        &self,
        Parameters(AddArg {
            text,
            reason,
            discovered_from,
            kind,
            deliverable,
            acceptance,
            skip_if_duplicate,
        }): Parameters<AddArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let q = ptask_core::quickadd::parse(&text).map_err(domain_err)?;
            let (mut new, mut ext) = q.task_parts("mcp");
            new.ai_reasoning = reason.unwrap_or_default();
            (ext.kind, ext.deliverable) =
                ptask_core::tasks::kind_and_deliverable(kind.as_deref(), deliverable.as_deref())
                    .map_err(domain_err)?;
            ext.discovered_from = discovered_from
                .as_deref()
                .map(|parent| ptask_core::tasks::resolve_for_lookup(&db, parent, true))
                .transpose()
                .map_err(domain_err)?
                .map(|parent| parent.id);
            ext.acceptance = acceptance;
            let dupes = ptask_core::dupes::similar(
                &db,
                &new.title,
                None,
                ptask_core::dupes::DEFAULT_THRESHOLD,
                5,
            )
            .map_err(domain_err)?;
            if skip_if_duplicate && ptask_core::dupes::refuses(&new.title, &dupes) {
                return json_ok(&serde_json::json!({
                    "ok": false, "created": false, "skipped": true,
                    "possible_duplicates": dupes,
                }));
            }
            // The link commits with the task (or neither does): a link
            // written afterwards could fail for an already-created task, and
            // the agent's retry would duplicate it.
            let t = ptask_core::tasks::create_with_extensions(&db, new, ext, &ctx)
                .map_err(domain_err)?;
            rescore_db(&db);
            let mut v = task_json(&t);
            if !dupes.is_empty() {
                v["possible_duplicates"] = serde_json::json!(dupes);
            }
            json_ok(&v)
        })
        .await
    }

    #[tool(
        description = "Full detail for one task: fields, attributed journal history, notes (findings and closure evidence, oldest first), claim, duplicate_of / merged_in, and acceptance criteria."
    )]
    async fn task_show(
        &self,
        Parameters(IdArg { id }): Parameters<IdArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, true).map_err(domain_err)?;
            let hist = ptask_core::event_log::history_for_task(&db, &t.id, 50)
                .map_err(domain_err)?
                .into_iter()
                .map(|e| {
                    serde_json::json!({
                        "ts": e.ts, "event_type": e.event_type, "actor": e.actor,
                    })
                })
                .collect::<Vec<_>>();
            let mut v = with_goals(&db, &t, task_json(&t))?;
            v["history"] = serde_json::json!(hist);
            v["notes"] = serde_json::json!(
                ptask_core::notes::list(&db, &t.id, ptask_core::notes::MAX_NOTES_LISTED)
                    .map_err(domain_err)?
            );
            let links = ptask_core::dupes::links(&db, &t.id).map_err(domain_err)?;
            v["duplicate_of"] = serde_json::json!(links.duplicate_of);
            v["merged_in"] = serde_json::json!(links.merged_in);
            // Open prerequisites: non-empty means task_done will be refused.
            let blockers = ptask_core::tasks::open_blockers(&db, &t.id).map_err(domain_err)?;
            v["blocked_by"] = serde_json::json!(blockers);
            v["criteria"] =
                serde_json::json!(ptask_core::criteria::list(&db, &t.id).map_err(domain_err)?);
            v["claim"] =
                serde_json::json!(ptask_core::claims::get(&db, &t.id).map_err(domain_err)?);
            json_ok(&v)
        })
        .await
    }

    #[tool(
        description = "Mark a task done. Pass note with the verification evidence (commit, PR, test run, readback): it is journaled with the completion, so the close is not a bare claim. Recurring tasks are advanced in place (status stays pending) and the JSON reports status=advanced plus next_deadline."
    )]
    async fn task_done(
        &self,
        Parameters(DoneArg {
            id,
            expected_deadline,
            note,
        }): Parameters<DoneArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, false).map_err(domain_err)?;
            let t = ptask_core::tasks::expect_deadline(t, expected_deadline.as_deref())
                .map_err(domain_err)?;
            let outcome = ptask_core::tasks::mark_done_noted(&db, &t, note.as_deref(), &ctx)
                .map_err(domain_err)?;
            rescore_db(&db);
            match outcome {
                ptask_core::tasks::DoneOutcome::Completed => json_ok(&serde_json::json!({
                    "ok": true, "pt_id": t.pt_id, "status": "done"
                })),
                ptask_core::tasks::DoneOutcome::Advanced { next_deadline } => {
                    json_ok(&serde_json::json!({
                        "ok": true,
                        "pt_id": t.pt_id,
                        "status": "advanced",
                        "next_deadline": next_deadline,
                    }))
                }
            }
        })
        .await
    }

    #[tool(
        description = "Dismiss a task (won't-do; distill won't resurrect it). Pass note with the reason (duplicate of PT-N, superseded, obsolete)."
    )]
    async fn task_dismiss(
        &self,
        Parameters(DismissArg { id, note }): Parameters<DismissArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, false).map_err(domain_err)?;
            ptask_core::tasks::dismiss_noted(&db, &t.id, note.as_deref(), &ctx)
                .map_err(domain_err)?;
            rescore_db(&db);
            json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id, "status": "dismissed"}))
        })
        .await
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
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, true).map_err(domain_err)?;
            if title.is_none()
                && description.is_none()
                && priority.is_none()
                && deadline.is_none()
                && labels_add.is_empty()
                && labels_remove.is_empty()
            {
                return Err(McpError::invalid_params("no fields to edit", None));
            }
            if priority.is_some_and(|p| !(1..=5).contains(&p)) {
                return Err(McpError::invalid_params("priority must be 1..5", None));
            }
            let edit = ptask_core::tasks::TaskEdit {
                title: title.as_deref(),
                description: description.as_deref(),
                priority,
                deadline: deadline
                    .as_deref()
                    .map(|d| if d.trim().is_empty() { None } else { Some(d) }),
                labels_add: &labels_add,
                labels_remove: &labels_remove,
            };
            ptask_core::tasks::edit_atomic(&db, &t.id, edit, &ctx).map_err(domain_err)?;
            rescore_db(&db);
            json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id}))
        })
        .await
    }

    #[tool(
        description = "A task's acceptance criteria (definition of done): add conditions, check the ones that now hold (with evidence: the command, the readback), uncheck any that stopped holding. Returns the list. task_done refuses while any criterion is unchecked; check them only when they are true."
    )]
    async fn task_criteria(
        &self,
        Parameters(CriteriaArg {
            id,
            add,
            check,
            uncheck,
            evidence,
        }): Parameters<CriteriaArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, false).map_err(domain_err)?;
            // One transaction: a bad batch (duplicate numbers, over-long
            // evidence, missing criterion) changes nothing, so a retry does
            // not add the same criteria twice.
            let criteria = ptask_core::criteria::apply_batch(
                &db,
                &t.id,
                &add,
                &check,
                &uncheck,
                evidence.as_deref(),
                &ctx,
            )
            .map_err(domain_err)?;
            let unchecked = criteria.iter().filter(|c| !c.done).count();
            json_ok(&serde_json::json!({
                "pt_id": t.pt_id, "criteria": criteria, "unchecked": unchecked,
            }))
        })
        .await
    }

    #[tool(
        description = "Likely duplicates of a task: open tasks, or tasks closed in the last 14 days, with a similar title (lexical; best first). Read-only."
    )]
    async fn task_duplicates(
        &self,
        Parameters(IdArg { id }): Parameters<IdArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, true).map_err(domain_err)?;
            let dupes = ptask_core::dupes::similar(
                &db,
                &t.title,
                Some(&t.id),
                ptask_core::dupes::DEFAULT_THRESHOLD,
                10,
            )
            .map_err(domain_err)?;
            json_ok(&serde_json::json!({"pt_id": t.pt_id, "possible_duplicates": dupes}))
        })
        .await
    }

    #[tool(
        description = "Merge a duplicate into the task it duplicates, in one step: the duplicate is dismissed as duplicate_of, every task that depended on it now depends on the target (so nothing is silently unblocked), its prerequisites, labels, recurrence, goal, discovered_from links and subtasks carry over, and the target keeps the higher priority. Use instead of dismissing a duplicate by hand. The duplicate must be open; a dismissed target, a done target that would unblock open dependents, or a dependency cycle refuses the merge."
    )]
    async fn task_merge(
        &self,
        Parameters(MergeArg {
            duplicate,
            into,
            reason,
        }): Parameters<MergeArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let dup = ptask_core::tasks::resolve_for_lookup(&db, &duplicate, false)
                .map_err(domain_err)?;
            let target =
                ptask_core::tasks::resolve_for_lookup(&db, &into, true).map_err(domain_err)?;
            let m = ptask_core::dupes::merge(&db, &dup.id, &target.id, reason.as_deref(), &ctx)
                .map_err(domain_err)?;
            rescore_db(&db);
            let mut v = serde_json::to_value(&m).map_err(domain_err)?;
            v["ok"] = serde_json::json!(true);
            json_ok(&v)
        })
        .await
    }

    #[tool(
        description = "Append a note to a task: findings, partial progress, evidence, a handover for the next worker. Append-only and attributed to you; task_show and the worker brief carry the trail. Works on done/dismissed tasks too (by PT-N or uuid), e.g. evidence that arrives after the close."
    )]
    async fn task_note(
        &self,
        Parameters(NoteArg { id, text }): Parameters<NoteArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, false).map_err(domain_err)?;
            let note = ptask_core::notes::add(&db, &t.id, &text, &ctx).map_err(domain_err)?;
            json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id, "note": note}))
        })
        .await
    }

    #[tool(
        description = "Atomically claim a task before working on it (todo/backlog/triage → in_progress, held by you). Errors if already claimed, naming the holder — the check-and-set is one SQL statement, so two agents can't both win. Returns claim_token: pass it to task_heartbeat and task_release. Pass lease_minutes for work that should come back if you die: renew it with task_heartbeat; an expired lease is free to claim, or can be reclaimed to todo."
    )]
    async fn task_claim(
        &self,
        Parameters(ClaimArg { id, lease_minutes }): Parameters<ClaimArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, false).map_err(domain_err)?;
            let claim =
                ptask_core::claims::claim(&db, &t.id, lease_minutes, &ctx).map_err(domain_err)?;
            let mut v = serde_json::json!({
                "ok": true, "pt_id": t.pt_id, "claimed_by": claim.by,
                "claim_expires_at": claim.expires_at,
                "claim_token": claim.token,
            });
            v = with_goals(&db, &t, v)?;
            json_ok(&v)
        })
        .await
    }

    #[tool(
        description = "Renew your claim's lease (default 30 minutes from now). Pass claim_token from task_claim. Errors with \"claim lost\" when that instance is no longer current (released, reclaimed, closed, retaken): stop working on it and do not close it; re-claim if it is still yours to do."
    )]
    async fn task_heartbeat(
        &self,
        Parameters(HeartbeatArg {
            id,
            claim_token,
            lease_minutes,
        }): Parameters<HeartbeatArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, true).map_err(domain_err)?;
            let claim = ptask_core::claims::heartbeat(
                &db,
                &t.id,
                lease_minutes.unwrap_or(30),
                claim_token.as_deref().unwrap_or(""),
                &ctx,
            )
            .map_err(domain_err)?;
            json_ok(&serde_json::json!({
                "ok": true, "pt_id": t.pt_id, "claimed_by": claim.by,
                "claim_expires_at": claim.expires_at,
            }))
        })
        .await
    }

    #[tool(
        description = "Hand back a task you claimed (in_progress → todo) without closing it: you are stopping, blocked, or it is not yours to do. Pass claim_token from task_claim. Only your own claim instance; the operator releases others' from the CLI. There is no force over MCP."
    )]
    async fn task_release(
        &self,
        Parameters(ReleaseArg {
            id,
            claim_token,
            reason,
        }): Parameters<ReleaseArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, false).map_err(domain_err)?;
            ptask_core::claims::release(
                &db,
                &t.id,
                false,
                reason.as_deref(),
                claim_token.as_deref(),
                &ctx,
            )
            .map_err(domain_err)?;
            rescore_db(&db);
            json_ok(&serde_json::json!({"ok": true, "pt_id": t.pt_id, "status": "todo"}))
        })
        .await
    }

    #[tool(
        description = "Promote an investigation to implementation: flips kind scout → ship on the SAME task row (deliverable report → pr). Never close a scout and open a ship duplicate — promotion must leave the open-task count unchanged. Errors on a terminal task; reopen it first."
    )]
    async fn task_promote(
        &self,
        Parameters(IdArg { id }): Parameters<IdArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::tasks::resolve_for_lookup(&db, &id, false).map_err(domain_err)?;
            ptask_core::tasks::promote(&db, &t.id, &ctx).map_err(domain_err)?;
            json_ok(&serde_json::json!({
                "ok": true, "pt_id": t.pt_id, "kind": "ship", "deliverable": "pr"
            }))
        })
        .await
    }

    #[tool(
        description = "Add or remove a dependency edge: `task` cannot be closed until `on` is done or dismissed. Chains (3 on 2 on 1) and fan-out (2 and 3 both on 1) are both fine; cycles are rejected. Pass remove=true to drop the edge."
    )]
    async fn task_depend(
        &self,
        Parameters(DependArg { task, on, remove }): Parameters<DependArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let from =
                ptask_core::tasks::resolve_for_lookup(&db, &task, true).map_err(domain_err)?;
            let to = ptask_core::tasks::resolve_for_lookup(&db, &on, true).map_err(domain_err)?;
            if remove {
                ptask_core::tasks::remove_dependency(&db, &from.id, &to.id, &ctx)
                    .map_err(domain_err)?;
            } else {
                ptask_core::tasks::add_dependency(&db, &from.id, &to.id, &ctx)
                    .map_err(domain_err)?;
            }
            rescore_db(&db);
            let blockers = ptask_core::tasks::open_blockers(&db, &from.id).map_err(domain_err)?;
            json_ok(&serde_json::json!({
                "ok": true, "task": from.pt_id, "depends_on": to.pt_id,
                "removed": remove, "blocked_by": blockers,
            }))
        })
        .await
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
        let db = self.db.clone();
        let ctx = self.ctx();
        let actor = self.actor.clone();
        on_blocking(move || {
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err(McpError::invalid_params("text must be non-empty", None));
            }
            let source = source.unwrap_or_else(|| "mcp".into());
            let source_file = client_key
                .clone()
                .unwrap_or_else(|| format!("mcp://{actor}"));
            let (row, duplicate) =
                ptask_core::raw_items::insert_idempotent(&db, &text, &source, &source_file)
                    .map_err(domain_err)?;
            let mut out = serde_json::json!({"id": row.id, "duplicate": duplicate});
            if !duplicate && severity.is_some_and(|s| s >= 3) {
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
                    &db,
                    new,
                    ptask_core::Extensions::default(),
                    &ctx,
                )
                .map_err(domain_err)?;
                ptask_core::raw_items::mark_processed(&db, row.id).map_err(domain_err)?;
                rescore_db(&db);
                out["task_uuid"] = serde_json::json!(t.id);
                out["pt_id"] = serde_json::json!(t.pt_id);
            }
            json_ok(&out)
        })
        .await
    }

    #[tool(
        description = "Full-text search (FTS5) over task titles + descriptions, any status. Free text: every word must match; punctuation and AND/OR/NOT are literal; a trailing * matches a prefix."
    )]
    async fn task_search(
        &self,
        Parameters(SearchArg { query, limit }): Parameters<SearchArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            if query.trim().is_empty() {
                return Err(McpError::invalid_params("query must be non-empty", None));
            }
            let Some(q) = ptask_core::tasks::fts_match_query(&query) else {
                return json_ok(&Vec::<serde_json::Value>::new());
            };
            let limit = limit.unwrap_or(20).clamp(1, 100) as i64;
            let rows: Vec<serde_json::Value> = db
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
        })
        .await
    }

    #[tool(
        description = "Session-priming digest: counts + recently done/dismissed/created over a lookback window, plus the current top of the ready queue. Call at session start to load task context."
    )]
    async fn task_digest(
        &self,
        Parameters(DigestArg { days }): Parameters<DigestArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let v = ptask_core::digest::build(&db, days.unwrap_or(7)).map_err(domain_err)?;
            json_ok(&v)
        })
        .await
    }

    #[tool(
        description = "Request operator approval for an exact payload. Agents request; only the operator decides. Supply exactly one of payload (UTF-8 file), payload_json, or digest."
    )]
    async fn approval_request(
        &self,
        Parameters(arg): Parameters<ApprovalRequestArg>,
    ) -> Result<CallToolResult, McpError> {
        use ptask_core::approvals::{self, RequestInput};
        let payload = approvals::payload_from_wire(
            arg.payload,
            arg.payload_name,
            arg.payload_json.as_ref(),
            arg.digest.as_deref(),
        )
        .map_err(domain_err)?;
        let input = RequestInput {
            kind: arg.kind,
            title: arg.title,
            request_note: arg.note,
            payload,
            task_pt_id: arg.task,
            expires_in: arg.expires_in,
        };
        let db = self.db.clone();
        let ctx = self.ctx();
        let notify = self.notify.clone();
        let outcome =
            on_blocking(move || approvals::request(&db, input, &ctx).map_err(domain_err)).await?;
        if outcome.created {
            let _ = ptask_notify::notify_approval(
                &self.db,
                &notify.cfg,
                notify.dash_url.as_deref(),
                notify.tg_buttons,
                &outcome.approval,
            )
            .await;
        }
        let db = self.db.clone();
        let uuid = outcome.approval.uuid.clone();
        let mut outcome = outcome;
        if let Ok(ap) = on_blocking(move || approvals::get(&db, &uuid).map_err(domain_err)).await {
            outcome.approval = ap;
        }
        json_ok(&outcome.to_json())
    }

    #[tool(description = "List approvals. Default status is pending, oldest first.")]
    async fn approval_list(
        &self,
        Parameters(ApprovalListArg { status }): Parameters<ApprovalListArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let items = ptask_core::approvals::list(&db, status.as_deref()).map_err(domain_err)?;
            json_ok(&items.iter().map(|a| a.to_json(None)).collect::<Vec<_>>())
        })
        .await
    }

    #[tool(description = "Show one approval (AP-n) including payload preview.")]
    async fn approval_status(
        &self,
        Parameters(ApprovalIdArg { id }): Parameters<ApprovalIdArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let ap = ptask_core::approvals::get(&db, &id).map_err(domain_err)?;
            let events = ptask_core::approvals::events(&db, &ap.uuid).map_err(domain_err)?;
            json_ok(&ap.to_json(Some(&events)))
        })
        .await
    }

    #[tool(description = "Withdraw a pending approval you requested. Does not decide.")]
    async fn approval_withdraw(
        &self,
        Parameters(ApprovalIdArg { id }): Parameters<ApprovalIdArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let ap = ptask_core::approvals::withdraw(&db, &id, &ctx).map_err(domain_err)?;
            json_ok(&ap.to_json(None))
        })
        .await
    }

    #[tool(
        description = "List goals in tree order (parent before children, siblings by seq). Default: active only; all=true includes achieved and abandoned."
    )]
    async fn goal_list(
        &self,
        Parameters(GoalListArg { all }): Parameters<GoalListArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let items = ptask_core::goals::list(&db, all).map_err(domain_err)?;
            json_ok(
                &items
                    .iter()
                    .map(ptask_core::goals::GoalListItem::to_json)
                    .collect::<Vec<_>>(),
            )
        })
        .await
    }

    #[tool(
        description = "Show one goal (G-n): ancestors, children, tasks whose effective goal is this one, and open/done rollup over the subtree."
    )]
    async fn goal_show(
        &self,
        Parameters(GoalIdArg { id }): Parameters<GoalIdArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        on_blocking(move || {
            let shown = ptask_core::goals::show(&db, &id).map_err(domain_err)?;
            json_ok(&shown.to_json())
        })
        .await
    }

    #[tool(
        description = "Link a task to a goal (direct). Subtasks inherit via parent_uuid when they have no direct link. Arguments: task (PT-n), goal (G-n)."
    )]
    async fn goal_link(
        &self,
        Parameters(GoalLinkArg { task, goal }): Parameters<GoalLinkArg>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let ctx = self.ctx();
        on_blocking(move || {
            let t = ptask_core::goals::link(&db, &task, &goal, &ctx).map_err(domain_err)?;
            let mut v = task_json(&t);
            v["ok"] = serde_json::json!(true);
            v["goal"] = serde_json::json!(goal);
            json_ok(&with_goals(&db, &t, v)?)
        })
        .await
    }
}

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
                 (severity>=3) fast-lanes incidents into tasks. \
                 task_note appends findings and closure evidence; \
                 task_add skip_if_duplicate / task_duplicates / task_merge fold lookalikes; \
                 task_add(acceptance) / task_criteria gate the close until every criterion is checked."
                    .to_string(),
            )
    }
}

/// Serve the MCP handler over stdio — `pt mcp`. Blocks until the client
/// disconnects.
pub async fn serve_stdio(db: Db, actor: String, notify: McpNotify) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let service = PtaskMcp::new(db, actor)
        .with_notify(notify)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_initialize_advertises_tools_on_a_supported_protocol() {
        // rmcp 3.3.0 (2026-09-10) extracted protocol-version negotiation out of
        // the default initialize body into ServerHandler::negotiate_initialize
        // so a server that overrides initialize does not have to restate the
        // rule (modelcontextprotocol/rust-sdk#1247). pTask uses the default
        // initialize; this call is the new path that handshake now takes.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let mcp = PtaskMcp::new(db, "test-agent".into());
        let request = InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::from_build_env(),
        );
        let result = mcp
            .negotiate_initialize(&request)
            .expect("negotiate_initialize");
        assert!(
            result.capabilities.tools.is_some(),
            "pTask is a tools server; initialize must advertise tools"
        );
        let supported = mcp.supported_protocol_versions();
        assert!(
            supported.contains(&result.protocol_version),
            "negotiated {:?} is not in supported {:?}",
            result.protocol_version,
            supported
        );
    }

    #[tokio::test]
    async fn rejected_edit_leaves_task_and_journal_unchanged() {
        for invalid in ["priority", "deadline", "labels", "recurring", "database"] {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::open(dir.path().join("edit.db")).unwrap();
            let mut new = ptask_core::NewTask::minimal("original title");
            new.deadline = Some("2099-01-01".into());
            let mut ext = ptask_core::Extensions::default();
            if invalid == "recurring" {
                ext.recurrence = Some(ptask_core::recurrence::parse("every day").unwrap());
            }
            let task = ptask_core::tasks::create_with_extensions(&db, new, ext, &EventCtx::test())
                .unwrap();
            let cursor = ptask_core::event_log::current_cursor(&db).unwrap();
            if invalid == "database" {
                db.with_conn(|c| {
                    c.execute_batch("CREATE TRIGGER reject_label BEFORE INSERT ON task_labels BEGIN SELECT RAISE(ABORT, 'test label failure'); END;")?;
                    Ok(())
                }).unwrap();
            }
            let mcp = PtaskMcp::new(db.clone(), "test-agent".into());
            let result = mcp
                .task_edit(Parameters(EditArg {
                    id: task.id.clone(),
                    title: Some("changed title".into()),
                    description: None,
                    priority: Some(if invalid == "priority" { 6 } else { 4 }),
                    deadline: match invalid {
                        "deadline" => Some("not-a-date".into()),
                        "recurring" => Some(String::new()),
                        _ => None,
                    },
                    labels_add: vec![
                        if invalid == "labels" {
                            " "
                        } else {
                            "new-label"
                        }
                        .into(),
                    ],
                    labels_remove: vec![],
                }))
                .await;
            assert!(result.is_err(), "{invalid}");
            let after = ptask_core::tasks::resolve_for_lookup(&db, &task.id, true).unwrap();
            assert_eq!(after.title, task.title, "{invalid}");
            assert_eq!(after.priority, task.priority, "{invalid}");
            assert_eq!(after.deadline, task.deadline, "{invalid}");
            assert!(
                ptask_core::tasks::load_detail(&db, &task.id)
                    .unwrap()
                    .labels
                    .is_empty()
            );
            assert_eq!(
                ptask_core::event_log::current_cursor(&db).unwrap(),
                cursor,
                "{invalid}"
            );
        }
    }

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
                kind: None,
                deliverable: None,
                acceptance: vec![],
                skip_if_duplicate: false,
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
    async fn task_search_takes_free_text_and_finds_the_rows() {
        // Regression (CORE-6): agents search before task_add to avoid
        // duplicates; a raw MATCH failed on "follow-up", "don't", "PT-2201",
        // "c++", "what?", "100%", bare NOT/AND and "*", so they added again.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        for title in [
            "schedule the follow-up call",
            "don't page on disk alerts",
            "PT-2201 tailnet gate",
            "learn c++ templates",
            "what? why is the queue stuck",
            "rollout at 100% traffic",
            "NOT a drill: rotate keys",
        ] {
            ptask_core::tasks::create(&db, ptask_core::NewTask::minimal(title), &EventCtx::test())
                .unwrap();
        }
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());
        for (query, expected) in [
            ("follow-up", Some("schedule the follow-up call")),
            ("don't", Some("don't page on disk alerts")),
            ("\"don't\"", Some("don't page on disk alerts")),
            ("PT-2201", Some("PT-2201 tailnet gate")),
            ("c++", Some("learn c++ templates")),
            ("what?", Some("what? why is the queue stuck")),
            ("100%", Some("rollout at 100% traffic")),
            ("NOT drill", Some("NOT a drill: rotate keys")),
            ("temp*", Some("learn c++ templates")),
            ("AND", None),
            ("*", None),
        ] {
            let result = mcp
                .task_search(Parameters(SearchArg {
                    query: query.into(),
                    limit: None,
                }))
                .await
                .unwrap_or_else(|e| panic!("{query:?}: {e:?}"));
            let payload = serde_json::to_value(&result).unwrap();
            let text = payload
                .pointer("/content/0/text")
                .unwrap()
                .as_str()
                .unwrap();
            let rows: Vec<serde_json::Value> = serde_json::from_str(text).unwrap();
            let titles: Vec<&str> = rows.iter().map(|r| r["title"].as_str().unwrap()).collect();
            match expected {
                Some(title) => assert!(titles.contains(&title), "{query:?}: {titles:?}"),
                None => assert!(titles.is_empty(), "{query:?}: {titles:?}"),
            }
        }
    }

    #[tokio::test]
    async fn task_done_with_expected_deadline_advances_one_occurrence_once() {
        // Round 2, item 4ii: task_done resolves the task when it runs, so a
        // duplicate call for the occurrence an agent saw advanced it again.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let mut new = ptask_core::NewTask::minimal("daily");
        new.deadline = Some("2099-01-01".into());
        let t = ptask_core::tasks::create_with_extensions(
            &db,
            new,
            ptask_core::Extensions {
                recurrence: Some(ptask_core::recurrence::parse("every day").unwrap()),
                ..Default::default()
            },
            &EventCtx::test(),
        )
        .unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());
        let done = || {
            mcp.task_done(Parameters(DoneArg {
                id: t.pt_id.clone().unwrap(),
                expected_deadline: Some("2099-01-01".into()),
                note: None,
            }))
        };
        done().await.unwrap();
        assert!(done().await.is_err());
        let after = ptask_core::tasks::resolve_for_lookup(&db, &t.id, true).unwrap();
        assert_eq!(after.deadline.as_deref(), Some("2099-01-02"));
    }

    #[tokio::test]
    async fn task_done_twice_journals_one_completion() {
        // Regression (MCP-13): task_done resolves PT-N/uuid across terminal
        // states, so a repeated call re-completed the task and journaled a
        // second task.completed.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let t = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("finish once"),
            &EventCtx::test(),
        )
        .unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());
        let done = |id: String| {
            mcp.task_done(Parameters(DoneArg {
                id,
                expected_deadline: None,
                note: None,
            }))
        };
        done(t.pt_id.clone().unwrap()).await.unwrap();
        let cursor = ptask_core::event_log::current_cursor(&db).unwrap();
        assert!(done(t.pt_id.clone().unwrap()).await.is_err());
        assert!(done(t.id.clone()).await.is_err());
        assert_eq!(ptask_core::event_log::current_cursor(&db).unwrap(), cursor);
        let completed: i64 = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM pt_event_log WHERE event_type='task.completed'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(completed, 1);
    }

    #[tokio::test]
    async fn task_done_reports_advanced_for_a_recurring_task() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let rec = ptask_core::recurrence::parse("every monday").unwrap();
        let now = ptask_core::dates::now_in_operator_tz().unwrap();
        let mut deadline = now.clone();
        while deadline.weekday() != ptask_core::jiff::civil::Weekday::Monday {
            deadline = deadline
                .checked_add(ptask_core::jiff::Span::new().days(1))
                .unwrap();
        }
        let mut new = ptask_core::NewTask::minimal("standup");
        new.deadline = Some(ptask_core::dates::format_iso(&deadline));
        let t = ptask_core::tasks::create_with_extensions(
            &db,
            new,
            ptask_core::Extensions {
                recurrence: Some(rec),
                ..Default::default()
            },
            &EventCtx::test(),
        )
        .unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());
        let result = mcp
            .task_done(Parameters(DoneArg {
                id: t.pt_id.clone().unwrap(),
                expected_deadline: None,
                note: None,
            }))
            .await
            .unwrap();
        let payload = serde_json::to_value(&result).unwrap();
        let text = payload
            .pointer("/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("unexpected CallToolResult shape: {payload}"));
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["status"], "advanced", "got {body}");
        assert!(body["next_deadline"].as_str().unwrap().len() > 8);
        db.with_conn(|c| {
            let status: String = c
                .query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status, "pending", "recurring task must stay open");
            Ok(())
        })
        .unwrap();
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
                kind: None,
                deliverable: None,
                acceptance: vec![],
                skip_if_duplicate: false,
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

    #[tokio::test]
    async fn task_add_provenance_link_is_atomic_with_the_create() {
        // MCP-14: the discovered_from link was inserted after the create
        // committed and without an event. If it failed, the tool errored for
        // a task that already existed and an agent retry created it again.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let parent = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("parent task"),
            &EventCtx::test(),
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER no_links BEFORE INSERT ON task_links
                 BEGIN SELECT RAISE(ABORT, 'link write failed'); END;",
            )?;
            Ok(())
        })
        .unwrap();
        let cursor = ptask_core::event_log::current_cursor(&db).unwrap();
        let mcp = PtaskMcp::new(db.clone(), "test-agent".into());
        let add = || {
            mcp.task_add(Parameters(AddArg {
                text: "discovered child".into(),
                reason: None,
                discovered_from: parent.pt_id.clone(),
                kind: None,
                deliverable: None,
                acceptance: vec![],
                skip_if_duplicate: false,
            }))
        };

        assert!(add().await.is_err());
        db.with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM tasks WHERE title = 'discovered child'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 0, "a failed link must not leave the task behind");
            Ok(())
        })
        .unwrap();
        assert_eq!(ptask_core::event_log::current_cursor(&db).unwrap(), cursor);

        // Once the link can be written, the created event carries it.
        db.with_conn(|c| Ok(c.execute_batch("DROP TRIGGER no_links")?))
            .unwrap();
        assert!(add().await.is_ok());
        db.with_conn(|c| {
            let payload: String = c.query_row(
                "SELECT payload FROM pt_event_log
                 WHERE event_type = 'task.created' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )?;
            let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(v["discovered_from"], serde_json::json!(parent.id));
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

    #[tokio::test]
    async fn criteria_gate_task_done_over_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let mcp = PtaskMcp::new(db.clone(), "hal".into());
        let text = |r: CallToolResult| -> serde_json::Value {
            let v = serde_json::to_value(&r).unwrap();
            serde_json::from_str(v.pointer("/content/0/text").unwrap().as_str().unwrap()).unwrap()
        };
        let created = text(
            mcp.task_add(Parameters(AddArg {
                text: "Upgrade sglang on the fox pair".into(),
                reason: None,
                discovered_from: None,
                kind: None,
                deliverable: None,
                acceptance: vec![
                    "TP4 smoke test passes".into(),
                    "256k context verified".into(),
                ],
                skip_if_duplicate: false,
            }))
            .await
            .unwrap(),
        );
        let pt = created["pt_id"].as_str().unwrap().to_string();
        let done = || {
            mcp.task_done(Parameters(DoneArg {
                id: pt.clone(),
                expected_deadline: None,
                note: None,
            }))
        };
        let err = done().await.unwrap_err();
        assert!(
            err.message.contains("unchecked acceptance criteria"),
            "{err:?}"
        );
        // A bad batch changes nothing (no half-applied add).
        assert!(
            mcp.task_criteria(Parameters(CriteriaArg {
                id: pt.clone(),
                add: vec!["extra".into()],
                check: vec![9],
                uncheck: vec![],
                evidence: None,
            }))
            .await
            .is_err()
        );
        let r = text(
            mcp.task_criteria(Parameters(CriteriaArg {
                id: pt.clone(),
                add: vec![],
                check: vec![1, 2],
                uncheck: vec![],
                evidence: Some("smoke ok; ctx 262144".into()),
            }))
            .await
            .unwrap(),
        );
        assert_eq!(r["unchecked"], 0);
        assert_eq!(
            r["criteria"].as_array().unwrap().len(),
            2,
            "the bad batch added nothing"
        );
        assert_eq!(r["criteria"][0]["checked_by"], "hal");
        let shown = text(
            mcp.task_show(Parameters(IdArg { id: pt.clone() }))
                .await
                .unwrap(),
        );
        assert_eq!(shown["criteria"][1]["evidence"], "smoke ok; ctx 262144");
        done().await.unwrap();
    }

    #[tokio::test]
    async fn notes_and_closure_evidence_round_trip_over_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let t = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("upgrade sglang"),
            &EventCtx::test(),
        )
        .unwrap();
        let other = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("upgrade sglang (dup)"),
            &EventCtx::test(),
        )
        .unwrap();
        let mcp = PtaskMcp::new(db.clone(), "hal".into());
        let pt = t.pt_id.clone().unwrap();
        mcp.task_note(Parameters(NoteArg {
            id: pt.clone(),
            text: "0.5.4 builds; TP4 smoke ok".into(),
        }))
        .await
        .unwrap();
        // A blank note is refused and the task stays open.
        assert!(
            mcp.task_done(Parameters(DoneArg {
                id: pt.clone(),
                expected_deadline: None,
                note: Some("  ".into()),
            }))
            .await
            .is_err()
        );
        mcp.task_done(Parameters(DoneArg {
            id: pt.clone(),
            expected_deadline: None,
            note: Some("deployed; 256k ctx verified".into()),
        }))
        .await
        .unwrap();
        mcp.task_dismiss(Parameters(DismissArg {
            id: other.pt_id.clone().unwrap(),
            note: Some(format!("duplicate of {pt}")),
        }))
        .await
        .unwrap();
        let notes = ptask_core::notes::list(&db, &t.id, 50).unwrap();
        let got: Vec<(&str, &str, Option<&str>)> = notes
            .iter()
            .map(|n| (n.kind.as_str(), n.text.as_str(), n.actor.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("note", "0.5.4 builds; TP4 smoke ok", Some("hal")),
                ("done", "deployed; 256k ctx verified", Some("hal")),
            ]
        );
        // task_show carries the trail.
        let shown = mcp
            .task_show(Parameters(IdArg { id: pt.clone() }))
            .await
            .unwrap();
        let payload = serde_json::to_value(&shown).unwrap();
        let text = payload
            .pointer("/content/0/text")
            .unwrap()
            .as_str()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["notes"].as_array().unwrap().len(), 2, "{v:#}");
        let dup = ptask_core::notes::list(&db, &other.id, 50).unwrap();
        assert_eq!(dup[0].kind, "dismissed");
        // Evidence after the close: a done task by PT-N.
        mcp.task_note(Parameters(NoteArg {
            id: pt,
            text: "24h later: no regressions".into(),
        }))
        .await
        .unwrap();
        assert_eq!(ptask_core::notes::list(&db, &t.id, 50).unwrap().len(), 3);
    }

    #[tokio::test]
    async fn claims_have_holders_leases_heartbeats_and_releases_over_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let t = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("rebuild the index"),
            &EventCtx::test(),
        )
        .unwrap();
        let pt = t.pt_id.clone().unwrap();
        let hal = PtaskMcp::new(db.clone(), "hal".into());
        let grok = PtaskMcp::new(db.clone(), "grok".into());
        let text = |r: CallToolResult| -> serde_json::Value {
            let v = serde_json::to_value(&r).unwrap();
            serde_json::from_str(v.pointer("/content/0/text").unwrap().as_str().unwrap()).unwrap()
        };
        let claimed = text(
            hal.task_claim(Parameters(ClaimArg {
                id: pt.clone(),
                lease_minutes: Some(15),
            }))
            .await
            .unwrap(),
        );
        assert_eq!(claimed["claimed_by"], "hal");
        assert!(claimed["claim_expires_at"].is_string());
        let token = claimed["claim_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .expect("task_claim returns claim_token")
            .to_string();
        let err = grok
            .task_claim(Parameters(ClaimArg {
                id: pt.clone(),
                lease_minutes: None,
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("already claimed by hal"), "{err:?}");
        let err = grok
            .task_heartbeat(Parameters(HeartbeatArg {
                id: pt.clone(),
                claim_token: Some(token.clone()),
                lease_minutes: None,
            }))
            .await
            .unwrap_err();
        assert!(err.message.starts_with("claim lost"), "{err:?}");
        hal.task_heartbeat(Parameters(HeartbeatArg {
            id: pt.clone(),
            claim_token: Some(token.clone()),
            lease_minutes: Some(60),
        }))
        .await
        .unwrap();
        // Another agent cannot release it over MCP (no force here).
        assert!(
            grok.task_release(Parameters(ReleaseArg {
                id: pt.clone(),
                claim_token: Some(token.clone()),
                reason: None,
            }))
            .await
            .is_err()
        );
        let shown = text(
            hal.task_show(Parameters(IdArg { id: pt.clone() }))
                .await
                .unwrap(),
        );
        assert_eq!(shown["claim"]["by"], "hal");
        hal.task_release(Parameters(ReleaseArg {
            id: pt.clone(),
            claim_token: Some(token),
            reason: Some("blocked on disk".into()),
        }))
        .await
        .unwrap();
        let shown = text(
            hal.task_show(Parameters(IdArg { id: pt.clone() }))
                .await
                .unwrap(),
        );
        assert!(shown["claim"].is_null());
        assert_eq!(shown["status"], "todo");
        grok.task_claim(Parameters(ClaimArg {
            id: pt,
            lease_minutes: None,
        }))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn task_add_reports_and_can_skip_duplicates_and_task_merge_folds_them() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("mcp.db")).unwrap();
        let first = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("Reindex the search index on lab-1"),
            &EventCtx::test(),
        )
        .unwrap();
        let mcp = PtaskMcp::new(db.clone(), "hal".into());
        let text = |r: CallToolResult| -> serde_json::Value {
            let v = serde_json::to_value(&r).unwrap();
            serde_json::from_str(v.pointer("/content/0/text").unwrap().as_str().unwrap()).unwrap()
        };
        let add = |t: &str, skip: bool| {
            mcp.task_add(Parameters(AddArg {
                text: t.into(),
                reason: None,
                discovered_from: None,
                kind: None,
                deliverable: None,
                acceptance: vec![],
                skip_if_duplicate: skip,
            }))
        };
        let skipped = text(add("search index reindex on lab-1", true).await.unwrap());
        assert_eq!(skipped["created"], false);
        assert_eq!(
            skipped["possible_duplicates"][0]["pt_id"],
            first.pt_id.clone().unwrap()
        );
        let count = |db: &Db| -> i64 {
            db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?))
                .unwrap()
        };
        assert_eq!(count(&db), 1, "skip_if_duplicate created nothing");

        let created = text(add("search index reindex on lab-1", false).await.unwrap());
        let new_pt = created["pt_id"].as_str().unwrap().to_string();
        assert_eq!(created["possible_duplicates"].as_array().unwrap().len(), 1);
        let listed = text(
            mcp.task_duplicates(Parameters(IdArg { id: new_pt.clone() }))
                .await
                .unwrap(),
        );
        assert_eq!(
            listed["possible_duplicates"][0]["pt_id"],
            first.pt_id.clone().unwrap()
        );

        let merged = text(
            mcp.task_merge(Parameters(MergeArg {
                duplicate: new_pt.clone(),
                into: first.pt_id.clone().unwrap(),
                reason: Some("filed twice".into()),
            }))
            .await
            .unwrap(),
        );
        assert_eq!(merged["ok"], true);
        let shown = text(
            mcp.task_show(Parameters(IdArg { id: new_pt }))
                .await
                .unwrap(),
        );
        assert_eq!(shown["status"], "dismissed");
        assert_eq!(shown["duplicate_of"], first.pt_id.clone().unwrap());
        // A plain unrelated add has no possible_duplicates key.
        let other = text(add("Renew the office lease", false).await.unwrap());
        assert!(other.get("possible_duplicates").is_none());
    }
}
