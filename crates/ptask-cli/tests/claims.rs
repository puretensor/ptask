//! Black-box tests for claim ownership, leases and recovery: `pt claim`,
//! `pt heartbeat`, `pt release`, `pt reclaim`, and the hourly sweep that
//! only runs when the operator turns it on.

mod common;
use common::Pt;

fn claim_of(pt: &Pt, id: &str) -> serde_json::Value {
    pt.json(&["show", id])["claim"].clone()
}

/// `pt --json claim` as `actor`; returns the minted instance token.
fn claim_token(pt: &Pt, actor: &str, id: &str, extra: &[&str]) -> String {
    let mut args = vec!["--json", "claim", id];
    args.extend_from_slice(extra);
    let out = pt.ok_as(actor, &args);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    v["claim_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| panic!("pt claim returned no claim_token: {v}"))
        .to_string()
}

fn status_of(pt: &Pt, id: &str) -> String {
    pt.json(&["show", id])["status"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Make PT-N's lease end `mins` minutes ago.
fn expire_lease(pt: &Pt, pt_id: &str, mins: i64) {
    let db = ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap();
    db.with_conn(|c| {
        c.execute(
            "UPDATE tasks SET claim_expires_at =
                 strftime('%Y-%m-%dT%H:%M:%S', 'now', ?1) || '+00:00'
              WHERE pt_id = ?2",
            ptask_core::rusqlite::params![format!("-{mins} minutes"), pt_id],
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_claim_is_owned_and_the_second_claimer_learns_the_holder() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Roll the ceph OSDs"]);
    let out = pt.ok_as("hal", &["claim", "PT-1", "--lease", "30m"]);
    assert!(out.contains("claimed"), "{out}");
    let c = claim_of(&pt, "PT-1");
    assert_eq!(c["by"], "hal");
    assert_eq!(c["expired"], false);
    assert!(c["expires_at"].is_string());

    let second = pt.run_as("grok", &["claim", "PT-1"]);
    assert!(!second.status.success());
    let err = String::from_utf8_lossy(&second.stderr);
    assert!(err.contains("already claimed by hal"), "{err}");

    let shown = pt.ok(&["--no-color", "show", "PT-1"]);
    assert!(shown.contains("claimed by"), "{shown}");
    assert!(shown.contains("hal"), "{shown}");
    // A bad lease is refused before anything changes.
    pt.ok(&["add", "--raw", "other"]);
    assert!(
        !pt.run_as("hal", &["claim", "PT-2", "--lease", "3d"])
            .status
            .success()
    );
    assert_eq!(status_of(&pt, "PT-2"), "todo");
}

#[test]
fn heartbeat_is_the_holders_and_a_lost_claim_exits_non_zero() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Long migration"]);
    let token = claim_token(&pt, "hal", "PT-1", &["--lease", "10m"]);
    pt.ok_as(
        "hal",
        &["heartbeat", "PT-1", "--claim", &token, "--lease", "2h"],
    );
    let stolen = pt.run_as("grok", &["heartbeat", "PT-1", "--claim", &token]);
    assert!(!stolen.status.success());
    assert!(String::from_utf8_lossy(&stolen.stderr).contains("claim lost"));
    // A heartbeat writes no journal event: it is not a mutation to replay.
    assert!(
        !pt.run_as(
            "hal",
            &[
                "--idempotency-key",
                "hb-1",
                "heartbeat",
                "PT-1",
                "--claim",
                &token
            ]
        )
        .status
        .success()
    );
    pt.ok_as("hal", &["done", "PT-1"]);
    let after_close = pt.run_as("hal", &["heartbeat", "PT-1", "--claim", &token]);
    assert!(!after_close.status.success());
    assert!(String::from_utf8_lossy(&after_close.stderr).contains("stop work"));
    assert!(claim_of(&pt, "PT-1").is_null(), "closing drops the claim");
}

#[test]
fn release_is_the_holders_and_the_operator_can_force_it() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Patch fox-n1 firmware"]);
    pt.ok_as("hal", &["claim", "PT-1"]);
    let refused = pt.run_as("grok", &["release", "PT-1"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--force"));
    assert_eq!(status_of(&pt, "PT-1"), "in_progress");

    let out = pt.ok_as(
        "shell",
        &["release", "PT-1", "--force", "-m", "hal session died"],
    );
    assert!(out.contains("forced"), "{out}");
    assert_eq!(status_of(&pt, "PT-1"), "todo");
    assert!(claim_of(&pt, "PT-1").is_null());
    let log = pt.ok(&["--no-color", "log", "PT-1"]);
    assert!(log.contains("task.released"), "{log}");

    // Now anyone may claim it; the holder releases its own without --force.
    let grok_token = claim_token(&pt, "grok", "PT-1", &[]);
    pt.ok_as(
        "grok",
        &[
            "release",
            "PT-1",
            "--claim",
            &grok_token,
            "--reason",
            "blocked on BMC creds",
        ],
    );
    assert_eq!(status_of(&pt, "PT-1"), "todo");
    // `pt start` makes the starter the holder.
    pt.ok_as("shell", &["start", "PT-1"]);
    assert_eq!(claim_of(&pt, "PT-1")["by"], "shell");
}

#[test]
fn reclaim_lists_by_default_and_applies_on_request() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "dead agent's task"]); // PT-1
    pt.ok(&["add", "--raw", "live agent's task"]); // PT-2
    let hal_token = claim_token(&pt, "hal", "PT-1", &["--lease", "10m"]);
    pt.ok_as("grok", &["claim", "PT-2", "--lease", "10m"]);
    expire_lease(&pt, "PT-1", 5);
    assert_eq!(claim_of(&pt, "PT-1")["expired"], true);

    let listed = pt.json(&["reclaim"]);
    assert_eq!(listed["dry_run"], true);
    assert_eq!(listed["reclaimed"].as_array().unwrap().len(), 1);
    assert_eq!(listed["reclaimed"][0]["holder"], "hal");
    assert_eq!(
        status_of(&pt, "PT-1"),
        "in_progress",
        "listing writes nothing"
    );

    // The digest shows the abandoned work to the next session.
    let digest = pt.json(&["digest"]);
    assert_eq!(digest["expired_claims"][0]["pt_id"], "PT-1");

    let applied = pt.json(&["reclaim", "--apply"]);
    assert_eq!(applied["reclaimed"][0]["pt_id"], "PT-1");
    assert_eq!(status_of(&pt, "PT-1"), "todo");
    assert_eq!(status_of(&pt, "PT-2"), "in_progress");
    let lost = pt.run_as("hal", &["heartbeat", "PT-1", "--claim", &hal_token]);
    assert!(!lost.status.success(), "the old holder is told to stop");
    pt.ok_as("grok", &["claim", "PT-1"]);
}

#[test]
fn the_hourly_run_reclaims_only_when_the_operator_turns_it_on() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "abandoned"]);
    pt.ok_as("hal", &["claim", "PT-1", "--lease", "10m"]);
    expire_lease(&pt, "PT-1", 5);

    pt.ok(&["scoring", "run"]);
    assert_eq!(status_of(&pt, "PT-1"), "in_progress", "off by default");

    let out = pt
        .command("test", &["scoring", "run", "--dry-run"])
        .env("PTASK_CLAIM_RECLAIM", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        status_of(&pt, "PT-1"),
        "in_progress",
        "a dry run writes nothing"
    );

    let out = pt
        .command("test", &["scoring", "run"])
        .env("PTASK_CLAIM_RECLAIM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .to_lowercase()
            .contains("reclaimed")
    );
    assert_eq!(status_of(&pt, "PT-1"), "todo");
}

#[test]
fn a_keyed_claim_replays_and_metrics_count_claims() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "keyed"]);
    pt.ok(&["add", "--raw", "expired"]);
    pt.ok_as("hal", &["--idempotency-key", "c-1", "claim", "PT-1"]);
    let again = pt.ok_as("hal", &["--idempotency-key", "c-1", "claim", "PT-1"]);
    assert!(again.contains("replayed"), "{again}");
    pt.ok_as("hal", &["claim", "PT-2", "--lease", "5m"]);
    expire_lease(&pt, "PT-2", 1);

    let srv = pt.serve();
    let host = srv.url.trim_start_matches("http://");
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(host).unwrap();
    write!(
        s,
        "GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut body = String::new();
    s.read_to_string(&mut body).unwrap();
    assert!(
        body.contains("pt_claims_active{holder=\"hal\"} 2"),
        "{body}"
    );
    assert!(body.contains("pt_claims_expired 1"), "{body}");
}
