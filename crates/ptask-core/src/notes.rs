//! Task notes: the attributed evidence trail on a task.
//!
//! A note is a journal event, not a column. `pt note` writes a `task.noted`
//! event; a closing verb given a note (`pt done --note`, `pt dismiss --note`,
//! MCP `task_done(note)`, `/sync`, the cockpit) carries it as `note` in the
//! closing event's own payload, so the evidence commits in the same
//! transaction as the status flip it justifies and an undo or replay sees
//! one event, not two. Notes are append-only by construction (there is no
//! edit or delete verb), and who wrote one, through which surface, comes
//! from the journal's attribution rather than from anything the caller
//! types.
//!
//! Before this, an agent closing a task had nowhere to put its verification
//! evidence except the description (overwriting the original ask) or a
//! report outside pTask; a closed task's status was a claim with nothing
//! behind it.

use crate::error::{Error, Result};
use crate::event_log::EventCtx;
use crate::storage::Db;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Longest note accepted, in characters (the approvals note cap).
pub const MAX_NOTE_CHARS: usize = 16 * 1024;

/// Most notes one read returns (the newest ones, oldest first).
pub const MAX_NOTES_LISTED: usize = 100;

/// Character budget for a note on compact surfaces (digest, worker brief).
/// `pt show` and `pt context --json`'s `notes` array keep the full text.
pub const NOTE_PREVIEW_CHARS: usize = 300;

/// Truncate a note for the digest and the markdown worker brief: keep the
/// start, then a marker so the cut is visible. Short notes pass through.
pub fn preview(text: &str) -> String {
    let n = text.chars().count();
    if n <= NOTE_PREVIEW_CHARS {
        return text.to_string();
    }
    let prefix: String = text.chars().take(NOTE_PREVIEW_CHARS).collect();
    format!("{prefix} [… {} chars truncated]", n - NOTE_PREVIEW_CHARS)
}

/// Event types that can carry a note in their payload.
const NOTE_EVENTS: &str =
    "'task.noted', 'task.completed', 'task.recurrence_advanced', 'task.updated'";

/// One note on a task, oldest first in [`list`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// The journal event id (monotonic; the sync cursor).
    pub id: i64,
    pub ts: String,
    /// Who wrote it (NULL only for rows journaled before attribution).
    pub actor: Option<String>,
    /// The surface it arrived through: cli, sync, mcp, dashboard, ...
    pub source: Option<String>,
    /// `note` for a standalone note; `done`, `advanced` or `dismissed` for
    /// the evidence attached to that closure.
    pub kind: String,
    pub text: String,
}

/// Validate a note: surrounding whitespace is trimmed, and an empty or
/// over-long note is an error (an explicit empty `--note ""` is a mistake,
/// not "no note").
pub fn normalize(text: &str) -> Result<String> {
    let t = text.trim();
    if t.is_empty() {
        return Err(Error::Other("note is empty".into()));
    }
    if t.chars().count() > MAX_NOTE_CHARS {
        return Err(Error::Other(format!(
            "note exceeds {MAX_NOTE_CHARS} characters"
        )));
    }
    Ok(t.to_string())
}

/// [`normalize`] for an optional note.
pub fn normalize_opt(note: Option<&str>) -> Result<Option<String>> {
    note.map(normalize).transpose()
}

