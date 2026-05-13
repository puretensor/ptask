//! pTask distillation orchestrator.
//!
//! v0.6.5 — Rust owns the timer and the audit log; the existing Python
//! `ingest.distill` module is invoked as a subprocess. Stdout/stderr are
//! captured (line counts + tails), exit status determines success, and
//! every run lands a `distill.run` row in `pt_event_log` for the sync API
//! to surface to clients.
//!
//! v0.9.0 will replace the subprocess with a native Rust pipeline. The
//! `pt_event_log` row shape stays the same across the swap so dashboards
//! and clients don't need to change.

use anyhow::{Context, Result};
use ptask_core::Db;
use ptask_core::event_log;
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
    let start = std::time::Instant::now();
    let mut cmd = Command::new("python3");
    cmd.arg("-m")
        .arg("ingest.distill")
        .args(args)
        .current_dir(&root);
    let out = cmd
        .output()
        .with_context(|| format!("spawning python3 -m ingest.distill in {:?}", root))?;
    let duration_ms = start.elapsed().as_millis();

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout_lines = stdout.lines().count();
    let stderr_lines = stderr.lines().count();
    let report = RunReport {
        exit_code: out.status.code(),
        success: out.status.success(),
        stdout_lines,
        stderr_lines,
        stdout_tail: tail(&stdout, 2000),
        stderr_tail: tail(&stderr, 2000),
        duration_ms,
    };

    let payload = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    let event_uuid = format!("distill:{}", uuid::Uuid::new_v4());
    let event_type = if report.success {
        "distill.run"
    } else {
        "distill.failed"
    };
    if let Err(e) = event_log::record(db, &event_uuid, None, event_type, &payload) {
        warn!(target: "ptask::distill", error = %e, "pt_event_log record failed");
    }

    if report.success {
        info!(
            target: "ptask::distill",
            duration_ms,
            stdout_lines,
            stderr_lines,
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
        unsafe {
            std::env::remove_var("PTASK_DISTILL_PY_ROOT");
        }
        let p = python_root();
        assert!(p.ends_with("puretensor-tasks"));
    }
}
