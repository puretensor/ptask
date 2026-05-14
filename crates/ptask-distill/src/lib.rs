//! pTask distillation orchestrator.
//!
//! v0.6.5 — Rust owns the timer and the audit log; the existing Python
//! `ingest.distill` module is invoked as a subprocess. Stdout/stderr are
//! captured (line counts + tails), exit status determines success, and
//! every run lands a `distill.run` row in `pt_event_log` for the sync API
//! to surface to clients.
//!
//! v0.8.2 — Native SBERT embeddings via `candle-transformers`; see the
//! `embeddings` submodule. The Python `ingest.distill` subprocess shim
//! stays the cutover entry point until Phase 9 retires it at v0.9.0.

pub mod classifier;
pub mod clustering;
pub mod collectors;
pub mod consolidation;
pub mod embeddings;
pub mod semantic_dedup;
pub mod temporal_dedup;

use anyhow::{Context, Result};
use ptask_core::Db;
use ptask_core::event_log;
use ptask_core::pt_id;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use tracing::{info, warn};

/// Result of one distillation run. Includes counts and tail snippets that
/// summarise what happened — useful for `pt distill` CLI output and for the
/// `pt_event_log` payload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunReport {
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout_lines: usize,
    pub stderr_lines: usize,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub duration_ms: u128,
    pub new_tasks: usize,
    pub backfilled_pt_ids: usize,
}

/// Where the legacy Python lives. Override via env for fleet rollouts.
pub fn python_root() -> PathBuf {
    std::env::var("PTASK_DISTILL_PY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            PathBuf::from(home).join("puretensor-tasks")
        })
}

/// Run `python3 -m ingest.distill [args]` against the configured root.
/// Records a `distill.run` event in `pt_event_log` regardless of outcome.
pub fn run(db: &Db, args: &[&str]) -> Result<RunReport> {
    let root = python_root();
    info!(target: "ptask::distill", cwd = %root.display(), "starting distill subprocess");
    let before_tasks = task_uuid_set(db).context("snapshotting tasks before distill")?;
    let start = std::time::Instant::now();
    let mut cmd = Command::new("python3");
    cmd.arg("-m")
        .arg("ingest.distill")
        .args(args)
        .current_dir(&root);
    let output = cmd.output();
    let duration_ms = start.elapsed().as_millis();

    let (exit_code, success, stdout, stderr) = match output {
        Ok(out) => (
            out.status.code(),
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ),
        Err(e) => (None, false, String::new(), format!("spawn failed: {e}")),
    };
    let stdout_lines = stdout.lines().count();
    let stderr_lines = stderr.lines().count();

    let after_tasks = task_uuid_set(db).context("snapshotting tasks after distill")?;
    let mut new_task_uuids: Vec<String> = after_tasks.difference(&before_tasks).cloned().collect();
    new_task_uuids.sort();
    // Python owns the legacy `tasks` write path until v0.9, but pTask owns
    // `pt_extensions`; mint PT-Ns immediately so new distilled tasks are
    // addressable through every Rust surface.
    let backfilled_pt_ids = pt_id::backfill_all(db).context("backfilling PT-N after distill")?;

    let report = RunReport {
        exit_code,
        success,
        stdout_lines,
        stderr_lines,
        stdout_tail: tail(&stdout, 2000),
        stderr_tail: tail(&stderr, 2000),
        duration_ms,
        new_tasks: new_task_uuids.len(),
        backfilled_pt_ids,
    };

    let run_uuid = format!("distill:{}", uuid::Uuid::new_v4());
    record_new_task_events(db, &run_uuid, &new_task_uuids)?;
    let payload = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    let event_type = if report.success {
        "distill.run"
    } else {
        "distill.failed"
    };
    if let Err(e) = event_log::record(db, &run_uuid, None, event_type, &payload) {
        warn!(target: "ptask::distill", error = %e, "pt_event_log record failed");
    }

    if report.success {
        info!(
            target: "ptask::distill",
            duration_ms,
            stdout_lines,
            stderr_lines,
            new_tasks = report.new_tasks,
            backfilled_pt_ids,
            "distill ok"
        );
    } else {
        warn!(
            target: "ptask::distill",
            exit = ?report.exit_code,
            duration_ms,
            stderr_tail = %report.stderr_tail,
            "distill failed"
        );
    }

    Ok(report)
}

fn task_uuid_set(db: &Db) -> Result<HashSet<String>> {
    db.with_conn(|c| {
        let mut stmt = c.prepare("SELECT id FROM tasks")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<HashSet<_>, _>>()?)
    })
    .map_err(anyhow::Error::from)
}

