//! CLI surface for the operator approval inbox.

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use ptask_core::approvals::{self, DecidedVia, Decision, PayloadSource, RequestInput};
use ptask_core::event_log::EventCtx;
use ptask_core::{Config, Db};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use crate::ui;

/// Distinct process exit for verify/consume (3..=6) and decide-guard
/// failures. `main` maps this to `std::process::exit` without treating it
/// as a generic anyhow error.
#[derive(Debug)]
pub struct ExitCodeError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for ExitCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ExitCodeError {}

pub fn exit_code(err: &anyhow::Error) -> Option<i32> {
    err.downcast_ref::<ExitCodeError>().map(|e| e.code)
}

#[derive(Subcommand, Debug)]
pub enum ApprovalCommand {
    /// Request operator approval for an exact payload.
    Request(RequestArgs),
    /// List approvals (default: pending, oldest first).
    #[command(alias = "ls")]
    List(ListArgs),
    /// Show one approval, including journal events.
    Show(IdArgs),
    /// Write the stored payload bytes to stdout.
    Payload(IdArgs),
    /// Withdraw a pending request (requester only).
    Withdraw(IdArgs),
    /// Check that an approval is approved and the payload still matches.
    Verify(VerifyArgs),
    /// Verify and latch the approval as consumed (exactly once).
    Consume(VerifyArgs),
    /// Mark pending rows past expires_at as expired.
    Expire,
    /// Send Telegram pings for pending un-notified requests.
    Notify,
    /// Approve or reject a pending request (operator only).
    Decide(LongDecideArgs),
}

#[derive(Args, Debug)]
#[command(group(
    clap::ArgGroup::new("payload_src")
        .required(true)
        .args(["payload_file", "payload_json", "digest"])
))]
pub struct RequestArgs {
    /// email | ebay | spend | destroy | external | budget | other
    #[arg(long)]
    pub kind: String,
    /// Short operator-facing title.
    #[arg(long)]
    pub title: String,
    /// Requester's note (always labelled as such; never mixed into preview).
    #[arg(long)]
    pub note: Option<String>,
    /// Read the requester's note from a file.
    #[arg(long = "note-file")]
    pub note_file: Option<PathBuf>,
    /// Store this file as the bound payload (max 256 KiB).
    #[arg(long = "payload-file")]
    pub payload_file: Option<PathBuf>,
    /// Store canonical JSON as the bound payload.
    #[arg(long = "payload-json")]
    pub payload_json: Option<String>,
    /// Bind to a digest without storing the payload.
    #[arg(long)]
    pub digest: Option<String>,
    /// Optional linked task (PT-n).
    #[arg(long)]
    pub task: Option<String>,
    /// Auto-expire after this duration, e.g. 1s, 15m, 2h, 7d.
    #[arg(long = "expires-in")]
    pub expires_in: Option<String>,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// pending (default) | approved | rejected | withdrawn | expired | all
    #[arg(long = "status", default_value = "pending")]
    pub status: String,
}

#[derive(Args, Debug)]
pub struct IdArgs {
    /// AP-n (or the row uuid).
    pub id: String,
}

#[derive(Args, Debug)]
#[command(group(
    clap::ArgGroup::new("payload_src")
        .required(true)
        .args(["payload_file", "payload_json", "digest"])
))]
pub struct VerifyArgs {
    /// AP-n (or the row uuid).
    pub id: String,
    #[arg(long = "payload-file")]
    pub payload_file: Option<PathBuf>,
    #[arg(long = "payload-json")]
    pub payload_json: Option<String>,
    #[arg(long)]
    pub digest: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ViaChoice {
    Dashboard,
}

#[derive(Args, Debug)]
pub struct DecideArgs {
    /// AP-n (or the row uuid).
    pub id: String,
    #[arg(long)]
    pub note: Option<String>,
    /// Record decided_via=dashboard (also allows a non-TTY stdin).
    #[arg(long = "via")]
    pub via: Option<ViaChoice>,
}

#[derive(Args, Debug)]
pub struct LongDecideArgs {
    /// AP-n (or the row uuid).
    pub id: String,
    /// approve | reject
    pub decision: String,
    #[arg(long)]
    pub note: Option<String>,
    #[arg(long = "via")]
    pub via: Option<ViaChoice>,
}

fn map_core(err: ptask_core::Error) -> anyhow::Error {
    if let ptask_core::Error::Approval(ae) = &err
        && let Some(code) = ae.verify_exit_code()
    {
        return ExitCodeError {
            code,
            message: ae.to_string(),
        }
        .into();
    }
    err.into()
}

fn payload_source(
    file: Option<&PathBuf>,
    json: Option<&str>,
    digest: Option<&str>,
) -> Result<PayloadSource> {
    match (file, json, digest) {
        (Some(p), None, None) => approvals::payload_from_file(p).map_err(map_core),
        (None, Some(j), None) => approvals::payload_from_json_str(j).map_err(map_core),
        (None, None, Some(d)) => approvals::payload_from_digest(d).map_err(map_core),
        _ => Err(ExitCodeError {
            code: 1,
            message: "exactly one of --payload-file, --payload-json, --digest is required".into(),
        }
        .into()),
    }
}

fn note_text(note: Option<&str>, note_file: Option<&PathBuf>) -> Result<Option<String>> {
    match (note, note_file) {
        (Some(_), Some(_)) => anyhow::bail!("pass only one of --note or --note-file"),
        (Some(n), None) => Ok(Some(n.to_string())),
        (None, Some(p)) => {
            Ok(Some(std::fs::read_to_string(p).with_context(|| {
                format!("reading note file {}", p.display())
            })?))
        }
        (None, None) => Ok(None),
    }
}

fn notify_one(db: &Db, ap: &approvals::Approval) {
    let cfg = Config::from_env();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return,
    };
    let _ = rt.block_on(ptask_notify::notify_approval(
        db,
        &cfg.notify,
        cfg.dash.url.as_deref(),
        cfg.tg_approval_buttons,
        ap,
    ));
}

