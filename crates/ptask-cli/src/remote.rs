//! Remote `pt` client — speaks the v0.4.2 `/sync` wire protocol against
//! the canonical-host `pt serve` (tensor-core post-v1.0.3 activation).
//!
//! Lets every fleet node `pt remote add "..."`, `pt remote list`,
//! `pt remote done PT-42` without owning its own copy of `tasks.db`.
//! Fleet shell profile (`/etc/profile.d/ptask.sh`) sets `PTASK_SYNC_URL`;
//! callers that don't go through the env fall through to the hard-coded
//! Tailscale-IP default in `default_url`.

use anyhow::{Context, Result, anyhow};
use ptask_core::Task;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

/// Endpoint URL — `${PTASK_SYNC_URL}` if set, else the canonical-host
/// fallback. Tests inject via `RemoteClient::with_url`.
///
/// The fallback is tensor-core's Tailscale IP + `:9501` (live as of
/// v1.0.3 activation). Operator profile (`/etc/profile.d/ptask.sh`) sets
/// `PTASK_SYNC_URL` fleet-wide; the env var is the authoritative source
/// of truth and this constant is only consulted when it's missing.
pub fn default_url() -> String {
    std::env::var("PTASK_SYNC_URL").unwrap_or_else(|_| "http://100.121.42.54:9501".to_string())
}

#[derive(Debug, Clone)]
pub struct RemoteClient {
    base: String,
    api_token: Option<String>,
    client: reqwest::blocking::Client,
}

impl RemoteClient {
    pub fn from_env() -> Result<Self> {
        Self::with_url(&default_url())
    }