fn record_new_task_events(db: &Db, run_uuid: &str, task_uuids: &[String]) -> Result<()> {
    for task_uuid in task_uuids {
        let pt_id = db
            .with_conn(|c| pt_id::lookup_pt_id(c, task_uuid))
            .unwrap_or(None);
        let payload = serde_json::json!({
            "task_uuid": task_uuid,
            "pt_id": pt_id,
            "source": "distill",
            "run_uuid": run_uuid,
        });
        let event_uuid = format!("{run_uuid}:task:{task_uuid}");
        if let Err(e) =
            event_log::record(db, &event_uuid, Some(task_uuid), "task.created", &payload)
        {
            warn!(
                target: "ptask::distill",
                task_uuid,
                error = %e,
                "task.created event_log record failed"
            );
        }
    }
    Ok(())
}

/// Keep at most `max_bytes` of the *tail* of a possibly-long string.
fn tail(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Find a char boundary at-or-after the cut point.
    let start = s.len() - max_bytes;
    let mut idx = start;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    format!("…{}", &s[idx..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn tail_keeps_short_strings_intact() {
        assert_eq!(tail("abc", 10), "abc");
    }

    #[test]
    fn tail_truncates_long_strings_with_ellipsis() {
        let s = "x".repeat(1000);
        let t = tail(&s, 100);
        assert!(t.starts_with('…'));
        // 100 byte budget + the 3-byte … = 103 total.
        assert_eq!(t.chars().filter(|&c| c == 'x').count(), 100);
    }

    #[test]
    fn tail_respects_char_boundaries() {
        // Stress: 4-byte chars near the cut point.
        let s = format!("{}xy", "🌙".repeat(100));
        let t = tail(&s, 5);
        // No panic == passing this test.
        assert!(t.starts_with('…'));
    }

    #[test]
    fn python_root_defaults_to_home_puretensor_tasks() {
        let _env = env_lock();
        unsafe {
            std::env::remove_var("PTASK_DISTILL_PY_ROOT");
        }
        let p = python_root();
        assert!(p.ends_with("puretensor-tasks"));
    }

    #[test]
    fn run_backfills_python_created_tasks_and_records_task_events() {
        let _env = env_lock();
        let (root, db_path, db) = fake_python_root_and_db(
            r#"import os, sqlite3
con = sqlite3.connect(os.environ["DB_PATH"])
con.execute("INSERT INTO tasks (id, title, created_at) VALUES (?, ?, ?)",
            ("py-task-1", "distilled from python", "2026-05-13T12:00:00+00:00"))
con.commit()
print("created one task")
"#,
        );
        unsafe {
            std::env::set_var("PTASK_DISTILL_PY_ROOT", &root);
            std::env::set_var("DB_PATH", &db_path);
        }

        let report = run(&db, &["--days", "60"]).unwrap();
        assert!(report.success, "stderr: {}", report.stderr_tail);
        assert_eq!(report.new_tasks, 1);
        assert_eq!(report.backfilled_pt_ids, 1);

        db.with_conn(|c| {
            let pt: String = c
                .query_row(
                    "SELECT pt_id FROM pt_extensions WHERE task_uuid='py-task-1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(pt, "PT-2");
            let task_events: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pt_event_log
                     WHERE event_type='task.created' AND task_uuid='py-task-1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(task_events, 1);
            let run_events: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pt_event_log WHERE event_type='distill.run'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(run_events, 1);
            Ok(())
        })
        .unwrap();

        unsafe {
            std::env::remove_var("PTASK_DISTILL_PY_ROOT");
            std::env::remove_var("DB_PATH");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn run_records_failed_event_when_subprocess_cannot_spawn() {
        let _env = env_lock();
        let (root, _db_path, db) = fake_python_root_and_db("");
        let missing_root =
            std::env::temp_dir().join(format!("ptask-distill-missing-{}", uuid::Uuid::new_v4()));
        unsafe {
            std::env::set_var("PTASK_DISTILL_PY_ROOT", &missing_root);
        }

        let report = run(&db, &[]).unwrap();
        assert!(!report.success);
        assert!(report.stderr_tail.contains("spawn failed"));
        db.with_conn(|c| {
            let failed: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pt_event_log WHERE event_type='distill.failed'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(failed, 1);
            Ok(())
        })
        .unwrap();

        unsafe {
            std::env::remove_var("PTASK_DISTILL_PY_ROOT");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fake_python_root_and_db(script: &str) -> (PathBuf, PathBuf, Db) {
        let root = std::env::temp_dir().join(format!("ptask-distill-{}", uuid::Uuid::new_v4()));
        let ingest = root.join("ingest");
        std::fs::create_dir_all(&ingest).unwrap();
        std::fs::write(ingest.join("__init__.py"), "").unwrap();
        std::fs::write(ingest.join("distill.py"), script).unwrap();

        let db_path = root.join("tasks.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute(
                "CREATE TABLE tasks (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    created_at TEXT NOT NULL
                 )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks (id, title, created_at) VALUES (?1, ?2, ?3)",
                params!["existing", "existing task", "2026-05-12T12:00:00+00:00"],
            )
            .unwrap();
        }
        let db = Db::open(&db_path).unwrap();
        ptask_core::pt_id::backfill_all(&db).unwrap();
        (root, db_path, db)
    }
}