fn print_human(ap: &approvals::Approval, events: Option<&[approvals::ApprovalEvent]>) {
    println!(
        "{}",
        ui::outcome(
            match ap.status.as_str() {
                "approved" => ui::Status::Ok,
                "rejected" | "expired" | "withdrawn" => ui::Status::Bad,
                _ => ui::Status::Busy,
            },
            &ap.status,
            &ap.ap_id(),
            &ap.title,
            &ap.kind
        )
    );
    let mut pairs: Vec<(&str, String)> = vec![
        ("requester", ap.requester.clone()),
        ("digest", ap.digest.clone()),
        (
            "payload",
            if ap.payload_stored() {
                format!(
                    "{} ({} bytes)",
                    ap.payload_kind.as_deref().unwrap_or("file"),
                    ap.payload_bytes.unwrap_or(0)
                )
            } else {
                "not stored".into()
            },
        ),
    ];
    if let Some(t) = &ap.task_pt_id {
        pairs.push(("task", t.clone()));
    }
    if let Some(n) = &ap.request_note {
        pairs.push(("requester note", n.clone()));
    }
    if let Some(d) = &ap.decided_by {
        pairs.push(("decided by", d.clone()));
    }
    for l in ui::kv(&pairs, 16) {
        println!("    {}", l.trim_start());
    }
    println!();
    println!(
        "{}",
        ui::section(
            "preview",
            ui::Ink::Steel,
            "rendered from the stored payload"
        )
    );
    println!("{}", ap.preview());
    if let Some(events) = events
        && !events.is_empty()
    {
        println!();
        println!("{}", ui::section("events", ui::Ink::Steel, "journal"));
        for e in events {
            println!(
                "  {}  {}  {}",
                e.at,
                e.event_type,
                e.actor.as_deref().unwrap_or("-")
            );
        }
    }
}

fn decide_guardrails(via_dashboard: bool) -> Result<DecidedVia> {
    let claudecode = std::env::var("CLAUDECODE")
        .ok()
        .is_some_and(|s| !s.is_empty());
    if claudecode {
        return Err(ExitCodeError {
            code: 1,
            message: "approval decisions are reserved for the operator".into(),
        }
        .into());
    }
    if !via_dashboard && !std::io::stdin().is_terminal() {
        return Err(ExitCodeError {
            code: 1,
            message: "approval decisions require an operator TTY or --via dashboard".into(),
        }
        .into());
    }
    Ok(if via_dashboard {
        DecidedVia::Dashboard
    } else {
        DecidedVia::Cli
    })
}

pub fn cmd_request(db: &Db, a: RequestArgs, ctx: EventCtx, json: bool) -> Result<()> {
    if a.note.is_some() && a.note_file.is_some() {
        anyhow::bail!("pass only one of --note or --note-file");
    }
    let payload = payload_source(
        a.payload_file.as_ref(),
        a.payload_json.as_deref(),
        a.digest.as_deref(),
    )?;
    let input = RequestInput {
        kind: a.kind,
        title: a.title,
        request_note: note_text(a.note.as_deref(), a.note_file.as_ref())?,
        payload,
        task_pt_id: a.task,
        expires_in: a.expires_in,
    };
    let outcome = approvals::request(db, input, &ctx).map_err(map_core)?;
    if outcome.created {
        notify_one(db, &outcome.approval);
    }
    let ap = approvals::get(db, &outcome.approval.uuid).unwrap_or(outcome.approval);
    emit_one(ap, None, json)
}