    pub fn with_url(base: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("build remote client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            api_token: std::env::var("PTASK_API_TOKEN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            client,
        })
    }

    /// Fetch the server's advertised version from the open `GET /version`
    /// route. `None` when the server is unreachable or predates the route.
    pub fn server_version(&self) -> Option<String> {
        let url = format!("{}/version", self.base);
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().ok()?;
        v.get("ptask_core")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    }

    /// Loud client/server skew diagnosis appended to remote errors, so a
    /// 401/404 from a mismatched deploy names its real cause instead of
    /// reading like an auth or routing problem (the fleet ran 1.0.2 clients
    /// against a token-enforced 1.10.1 server for weeks undiagnosed).
    fn skew_hint(&self) -> String {
        let client = ptask_core::VERSION;
        match self.server_version() {
            Some(server) if server != client => format!(
                "\nversion skew: client v{client} vs server v{server} — \
                 redeploy pt so both sides match (scripts/ansible/ptask.yml)"
            ),
            _ => String::new(),
        }
    }

    fn sync(&self, req: &Value) -> Result<SyncResp> {
        let url = format!("{}/sync", self.base);
        let mut builder = self.client.post(&url).json(req);
        if let Some(token) = self.api_token.as_ref() {
            builder = builder.bearer_auth(token);
        }
        let resp = builder.send().with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("remote /sync {status}: {body}{}", self.skew_hint()));
        }
        resp.json::<SyncResp>().context("parse /sync response")
    }

    /// GET a read-only endpoint and return the parsed JSON. Forwards the API
    /// token as a bearer credential, matching the `/sync` path.
    fn get_json(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut builder = self.client.get(&url);
        if let Some(token) = self.api_token.as_ref() {
            builder = builder.bearer_auth(token);
        }
        let resp = builder.send().with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!(
                "remote GET {path} {status}: {body}{}",
                self.skew_hint()
            ));
        }
        resp.json::<Value>()
            .with_context(|| format!("parse GET {path}"))
    }

    /// GET a read-only endpoint with URL-encoded query parameters.
    fn get_json_with_params(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let mut url =
            reqwest::Url::parse(&format!("{}{}", self.base, path)).context("build remote URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in params {
                pairs.append_pair(k, v);
            }
        }
        let mut builder = self.client.get(url.clone());
        if let Some(token) = self.api_token.as_ref() {
            builder = builder.bearer_auth(token);
        }
        let resp = builder.send().with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!(
                "remote GET {path} {status}: {body}{}",
                self.skew_hint()
            ));
        }
        resp.json::<Value>()
            .with_context(|| format!("parse GET {path}"))
    }

    /// `pt remote add "..."` — POST `task_create` with the quick-add text.
    /// Returns the created `Task` row.
    pub fn add(&self, text: &str) -> Result<Task> {
        let cmd_uuid = uuid::Uuid::new_v4().to_string();
        let temp_id = format!("tmp-{cmd_uuid}");
        let req = json!({
            "sync_token": "*",
            "resource_types": ["tasks"],
            "commands": [{
                "type": "task_create",
                "uuid": cmd_uuid,
                "temp_id": temp_id,
                "args": { "text": text, "source_type": "remote-cli" }
            }]
        });
        let resp = self.sync(&req)?;
        ensure_ok(&resp.sync_status, &cmd_uuid)?;
        let task_uuid = resp
            .temp_id_mapping
            .get(&temp_id)
            .ok_or_else(|| anyhow!("remote: missing temp_id_mapping for created task"))?;
        resp.resources
            .tasks
            .into_iter()
            .find(|t| t.id == *task_uuid)
            .ok_or_else(|| anyhow!("remote: created task {task_uuid} missing from /sync resources"))
    }

    /// `pt remote done <query>` — accepts PT-N, bare integer, or title
    /// substring. Resolves the task on the server, then dispatches
    /// `task_done` by uuid.
    pub fn done(&self, query: &str) -> Result<Task> {
        let task = self.resolve(query, false)?;
        let cmd_uuid = uuid::Uuid::new_v4().to_string();
        let req = json!({
            "sync_token": "*",
            "resource_types": ["tasks"],
            "commands": [{
                "type": "task_done",
                "uuid": cmd_uuid,
                "args": { "task_uuid": task.id }
            }]
        });
        let resp = self.sync(&req)?;
        ensure_ok(&resp.sync_status, &cmd_uuid)?;
        Ok(task)
    }

    /// `pt remote priority <query> <level>` — set priority (1..=5) on the
    /// canonical host. Resolves server-side, then dispatches `task_priority`.
    pub fn priority(&self, query: &str, level: i64) -> Result<Task> {
        let mut task = self.resolve(query, false)?;
        let cmd_uuid = uuid::Uuid::new_v4().to_string();
        let req = json!({
            "sync_token": "*",
            "resource_types": ["tasks"],
            "commands": [{
                "type": "task_priority",
                "uuid": cmd_uuid,
                "args": { "task_uuid": task.id, "priority": level }
            }]
        });
        let resp = self.sync(&req)?;
        ensure_ok(&resp.sync_status, &cmd_uuid)?;
        task.priority = level;
        Ok(task)
    }

    /// `pt remote edit <query> [--title T] [--desc D] [--deadline ISO | --clear-deadline]`.
    /// Resolves the query to a task ONCE, then dispatches the title/description
    /// (`task_retext`) and/or deadline (`task_edit`) changes as separate commands
    /// in a SINGLE `/sync` request — both keyed to the resolved `task_uuid`. This
    /// is the fix for the wrong-task hazard of resolving the query twice: a title
    /// rename can no longer make a second resolve drift onto a different task.
    ///
    /// `deadline`: `None` = leave it; `Some(None)` = clear (JSON null);
    /// `Some(Some(s))` = set to `s`.
    pub fn edit(
        &self,
        query: &str,
        title: Option<&str>,
        description: Option<&str>,
        deadline: Option<Option<&str>>,
    ) -> Result<Task> {
        let mut task = self.resolve(query, false)?;
        let retext_uuid = uuid::Uuid::new_v4().to_string();
        let edit_uuid = uuid::Uuid::new_v4().to_string();
        let mut commands: Vec<Value> = Vec::new();
        if title.is_some() || description.is_some() {
            commands.push(json!({
                "type": "task_retext",
                "uuid": retext_uuid,
                "args": { "task_uuid": task.id, "title": title, "description": description }
            }));
        }
        if let Some(dl) = deadline {
            commands.push(json!({
                "type": "task_edit",
                "uuid": edit_uuid,
                "args": { "task_uuid": task.id, "deadline": dl }
            }));
        }
        if commands.is_empty() {
            return Err(anyhow!("remote edit: nothing to change"));
        }
        let req = json!({ "sync_token": "*", "resource_types": ["tasks"], "commands": commands });
        let resp = self.sync(&req)?;
        if title.is_some() || description.is_some() {
            ensure_ok(&resp.sync_status, &retext_uuid)?;
        }
        if deadline.is_some() {
            ensure_ok(&resp.sync_status, &edit_uuid)?;
        }
        if let Some(t) = title {
            task.title = t.to_string();
        }
        if let Some(d) = description {
            task.description = d.to_string();
        }
        if let Some(dl) = deadline {
            task.deadline = dl.map(str::to_string);
        }
        Ok(task)
    }

    /// `pt remote reopen <query>` — flip a done/dismissed task back to pending.
    /// Resolves including terminal-state tasks (the point is to find a completed
    /// one); resolve-by-substring therefore matches done tasks here too.
    pub fn reopen(&self, query: &str) -> Result<Task> {
        let mut task = self.resolve(query, true)?;
        let cmd_uuid = uuid::Uuid::new_v4().to_string();
        let req = json!({
            "sync_token": "*",
            "resource_types": ["tasks"],
            "commands": [{
                "type": "task_reopen",
                "uuid": cmd_uuid,
                "args": { "task_uuid": task.id }
            }]
        });
        let resp = self.sync(&req)?;
        ensure_ok(&resp.sync_status, &cmd_uuid)?;
        task.status = "pending".to_string();
        Ok(task)
    }

    /// `pt remote show <query>` — fetch one task's full row (read-only). No
    /// mutation; resolves including terminal-state tasks so completed items are
    /// viewable by PT-N.
    pub fn show(&self, query: &str) -> Result<Task> {
        self.resolve(query, true)
    }

    /// `pt remote dismiss <query>` — soft-close a task (status → dismissed).
    /// Reversible via `reopen`. Resolves active tasks only.
    pub fn dismiss(&self, query: &str) -> Result<Task> {
        let mut task = self.resolve(query, false)?;
        let cmd_uuid = uuid::Uuid::new_v4().to_string();
        let req = json!({
            "sync_token": "*",
            "resource_types": ["tasks"],
            "commands": [{
                "type": "task_dismiss",
                "uuid": cmd_uuid,
                "args": { "task_uuid": task.id }
            }]
        });
        let resp = self.sync(&req)?;
        ensure_ok(&resp.sync_status, &cmd_uuid)?;
        task.status = "dismissed".to_string();
        Ok(task)
    }

    /// `pt remote start <query>` — mark in progress on the canonical host.
    pub fn start(&self, query: &str) -> Result<Task> {
        self.simple_task_command("task_start", query, serde_json::Map::new(), false)
    }

    /// `pt remote snooze <query> <until-iso>`.
    pub fn snooze(&self, query: &str, until_iso: &str) -> Result<Task> {
        let mut extra = serde_json::Map::new();
        extra.insert("until".into(), json!(until_iso));
        self.simple_task_command("task_snooze", query, extra, false)
    }

    /// `pt remote depend <query> --on <target> [--clear]`. The target is
    /// resolved server-side by the command handler.
    pub fn depend(&self, query: &str, on: &str, clear: bool) -> Result<Task> {
        let mut extra = serde_json::Map::new();
        extra.insert("on".into(), json!(on));
        if clear {
            extra.insert("clear".into(), json!(true));
        }
        self.simple_task_command("task_depend", query, extra, false)
    }

    /// `pt remote rm <query>` — permanent delete (tombstoned for delta
    /// sync). Resolves terminal tasks too.
    pub fn rm(&self, query: &str) -> Result<Task> {
        self.simple_task_command("task_delete", query, serde_json::Map::new(), true)
    }

    /// `GET /list?filter=` — server-side filtered list (replaces the old
    /// fetch-everything-and-filter-locally pattern when a DSL is given).
    pub fn list_filtered(
        &self,
        filter: Option<&str>,
        status: &str,
        limit: usize,
    ) -> Result<Vec<Task>> {
        let mut params: Vec<(&str, String)> =
            vec![("status", status.to_string()), ("limit", limit.to_string())];
        if let Some(f) = filter {
            params.push(("filter", f.to_string()));
        }
        let v = self.get_json_with_params("/list", &params)?;
        let tasks = v
            .get("tasks")
            .cloned()
            .ok_or_else(|| anyhow!("GET /list: missing tasks field"))?;
        serde_json::from_value(tasks).context("parse /list tasks")
    }

    /// Shared shape for resolve-then-single-command verbs.
    fn simple_task_command(
        &self,
        command: &str,
        query: &str,
        extra: serde_json::Map<String, Value>,
        include_terminal: bool,
    ) -> Result<Task> {
        let task = self.resolve(query, include_terminal)?;
        let cmd_uuid = uuid::Uuid::new_v4().to_string();
        let mut args = serde_json::Map::new();
        args.insert("task_uuid".into(), json!(task.id));
        args.extend(extra);
        let req = json!({
            "sync_token": "*",
            "resource_types": ["tasks"],
            "commands": [{
                "type": command,
                "uuid": cmd_uuid,
                "args": Value::Object(args),
            }]
        });
        let resp = self.sync(&req)?;
        ensure_ok(&resp.sync_status, &cmd_uuid)?;
        Ok(task)
    }

    /// `pt remote next` — DAG-ready tasks computed on the canonical host. The
    /// `/sync` Task shape can't carry `depends_on` edges, so readiness must be
    /// resolved server-side.
    pub fn next(&self, limit: usize) -> Result<Vec<Task>> {
        let v = self.get_json(&format!("/next?limit={limit}"))?;
        serde_json::from_value(v.get("tasks").cloned().unwrap_or(Value::Null))
            .context("parse /next tasks")
    }

    /// Fetch a task's side-table detail (labels/project/deps/recurrence) by
    /// UUID. Backs the rich `pt remote show` output.
    pub fn detail(&self, task_uuid: &str) -> Result<ptask_core::tasks::TaskDetail> {
        let v = self.get_json(&format!("/detail/{task_uuid}"))?;
        serde_json::from_value(v).context("parse /detail")
    }

    /// `pt remote list` — full sync, then client-side filters (status,
    /// priority, limit). Matches the local `pt list` UX so the operator
    /// has parity from any node.
    pub fn list(
        &self,
        status: Option<&str>,
        priority: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Task>> {
        let req = json!({ "sync_token": "*", "resource_types": ["tasks"] });
        let resp = self.sync(&req)?;
        let mut out: Vec<Task> = resp
            .resources
            .tasks
            .into_iter()
            .filter(|t| match status {
                None | Some("all") => true,
                Some(s) => t.status == s,
            })
            .filter(|t| priority.is_none_or(|p| t.priority == p))
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(limit);
        Ok(out)
    }

    /// Resolve a PT-N / bare-integer / title-substring query to a single task
    /// via server-side `/resolve`. `include_terminal` keeps done/dismissed
    /// tasks in the substring search (PT-N / integer always match any status);
    /// reopen/show pass `true`, mutating verbs that only make sense on active
    /// tasks pass `false`.
    fn resolve(&self, query: &str, include_terminal: bool) -> Result<Task> {
        let v = self.get_json_with_params(
            "/resolve",
            &[
                ("query", query.to_string()),
                ("include_terminal", include_terminal.to_string()),
            ],
        )?;
        serde_json::from_value(v.get("task").cloned().unwrap_or(Value::Null))
            .context("parse /resolve task")
    }
}