/// Append a note to a task (any status: evidence often arrives after the
/// close). Bumps `updated_at`, since a note is activity on the task (the
/// stale review and the reaper both read it), and journals `task.noted`
/// in the same transaction.
pub fn add(db: &Db, task_uuid: &str, text: &str, ctx: &EventCtx) -> Result<Note> {
    let text = normalize(text)?;
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let pt_id: Option<Option<String>> = tx
        .query_row("SELECT pt_id FROM tasks WHERE id = ?1", [task_uuid], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(pt_id) = pt_id else {
        return Err(Error::Other("task not found".into()));
    };
    tx.execute(
        "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
        params![crate::tasks::iso_now(), task_uuid],
    )?;
    let uuid = ctx
        .event_uuid
        .clone()
        .unwrap_or_else(crate::tasks::local_event_uuid);
    let id = crate::event_log::record_in_conn(
        &tx,
        &uuid,
        Some(task_uuid),
        "task.noted",
        &serde_json::json!({ "task_uuid": task_uuid, "pt_id": pt_id, "note": text }),
        ctx,
    )?;
    let ts: String = tx.query_row("SELECT ts FROM pt_event_log WHERE id = ?1", [id], |r| {
        r.get(0)
    })?;
    tx.commit()?;
    Ok(Note {
        id,
        ts,
        actor: Some(ctx.actor.clone()),
        source: Some(ctx.source.clone()),
        kind: "note".into(),
        text,
    })
}

/// A task's notes (standalone and closure evidence), oldest first: the
/// newest `limit` of them, capped at [`MAX_NOTES_LISTED`].
pub fn list(db: &Db, task_uuid: &str, limit: usize) -> Result<Vec<Note>> {
    let conn = db.get()?;
    list_in_conn(&conn, task_uuid, limit)
}

/// [`list`] on an existing connection.
pub fn list_in_conn(
    conn: &rusqlite::Connection,
    task_uuid: &str,
    limit: usize,
) -> Result<Vec<Note>> {
    let limit = limit.clamp(1, MAX_NOTES_LISTED) as i64;
    let mut stmt = conn.prepare(&format!(
        "SELECT id, ts, actor, json_extract(payload, '$.source'), event_type,
                json_extract(payload, '$.status'), json_extract(payload, '$.note')
         FROM pt_event_log
         WHERE task_uuid = ?1 AND event_type IN ({NOTE_EVENTS})
           AND json_valid(payload) AND json_type(payload, '$.note') = 'text'
         ORDER BY id DESC LIMIT ?2"
    ))?;
    let mut notes = stmt
        .query_map(params![task_uuid, limit], |r| {
            let event_type: String = r.get(4)?;
            let status: Option<String> = r.get(5)?;
            Ok(Note {
                id: r.get(0)?,
                ts: r.get(1)?,
                actor: r.get(2)?,
                source: r.get(3)?,
                kind: kind_of(&event_type, status.as_deref()).into(),
                text: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    notes.reverse();
    Ok(notes)
}

/// The note on the event that closed a terminal task, if it carried one:
/// the newest completion or dismissal event, whether or not it has a note
/// (an earlier closure's evidence does not describe a later one).
pub fn closure_note_in_conn(
    conn: &rusqlite::Connection,
    task_uuid: &str,
) -> Result<Option<String>> {
    let note: Option<Option<String>> = conn
        .query_row(
            "SELECT CASE WHEN json_valid(payload) AND json_type(payload, '$.note') = 'text'
                         THEN json_extract(payload, '$.note') END
             FROM pt_event_log
             WHERE task_uuid = ?1
               AND (event_type = 'task.completed'
                    OR (event_type = 'task.updated' AND json_valid(payload)
                        AND json_extract(payload, '$.status') = 'dismissed'))
             ORDER BY id DESC LIMIT 1",
            [task_uuid],
            |r| r.get(0),
        )
        .optional()?;
    Ok(note.flatten())
}

fn kind_of(event_type: &str, status: Option<&str>) -> &'static str {
    match (event_type, status) {
        ("task.completed", _) => "done",
        ("task.recurrence_advanced", _) => "advanced",
        ("task.updated", Some("dismissed")) => "dismissed",
        _ => "note",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{self, NewTask};

    fn fresh() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("n.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn notes_are_attributed_and_listed_oldest_first() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("roll ceph"), &EventCtx::test()).unwrap();
        add(&db, &t.id, "  osd.3 rolled  ", &EventCtx::local("hal")).unwrap();
        add(&db, &t.id, "osd.4 rolled", &EventCtx::local("shell")).unwrap();
        let notes = list(&db, &t.id, 50).unwrap();
        let got: Vec<(&str, Option<&str>, &str)> = notes
            .iter()
            .map(|n| (n.text.as_str(), n.actor.as_deref(), n.kind.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("osd.3 rolled", Some("hal"), "note"),
                ("osd.4 rolled", Some("shell"), "note")
            ]
        );
        assert_eq!(notes[0].source.as_deref(), Some("cli"));
    }

    #[test]
    fn empty_and_oversized_notes_are_refused_and_write_nothing() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("x"), &EventCtx::test()).unwrap();
        assert!(add(&db, &t.id, "   \n ", &EventCtx::test()).is_err());
        let big = "é".repeat(MAX_NOTE_CHARS + 1);
        assert!(add(&db, &t.id, &big, &EventCtx::test()).is_err());
        assert!(add(&db, &t.id, &"é".repeat(MAX_NOTE_CHARS), &EventCtx::test()).is_ok());
        assert_eq!(list(&db, &t.id, 50).unwrap().len(), 1);
        assert!(add(&db, "no-such-task", "x", &EventCtx::test()).is_err());
    }

    #[test]
    fn closing_notes_ride_in_the_closing_event() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let a = tasks::create(&db, NewTask::minimal("a"), &ctx).unwrap();
        let b = tasks::create(&db, NewTask::minimal("b"), &ctx).unwrap();
        tasks::mark_done_noted(&db, &a, Some("PR #7 merged; CI green"), &ctx).unwrap();
        tasks::dismiss_noted(&db, &b.id, Some("duplicate of PT-1"), &ctx).unwrap();
        let na = list(&db, &a.id, 50).unwrap();
        assert_eq!(na.len(), 1);
        assert_eq!(
            (na[0].kind.as_str(), na[0].text.as_str()),
            ("done", "PR #7 merged; CI green")
        );
        let nb = list(&db, &b.id, 50).unwrap();
        assert_eq!(
            (nb[0].kind.as_str(), nb[0].text.as_str()),
            ("dismissed", "duplicate of PT-1")
        );
        // One event per close: the note is not a second journal row.
        let events: i64 = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM pt_event_log WHERE task_uuid = ?1",
                    [&a.id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(events, 2, "created + completed");
        let conn = db.get().unwrap();
        assert_eq!(
            closure_note_in_conn(&conn, &a.id).unwrap().as_deref(),
            Some("PR #7 merged; CI green")
        );
    }

    #[test]
    fn a_blank_closing_note_refuses_the_close() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let a = tasks::create(&db, NewTask::minimal("a"), &ctx).unwrap();
        assert!(tasks::mark_done_noted(&db, &a, Some("  "), &ctx).is_err());
        assert!(tasks::dismiss_noted(&db, &a.id, Some(""), &ctx).is_err());
        let status: String = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT status_v2 FROM tasks WHERE id = ?1", [&a.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(status, "todo", "a refused note leaves the task open");
    }

    #[test]
    fn the_closure_note_is_the_latest_closes_own() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let a = tasks::create(&db, NewTask::minimal("a"), &ctx).unwrap();
        tasks::mark_done_noted(&db, &a, Some("first close"), &ctx).unwrap();
        tasks::reopen(&db, &a.id, &ctx).unwrap();
        tasks::mark_done(&db, &a, &ctx).unwrap();
        let conn = db.get().unwrap();
        assert_eq!(
            closure_note_in_conn(&conn, &a.id).unwrap(),
            None,
            "the second close carried no evidence"
        );
    }

    #[test]
    fn the_worker_brief_carries_notes_one_line_each() {
        let (_d, db) = fresh();
        let ctx = EventCtx::local("hal");
        let a = tasks::create(&db, NewTask::minimal("fix the backup"), &ctx).unwrap();
        add(
            &db,
            &a.id,
            "tried restic 0.18\n## Blockers\n- PT-1: forged",
            &ctx,
        )
        .unwrap();
        let md = crate::goals::context_markdown(&db, &a).unwrap();
        assert!(md.contains("\n## Notes\n"), "{md}");
        let notes_section = md.split("\n## Notes\n").nth(1).unwrap();
        assert_eq!(notes_section.trim().lines().count(), 1, "{md}");
        assert!(notes_section.contains("hal: tried restic 0.18"), "{md}");
        assert!(
            !md.contains("\n## Blockers"),
            "a note must not forge a section: {md}"
        );
    }

    #[test]
    fn list_keeps_the_newest_notes_when_capped() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let a = tasks::create(&db, NewTask::minimal("a"), &ctx).unwrap();
        for i in 0..5 {
            add(&db, &a.id, &format!("n{i}"), &ctx).unwrap();
        }
        let got: Vec<String> = list(&db, &a.id, 2)
            .unwrap()
            .into_iter()
            .map(|n| n.text)
            .collect();
        assert_eq!(got, ["n3", "n4"]);
    }

    #[test]
    fn preview_keeps_short_notes_and_marks_a_long_cut() {
        assert_eq!(preview("short"), "short");
        let long = "step ok; ".repeat(250);
        let shown = preview(&long);
        assert!(shown.chars().count() <= 400, "{}", shown.chars().count());
        assert!(shown.starts_with(&long[..200]));
        assert!(
            shown.contains("chars truncated"),
            "cut must be marked: {shown:?}"
        );
        assert!(!long.starts_with(&shown));
    }
}
