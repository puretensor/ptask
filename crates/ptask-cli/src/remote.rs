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
use serde::{Deserialize, Serialize};
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
            return Err(anyhow!("remote /sync {status}: {body}"));
        }
        resp.json::<SyncResp>().context("parse /sync response")
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
    /// substring. Resolves the task locally via a full-sync `list` then
    /// dispatches `task_done` by uuid.
    pub fn done(&self, query: &str) -> Result<Task> {
        let task = self.resolve(query)?;
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

    fn resolve(&self, query: &str) -> Result<Task> {
        let req = json!({ "sync_token": "*", "resource_types": ["tasks"] });
        let resp = self.sync(&req)?;
        let tasks = resp.resources.tasks;
        if let Some(stripped) = query
            .strip_prefix("PT-")
            .or_else(|| query.strip_prefix("pt-"))
        {
            let needle = format!("PT-{}", stripped);
            if let Some(t) = tasks
                .into_iter()
                .find(|t| t.pt_id.as_deref() == Some(needle.as_str()))
            {
                return Ok(t);
            }
        } else if let Ok(n) = query.parse::<u64>() {
            let needle = format!("PT-{n}");
            if let Some(t) = tasks
                .into_iter()
                .find(|t| t.pt_id.as_deref() == Some(needle.as_str()))
            {
                return Ok(t);
            }
        } else {
            let needle = query.to_ascii_lowercase();
            let mut hits: Vec<Task> = tasks
                .into_iter()
                .filter(|t| {
                    t.status != "done"
                        && t.status != "dismissed"
                        && t.title.to_ascii_lowercase().contains(&needle)
                })
                .collect();
            if hits.len() == 1 {
                return Ok(hits.remove(0));
            }
            if hits.len() > 1 {
                let titles: Vec<&str> = hits.iter().map(|t| t.title.as_str()).collect();
                return Err(anyhow!(
                    "remote: query {query:?} matched {} tasks — be more specific: {titles:?}",
                    hits.len()
                ));
            }
        }
        Err(anyhow!("remote: no task matched {query:?}"))
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

#[derive(Serialize)] // tests can build mock responses.
struct _Echo;

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
                let existing = vec![
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
                        "id": "uuid-newer-existing", "pt_id": "PT-999",
                        "title": "newer existing task should not be returned by add",
                        "description": "", "priority": 5, "status": "pending",
                        "created_at": "2026-05-15T00:00:00+00:00",
                        "updated_at": "2026-05-15T00:00:00+00:00",
                        "deadline": null, "source_type": "manual", "ai_reasoning": ""
                    }),
                ];
                tasks.extend(existing);
                axum::Json(json!({
                    "sync_token": "42",
                    "resources": { "tasks": tasks },
                    "sync_status": status,
                    "temp_id_mapping": temp_map,
                }))
            }
        };
        let app = axum::Router::new().route("/sync", axum::routing::post(handler));
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
        assert!(
            calls_v.iter().any(|c| c["commands"]
                .as_array()
                .map(|cs| cs.iter().any(|cmd| cmd["type"] == "task_done"))
                .unwrap_or(false)),
            "should have dispatched task_done"
        );
    }
}
