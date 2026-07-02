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
pub mod collectors;
pub mod consolidation;
pub mod temporal_dedup;
pub mod types;

// The candle-backed embedding stack and its consumers. Gated: the native
// pipeline has never been the production entry point (the v0.6.5 Python
// subprocess shim drives `pt distill`), so the default build skips the
// heaviest dependency subtree in the workspace.
#[cfg(feature = "native-ml")]
pub mod clustering;
#[cfg(feature = "native-ml")]
pub mod embeddings;
#[cfg(feature = "native-ml")]
pub mod semantic_dedup;

use anyhow::{Context, Result};
use ptask_core::Db;
use ptask_core::event_log;
use ptask_core::event_log::EventCtx;
use ptask_core::pt_id;
use std::collections::HashSet;
use std::path::Path;
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
    /// Set when the subprocess exited 0 but its output carries an
    /// all-stages-failed signature (see [`detect_soft_failure`]). A soft
    /// failure forces `success = false`.
    pub soft_failure: Option<String>,
}

/// Run `python3 -m ingest.distill [args]` against `py_root` (the legacy
/// Python pipeline root — comes from `Config::from_env().distill.py_root`
/// at the entrypoint; this crate never reads the environment).
/// Records a `distill.run` event in `pt_event_log` regardless of outcome.
pub fn run(db: &Db, args: &[&str], py_root: &Path) -> Result<RunReport> {
    let root = py_root;
    info!(target: "ptask::distill", cwd = %root.display(), "starting distill subprocess");
    let before_tasks = task_uuid_set(db).context("snapshotting tasks before distill")?;
    let start = std::time::Instant::now();
    let mut cmd = Command::new("python3");
    cmd.arg("-m")
        .arg("ingest.distill")
        .args(args)
        .current_dir(root);
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
    let soft_failure = if success {
        detect_soft_failure(&stdout, &stderr)
    } else {
        None
    };
    let success = success && soft_failure.is_none();

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
        soft_failure,
    };

    let run_uuid = format!("distill:{}", uuid::Uuid::new_v4());
    record_new_task_events(db, &run_uuid, &new_task_uuids)?;
    let payload = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    let event_type = if report.success {
        "distill.run"
    } else {
        "distill.failed"
    };
    if let Err(e) = event_log::record(
        db,
        &run_uuid,
        None,
        event_type,
        &payload,
        &EventCtx::system("distill"),
    ) {
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
            soft_failure = report.soft_failure.as_deref().unwrap_or(""),
            stderr_tail = %report.stderr_tail,
            "distill failed"
        );
    }

    Ok(report)
}

/// Failure signatures the Python exit code hides.
///
/// The shim's contract with `ingest.distill` is "exit 0 = pipeline healthy",
/// but the Python pipeline logs stage errors and keeps going: a run where
/// every cluster fails LLM consolidation (e.g. expired GOOGLE_API_KEY) still
/// exits 0 while producing zero tasks. Scan captured output for the
/// all-failed markers so such runs land as `distill.failed` and the systemd
/// unit reports failure. Partial failures (some clusters consolidated) stay
/// successful — output is degraded, not absent.
fn detect_soft_failure(stdout: &str, stderr: &str) -> Option<String> {
    for line in stderr.lines().chain(stdout.lines()) {
        if let Some((failed, total)) = parse_failed_clusters(line)
            && failed == total
            && total > 0
        {
            return Some(format!(
                "all {total} clusters failed consolidation — stage 5 produced nothing"
            ));
        }
    }
    None
}