#[derive(Debug, Deserialize)]
struct SyncResp {
    #[allow(dead_code)]
    sync_token: String,
    resources: SyncResources,
    sync_status: BTreeMap<String, Value>,
    #[serde(default)]
    #[allow(dead_code)]
    temp_id_mapping: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct SyncResources {
    tasks: Vec<Task>,
}

fn ensure_ok(status: &BTreeMap<String, Value>, cmd_uuid: &str) -> Result<()> {
    match status.get(cmd_uuid) {
        Some(Value::String(s)) if s == "ok" => Ok(()),
        Some(other) => Err(anyhow!("remote: command {cmd_uuid} failed — {other}")),
        None => Err(anyhow!("remote: command {cmd_uuid} missing from response")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    fn existing_tasks_json() -> Vec<Value> {
        vec![
            json!({
                "id": "uuid-aaaaaaaa", "pt_id": "PT-100",
                "title": "buy bread tomorrow morning",
                "description": "", "priority": 2, "status": "pending",
                "created_at": "2026-05-13T00:00:00+00:00",
                "updated_at": "2026-05-13T00:00:00+00:00",
                "deadline": null, "source_type": "manual", "ai_reasoning": ""
            }),
            json!({
                "id": "uuid-bbbbbbbb", "pt_id": "PT-101",
                "title": "investigate ceph mon quorum failure",
                "description": "", "priority": 4, "status": "pending",
                "created_at": "2026-05-13T01:00:00+00:00",
                "updated_at": "2026-05-13T01:00:00+00:00",
                "deadline": null, "source_type": "manual", "ai_reasoning": ""
            }),
            json!({
                "id": "uuid-cccccccc", "pt_id": "PT-102",
                "title": "archive completed receipt",
                "description": "", "priority": 2, "status": "done",
                "created_at": "2026-05-13T02:00:00+00:00",
                "updated_at": "2026-05-13T02:00:00+00:00",
                "deadline": null, "source_type": "manual", "ai_reasoning": ""
            }),
            json!({
                "id": "uuid-newer-existing", "pt_id": "PT-999",
                "title": "newer existing task should not be returned by add",
                "description": "", "priority": 5, "status": "pending",
                "created_at": "2026-05-15T00:00:00+00:00",
                "updated_at": "2026-05-15T00:00:00+00:00",
                "deadline": null, "source_type": "manual", "ai_reasoning": ""
            }),
        ]
    }

    fn resolve_mock_response(params: BTreeMap<String, String>) -> axum::response::Response {
        use axum::response::IntoResponse;

        let query = params.get("query").map_or("", String::as_str).trim();
        if query.is_empty() {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "empty task query"})),
            )
                .into_response();
        }
        let include_terminal = params.get("include_terminal").is_some_and(|v| v == "true");
        let tasks = existing_tasks_json();
        let upper = query.to_ascii_uppercase();
        let pt_candidate = if upper.starts_with("PT-") {
            Some(upper)
        } else if let Ok(n) = query.parse::<u64>() {
            Some(format!("PT-{n}"))
        } else {
            None
        };
        if let Some(pt_id) = pt_candidate {
            if let Some(task) = tasks
                .into_iter()
                .find(|t| t["pt_id"].as_str() == Some(pt_id.as_str()))
            {
                return axum::Json(json!({ "task": task })).into_response();
            }
            return (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(json!({"error": format!("pt_id not found: {pt_id}")})),
            )
                .into_response();
        }