pub fn cmd_list(db: &Db, a: ListArgs, json: bool) -> Result<()> {
    let items = approvals::list(db, Some(&a.status)).map_err(map_core)?;
    if json {
        let v: Vec<serde_json::Value> = items.iter().map(|ap| ap.to_json(None)).collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    print_lines_headline(&format!("approvals · {}", a.status), items.len());
    if items.is_empty() {
        println!("{}", ui::empty("no approvals"));
        return Ok(());
    }
    for ap in &items {
        println!(
            "{}",
            ui::outcome(
                ui::Status::Busy,
                &ap.status,
                &ap.ap_id(),
                &ap.title,
                &ap.kind
            )
        );
    }
    Ok(())
}

pub fn cmd_show(db: &Db, a: IdArgs, json: bool) -> Result<()> {
    let ap = approvals::get(db, &a.id).map_err(map_core)?;
    let events = approvals::events(db, &ap.uuid).map_err(map_core)?;
    emit_one(ap, Some(&events), json)
}

pub fn cmd_payload(db: &Db, a: IdArgs) -> Result<()> {
    let bytes = approvals::payload_bytes(db, &a.id).map_err(map_core)?;
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)?;
    Ok(())
}

pub fn cmd_withdraw(db: &Db, a: IdArgs, ctx: EventCtx, json: bool) -> Result<()> {
    let ap = approvals::withdraw(db, &a.id, &ctx).map_err(map_core)?;
    emit_one(ap, None, json)
}

pub fn cmd_verify(db: &Db, a: VerifyArgs) -> Result<()> {
    let src = payload_source(
        a.payload_file.as_ref(),
        a.payload_json.as_deref(),
        a.digest.as_deref(),
    )?;
    approvals::verify(db, &a.id, &src).map_err(map_core)?;
    Ok(())
}

pub fn cmd_consume(db: &Db, a: VerifyArgs, ctx: EventCtx) -> Result<()> {
    let src = payload_source(
        a.payload_file.as_ref(),
        a.payload_json.as_deref(),
        a.digest.as_deref(),
    )?;
    approvals::consume(db, &a.id, &src, &ctx).map_err(map_core)?;
    Ok(())
}

pub fn cmd_expire(db: &Db, ctx: EventCtx) -> Result<()> {
    let n = approvals::expire(db, &ctx).map_err(map_core)?;
    println!(
        "{}",
        ui::outcome(
            ui::Status::Ok,
            "expired",
            &format!("{n}"),
            "pending past expires_at",
            ""
        )
    );
    Ok(())
}

pub fn cmd_notify(db: &Db) -> Result<()> {
    let cfg = Config::from_env();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("notify runtime")?;
    let n = rt.block_on(ptask_notify::notify_pending(
        db,
        &cfg.notify,
        cfg.dash.url.as_deref(),
        cfg.tg_approval_buttons,
    ))?;
    println!(
        "{}",
        ui::outcome(
            ui::Status::Ok,
            "notified",
            &format!("{n}"),
            "pending approvals",
            ""
        )
    );
    Ok(())
}

pub fn cmd_decide(
    db: &Db,
    id: &str,
    decision: Decision,
    note: Option<&str>,
    via: Option<ViaChoice>,
    ctx: EventCtx,
    json: bool,
) -> Result<()> {
    let via = decide_guardrails(matches!(via, Some(ViaChoice::Dashboard)))?;
    let ap = approvals::decide(db, id, decision, via, note, &ctx).map_err(map_core)?;
    emit_one(ap, None, json)
}

pub fn cmd_long_decide(db: &Db, a: LongDecideArgs, ctx: EventCtx, json: bool) -> Result<()> {
    let decision = Decision::parse(&a.decision)
        .map_err(ptask_core::Error::from)
        .map_err(map_core)?;
    cmd_decide(db, &a.id, decision, a.note.as_deref(), a.via, ctx, json)
}

pub async fn notify_pending_async(db: &Db) -> ptask_core::Result<usize> {
    let cfg = Config::from_env();
    ptask_notify::notify_pending(
        db,
        &cfg.notify,
        cfg.dash.url.as_deref(),
        cfg.tg_approval_buttons,
    )
    .await
}

fn emit_one(
    ap: approvals::Approval,
    events: Option<&[approvals::ApprovalEvent]>,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&ap.to_json(events))?);
    } else {
        print_human(&ap, events);
    }
    Ok(())
}

fn print_lines_headline(title: &str, n: usize) {
    print_lines(ui::headline(
        "ptask · approvals",
        None,
        &format!("{title} · {n}"),
    ));
}

fn print_lines(lines: Vec<String>) {
    for l in lines {
        println!("{l}");
    }
}