/// Parse the Python `Stage 5: F/T clusters failed consolidation` warning.
fn parse_failed_clusters(line: &str) -> Option<(u64, u64)> {
    let idx = line.find(" clusters failed consolidation")?;
    let frac = line[..idx].split_whitespace().last()?;
    let (f, t) = frac.split_once('/')?;
    Some((f.parse().ok()?, t.parse().ok()?))
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
        if let Err(e) = event_log::record(
            db,
            &event_uuid,
            Some(task_uuid),
            "task.created",
            &payload,
            &EventCtx::system("distill"),
        ) {
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
    use std::path::PathBuf;

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
    fn soft_failure_detects_all_clusters_failed() {
        let stderr = "2026-06-10 12:00:00 WARNING puretensor-tasks.consolidate: \
                      Stage 5: 8/8 clusters failed consolidation";
        let reason = detect_soft_failure("", stderr).expect("should detect");
        assert!(reason.contains("all 8 clusters"));
    }

    #[test]
    fn soft_failure_ignores_partial_cluster_failures() {
        let stderr = "WARNING: Stage 5: 3/8 clusters failed consolidation";
        assert_eq!(detect_soft_failure("", stderr), None);
    }

    #[test]
    fn soft_failure_ignores_clean_output() {
        let stderr = "INFO: === Distillation pipeline DONE: 4 canonical tasks created ===";
        assert_eq!(detect_soft_failure("", stderr), None);
        assert_eq!(detect_soft_failure("", ""), None);
    }

    #[test]
    fn parse_failed_clusters_handles_malformed_lines() {
        assert_eq!(parse_failed_clusters("clusters failed consolidation"), None);
        assert_eq!(
            parse_failed_clusters("x/y clusters failed consolidation"),
            None
        );
        assert_eq!(
            parse_failed_clusters("Stage 5: 0/0 clusters failed consolidation"),
            Some((0, 0))
        );
        // 0/0 must not trip the all-failed gate (total > 0 required).
        assert_eq!(
            detect_soft_failure("", "Stage 5: 0/0 clusters failed consolidation"),
            None
        );
    }

    #[test]
    fn run_flags_soft_failure_when_all_clusters_fail_but_exit_is_zero() {
        let (root, _db_path, db) = fake_python_root_and_db(
            r#"import sys
print("collected 100 items")
print("Stage 5: 8/8 clusters failed consolidation", file=sys.stderr)
sys.exit(0)
"#,
        );
        let report = run(&db, &[], &root).unwrap();
        assert_eq!(report.exit_code, Some(0));
        assert!(!report.success, "all-clusters-failed must not count as ok");
        assert!(report.soft_failure.is_some());
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

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn run_backfills_python_created_tasks_and_records_task_events() {
        let (root, db_path, db) = fake_python_root_and_db(
            r#"import sqlite3
con = sqlite3.connect("tasks.db")
con.execute("INSERT INTO tasks (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)",
            ("py-task-1", "distilled from python", "2026-05-13T12:00:00+00:00",
             "2026-05-13T12:00:00+00:00"))
con.commit()
print("created one task")
"#,
        );
        let _ = &db_path;
        let report = run(&db, &["--days", "60"], &root).unwrap();
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

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn run_records_failed_event_when_subprocess_cannot_spawn() {
        let (root, _db_path, db) = fake_python_root_and_db("");
        let missing_root =
            std::env::temp_dir().join(format!("ptask-distill-missing-{}", uuid::Uuid::new_v4()));
        let report = run(&db, &[], &missing_root).unwrap();
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

        let _ = std::fs::remove_dir_all(root);
    }

    fn fake_python_root_and_db(script: &str) -> (PathBuf, PathBuf, Db) {
        let root = std::env::temp_dir().join(format!("ptask-distill-{}", uuid::Uuid::new_v4()));
        let ingest = root.join("ingest");
        std::fs::create_dir_all(&ingest).unwrap();
        std::fs::write(ingest.join("__init__.py"), "").unwrap();
        std::fs::write(ingest.join("distill.py"), script).unwrap();

        let db_path = root.join("tasks.db");
        // V008 bootstraps the production-shape legacy schema — no stub.
        let db = Db::open(&db_path).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tasks (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                params!["existing", "existing task", "2026-05-12T12:00:00+00:00"],
            )?;
            Ok(())
        })
        .unwrap();
        ptask_core::pt_id::backfill_all(&db).unwrap();
        (root, db_path, db)
    }
}