        let needle = query.to_ascii_lowercase();
        let hits: Vec<Value> = tasks
            .into_iter()
            .filter(|t| {
                let status = t["status"].as_str().unwrap_or("");
                (include_terminal || (status != "done" && status != "dismissed"))
                    && t["title"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(&needle)
            })
            .collect();
        match hits.len() {
            0 => (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(json!({"error": format!("no task matching {query:?}")})),
            )
                .into_response(),
            1 => axum::Json(json!({ "task": hits.into_iter().next().unwrap() })).into_response(),
            n => (
                axum::http::StatusCode::CONFLICT,
                axum::Json(json!({"error": format!("{n} tasks match {query:?}")})),
            )
                .into_response(),
        }
    }

    async fn spawn_mock_sync() -> (String, Arc<Mutex<Vec<Value>>>) {
        let calls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_h = calls.clone();
        let handler = move |axum::Json(req): axum::Json<Value>| {
            let calls_inner = calls_h.clone();
            async move {
                calls_inner.lock().unwrap().push(req.clone());
                let now = "2026-05-14T00:00:00+00:00";
                let mut tasks: Vec<Value> = Vec::new();
                let mut status: BTreeMap<String, Value> = BTreeMap::new();
                let mut temp_map: BTreeMap<String, String> = BTreeMap::new();
                if let Some(cmds) = req["commands"].as_array() {
                    for cmd in cmds {
                        let uuid = cmd["uuid"].as_str().unwrap_or("?").to_string();
                        status.insert(uuid.clone(), Value::String("ok".into()));
                        let kind = cmd["type"].as_str().unwrap_or("");
                        if kind == "task_create" {
                            let text = cmd["args"]["text"].as_str().unwrap_or("");
                            let task_uuid = format!("uuid-{}", &uuid[..8]);
                            tasks.push(json!({
                                "id": task_uuid.clone(),
                                "pt_id": "PT-1",
                                "title": text,
                                "description": "",
                                "priority": 2,
                                "status": "pending",
                                "created_at": now,
                                "updated_at": now,
                                "deadline": null,
                                "source_type": "remote-cli",
                                "ai_reasoning": ""
                            }));
                            if let Some(t) = cmd["temp_id"].as_str() {
                                temp_map.insert(t.into(), task_uuid);
                            }
                        }
                    }
                }
                // Always return the canonical "existing" tasks for full-sync verbs.
                tasks.extend(existing_tasks_json());
                axum::Json(json!({
                    "sync_token": "42",
                    "resources": { "tasks": tasks },
                    "sync_status": status,
                    "temp_id_mapping": temp_map,
                }))
            }
        };
        // Read routes (v1.9.0): canned responses for next/detail.
        let next_handler = || async {
            axum::Json(json!({ "tasks": [{
                "id": "uuid-bbbbbbbb", "pt_id": "PT-101", "title": "ready task",
                "description": "", "priority": 4, "status": "pending",
                "created_at": "2026-05-13T00:00:00+00:00",
                "updated_at": "2026-05-13T00:00:00+00:00",
                "deadline": null, "source_type": "manual", "ai_reasoning": ""
            }]}))
        };
        let detail_handler = |axum::extract::Path(_uuid): axum::extract::Path<String>| async {
            axum::Json(json!({
                "labels": ["ops"], "project": "fleet", "duration_min": 30,
                "planned_at": null, "energy": null, "depends_on": [], "blocks_tasks": [],
                "recurrence_input": null, "recurrence_mode": null, "recurrence_next": null
            }))
        };
        let resolve_handler = |axum::extract::Query(params): axum::extract::Query<
            BTreeMap<String, String>,
        >| async { resolve_mock_response(params) };
        let app = axum::Router::new()
            .route("/sync", axum::routing::post(handler))
            .route("/next", axum::routing::get(next_handler))
            .route("/detail/{uuid}", axum::routing::get(detail_handler))
            .route("/resolve", axum::routing::get(resolve_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    #[test]
    fn default_url_uses_env() {
        // Use SAFETY: tests don't run concurrently with the env probe.
        unsafe {
            std::env::set_var("PTASK_SYNC_URL", "http://override.example/");
        }
        assert_eq!(default_url(), "http://override.example/");
        unsafe {
            std::env::remove_var("PTASK_SYNC_URL");
        }
    }

    /// Spawn the mock /sync, then run `c.add` on a *dedicated* OS thread so
    /// reqwest::blocking::Client gets a fresh runtime context. The test
    /// thread itself never enters a tokio context — that avoids the
    /// runtime-drop-from-async hazard the blocking client warns about.
    #[test]
    fn remote_add_uses_quick_add_path() {
        let server_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (url, calls) = server_rt.block_on(spawn_mock_sync());
        let c = RemoteClient::with_url(&url).unwrap();
        let task = c.add("Resolve banking setup p4 @ops").unwrap();
        assert_eq!(task.title, "Resolve banking setup p4 @ops");
        let call = &calls.lock().unwrap()[0];
        assert_eq!(call["commands"][0]["type"], "task_create");
        assert!(call["commands"][0]["temp_id"].as_str().is_some());
        assert_eq!(
            call["commands"][0]["args"]["text"],
            "Resolve banking setup p4 @ops"
        );
    }

    #[test]
    fn remote_list_full_sync_filters_locally() {
        let server_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (url, _calls) = server_rt.block_on(spawn_mock_sync());
        let c = RemoteClient::with_url(&url).unwrap();
        let only_p4 = c.list(Some("pending"), Some(4), 10).unwrap();
        assert_eq!(only_p4.len(), 1);
        assert_eq!(only_p4[0].pt_id.as_deref(), Some("PT-101"));
    }

    #[test]
    fn remote_done_resolves_pt_id() {
        let server_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (url, calls) = server_rt.block_on(spawn_mock_sync());
        let c = RemoteClient::with_url(&url).unwrap();
        let task = c.done("PT-100").unwrap();
        assert_eq!(task.pt_id.as_deref(), Some("PT-100"));
        let calls_v = calls.lock().unwrap();
        assert_eq!(
            calls_v.len(),
            1,
            "resolve must use /resolve, not an extra full-sync /sync"
        );
        assert!(
            calls_v.iter().any(|c| c["commands"]
                .as_array()
                .map(|cs| cs.iter().any(|cmd| cmd["type"] == "task_done"))
                .unwrap_or(false)),
            "should have dispatched task_done"
        );
    }

    /// Find the first dispatched command of a given type across all /sync calls.
    fn dispatched<'a>(calls: &'a [Value], kind: &str) -> Option<&'a Value> {
        calls
            .iter()
            .filter_map(|c| c["commands"].as_array())
            .flatten()
            .find(|cmd| cmd["type"] == kind)
    }

    /// Returns the runtime too: the caller must keep it alive, otherwise
    /// dropping it tears down the spawned mock-`/sync` server mid-test.
    fn mock_client() -> (
        RemoteClient,
        Arc<Mutex<Vec<Value>>>,
        tokio::runtime::Runtime,
    ) {
        let server_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (url, calls) = server_rt.block_on(spawn_mock_sync());
        (RemoteClient::with_url(&url).unwrap(), calls, server_rt)
    }

    #[test]
    fn remote_priority_dispatches_task_priority() {
        let (c, calls, _rt) = mock_client();
        let task = c.priority("PT-101", 5).unwrap();
        assert_eq!(task.pt_id.as_deref(), Some("PT-101"));
        assert_eq!(task.priority, 5, "returned task reflects the new priority");
        let calls_v = calls.lock().unwrap();
        let cmd = dispatched(&calls_v, "task_priority").expect("task_priority dispatched");
        assert_eq!(cmd["args"]["priority"], 5);
        assert_eq!(cmd["args"]["task_uuid"], "uuid-bbbbbbbb");
    }

    #[test]
    fn remote_edit_dispatches_task_edit_with_deadline() {
        let (c, calls, _rt) = mock_client();
        let task = c
            .edit("PT-100", None, None, Some(Some("2026-07-01")))
            .unwrap();
        assert_eq!(task.deadline.as_deref(), Some("2026-07-01"));
        let calls_v = calls.lock().unwrap();
        let cmd = dispatched(&calls_v, "task_edit").expect("task_edit dispatched");
        assert_eq!(cmd["args"]["deadline"], "2026-07-01");
    }

    #[test]
    fn remote_edit_clear_deadline_sends_json_null() {
        let (c, calls, _rt) = mock_client();
        let task = c.edit("PT-100", None, None, Some(None)).unwrap();
        assert!(task.deadline.is_none());
        let calls_v = calls.lock().unwrap();
        let cmd = dispatched(&calls_v, "task_edit").expect("task_edit dispatched");
        assert!(
            cmd["args"]["deadline"].is_null(),
            "clear must send JSON null, got {:?}",
            cmd["args"]["deadline"]
        );
    }

    #[test]
    fn remote_edit_title_and_deadline_share_one_resolve() {
        // Regression for the wrong-task hazard: a combined title+deadline edit
        // must resolve ONCE and send both commands against the SAME task_uuid in
        // a single /sync request (not two independent resolves).
        let (c, calls, _rt) = mock_client();
        let task = c
            .edit("PT-100", Some("renamed"), None, Some(Some("2026-08-01")))
            .unwrap();
        assert_eq!(task.title, "renamed");
        assert_eq!(task.deadline.as_deref(), Some("2026-08-01"));
        let calls_v = calls.lock().unwrap();
        // Exactly one /sync request carrying BOTH commands.
        let sync_calls: Vec<_> = calls_v
            .iter()
            .filter(|c| c["commands"].as_array().is_some_and(|a| !a.is_empty()))
            .collect();
        assert_eq!(sync_calls.len(), 1, "must be a single /sync request");
        let cmds = sync_calls[0]["commands"].as_array().unwrap();
        assert_eq!(cmds.len(), 2, "retext + edit in one request");
        // Both reference the same resolved task_uuid.
        assert!(
            cmds.iter()
                .all(|cmd| cmd["args"]["task_uuid"] == "uuid-aaaaaaaa"),
            "both commands must target the one resolved uuid"
        );
    }

    #[test]
    fn remote_reopen_dispatches_task_reopen() {
        let (c, calls, _rt) = mock_client();
        let task = c.reopen("PT-100").unwrap();
        assert_eq!(task.status, "pending");
        let calls_v = calls.lock().unwrap();
        let cmd = dispatched(&calls_v, "task_reopen").expect("task_reopen dispatched");
        assert_eq!(cmd["args"]["task_uuid"], "uuid-aaaaaaaa");
    }

    #[test]
    fn remote_show_is_read_only() {
        let (c, calls, _rt) = mock_client();
        let task = c.show("PT-101").unwrap();
        assert_eq!(task.pt_id.as_deref(), Some("PT-101"));
        let calls_v = calls.lock().unwrap();
        assert!(calls_v.is_empty(), "show must use read-only /resolve only");
    }

    #[test]
    fn remote_show_resolves_terminal_substring() {
        let (c, calls, _rt) = mock_client();
        let task = c.show("archive").unwrap();
        assert_eq!(task.pt_id.as_deref(), Some("PT-102"));
        assert_eq!(task.status, "done");
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn remote_edit_text_only_dispatches_task_retext() {
        let (c, calls, _rt) = mock_client();
        let task = c.edit("PT-100", Some("renamed"), None, None).unwrap();
        assert_eq!(task.title, "renamed");
        let calls_v = calls.lock().unwrap();
        let cmd = dispatched(&calls_v, "task_retext").expect("task_retext dispatched");
        assert_eq!(cmd["args"]["title"], "renamed");
        assert!(
            cmd["args"]["description"].is_null(),
            "unset description must be JSON null"
        );
        // text-only edit must NOT also dispatch a deadline command.
        assert!(
            dispatched(&calls_v, "task_edit").is_none(),
            "text-only edit must not touch the deadline"
        );
    }

    #[test]
    fn remote_next_fetches_ready_tasks() {
        let (c, _calls, _rt) = mock_client();
        let rows = c.next(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pt_id.as_deref(), Some("PT-101"));
    }

    #[test]
    fn remote_detail_fetches_side_table() {
        let (c, _calls, _rt) = mock_client();
        let d = c.detail("uuid-bbbbbbbb").unwrap();
        assert_eq!(d.labels, vec!["ops".to_string()]);
        assert_eq!(d.project.as_deref(), Some("fleet"));
        assert_eq!(d.duration_min, Some(30));
    }

    #[test]
    fn remote_dismiss_dispatches_task_dismiss() {
        let (c, calls, _rt) = mock_client();
        let task = c.dismiss("PT-100").unwrap();
        assert_eq!(task.status, "dismissed");
        let calls_v = calls.lock().unwrap();
        let cmd = dispatched(&calls_v, "task_dismiss").expect("task_dismiss dispatched");
        assert_eq!(cmd["args"]["task_uuid"], "uuid-aaaaaaaa");
    }
}
