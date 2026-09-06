//! `pt` — pTask command-line interface.
//!
//! v0.1.0 covers parity with the legacy Python `cli.py`:
//!   pt add <title> [-p PRIORITY] [-d DESCRIPTION] [--deadline ISO] [--reason TEXT]
//!   pt list        [-s STATUS]   [-p PRIORITY]    [-n LIMIT]       [-v]
//!   pt done <query>
//!
//! Later phases added `show`, `dismiss`, `rm`, `reopen`, richer `edit`,
//! `serve`, `bot`, `tui`, and the `remote` mutation/read verbs.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ptask_core::{Db, Extensions, NewTask, dag, priority, pt_id, quickadd, tasks, views};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

mod remote;
mod ui;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Parser, Debug)]
#[command(
    name = "pt",
    version = ptask_core::VERSION,
    about = "Sovereign task manager for PureTensor",
    long_about = None,
)]
struct Cli {
    /// Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db).
    #[arg(long, env = "PTASK_DB", global = true)]
    db: Option<String>,

    /// Emit machine-readable JSON instead of human text (task-facing verbs).
    #[arg(long, global = true)]
    json: bool,

    /// Idempotency key recorded with the mutation's event — a retried
    /// command with the same key returns ok without re-applying.
    #[arg(long = "idempotency-key", global = true)]
    idempotency_key: Option<String>,

    /// Colour: auto (TTY only; honours NO_COLOR and PT_COLOR=always|never), always, or never.
    #[arg(long = "color", global = true, value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,

    /// Plain output — shorthand for `--color never`.
    #[arg(long = "no-color", global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a new task.
    Add(AddArgs),
    /// List tasks.
    #[command(alias = "ls")]
    List(ListArgs),
    /// Mark a task done (by PT-N or title substring).
    Done(DoneArgs),
    /// Promote/demote a task's priority (critical|urgent|high|normal|low or 1..=5).
    #[command(alias = "pri")]
    Priority(PriorityArgs),
    /// Edit task fields.
    #[command(alias = "update")]
    Edit(EditArgs),
    /// Reopen a completed/dismissed task (status → pending).
    Reopen(ReopenArgs),
    /// Show one task's full row + side-table detail.
    Show(ShowArgs),
    /// Dismiss a task (soft close, status → dismissed; reversible via reopen).
    Dismiss(DismissArgs),
    /// Delete a task permanently (hard delete + tombstone).
    Rm(RmArgs),
    /// Show ready-to-start tasks (all dependencies done).
    Next(NextArgs),
    /// Advisory day plan: fit the ready queue into calendar free/busy (dry-run
    /// unless --write). Reads free slots via gcalendar.py; --write adds
    /// tentative events to our own calendar only.
    Plan(PlanArgs),
    /// Manage saved views.
    #[command(subcommand)]
    View(ViewCommand),
    /// Launch the terminal UI (ratatui).
    Tui,
    /// Run the HTTP server (sync API, capture, webhooks, metrics).
    Serve(ServeArgs),
    /// Run the Telegram bot (Bot API long-poll).
    Bot,
    /// Serve the MCP server over stdio (agent-native tool surface).
    Mcp,
    /// Session-priming digest: recent done/dismissed/created + ready queue.
    Digest(DigestArgs),
    /// Export tasks/links/labels as JSONL (optionally git-commit the export).
    Export(ExportArgs),
    /// Print the operator-gated delegation command for a task (skeleton).
    Delegate(DelegateArgs),
    /// Print a Linear-style branch name for a task.
    Branch(BranchArgs),
    /// Run the native Rust distillation pipeline.
    Distill(DistillArgs),
    /// Run one accountability cycle (escalation + Telegram/email).
    #[command(subcommand)]
    Accountability(AccountabilityCommand),
    /// Recompute composite priority scores for all active tasks.
    #[command(subcommand)]
    Scoring(ScoringCommand),
    /// Talk to a remote canonical `pt serve` (no local DB).
    #[command(subcommand)]
    Remote(RemoteCommand),
    /// Promote an investigation to implementation: kind scout → ship on the
    /// SAME row (never close + reopen a duplicate).
    Promote(StartArgs),
    /// Set a task's kind (scout|ship) and optional deliverable.
    Kind(KindArgs),
    /// Mark a task in progress (you're actively working it).
    Start(StartArgs),
    /// Snooze a task until a date — it leaves `pt next` and reminders,
    /// then wakes to todo automatically.
    Snooze(SnoozeArgs),
    /// Reap stale machine-generated tasks (incident >7d idle, distilled
    /// >30d idle) — soft-dismiss, reversible via `pt reopen`.
    Reap(ReapArgs),
    /// Manage dependency edges: PT-A depends on PT-B.
    Depend(DependArgs),
    /// Interactive review sweep: stale, snoozed-expired, and triage items.
    Review(ReviewArgs),
    /// Full-text search over titles + descriptions (FTS5).
    Search(SearchArgs),
    /// Explain a task's composite score: components, weights, rank.
    Why(WhyArgs),
    /// Apply one action to every task matching a filter DSL expression.
    Bulk(BulkArgs),
    /// Show a task's attributed event history (who did what, via which surface).
    Log(LogArgs),
    /// Reverse the most recent undoable mutation (done/dismiss/create).
    Undo,
    /// Manage named scoped API tokens (create/list/revoke).
    #[command(subcommand)]
    Token(TokenCommand),
    /// One-shot backfill PT-N for any tasks lacking one.
    Backfill,
    /// Generate the `pt(1)` manpage to stdout.
    GenManpage,
    /// Generate shell completions (bash/zsh/fish) to stdout.
    GenCompletions(GenCompletionsArgs),
}

#[derive(Subcommand, Debug)]
enum AccountabilityCommand {
    /// Run the state machine + dispatch once.
    Run(AccountabilityRunArgs),
}

#[derive(clap::Args, Debug)]
struct DigestArgs {
    /// Lookback window in days.
    #[arg(long, default_value_t = 7)]
    days: i64,
}

#[derive(clap::Args, Debug)]
struct ExportArgs {
    /// Output directory (default ~/puretensor-tasks/export).
    #[arg(long)]
    out: Option<std::path::PathBuf>,
    /// Commit the export in-place (init a repo on first run).
    #[arg(long)]
    git: bool,
}

#[derive(clap::Args, Debug)]
struct DelegateArgs {
    /// Task handle (PT-N / uuid / title substring).
    id: String,
}

#[derive(clap::Args, Debug)]
struct AccountabilityRunArgs {
    /// Don't actually send anything; log what would have been dispatched.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(clap::Args, Debug)]
struct StartArgs {
    /// PT-N, bare integer, or title substring.
    query: String,
}

#[derive(clap::Args, Debug)]
struct SnoozeArgs {
    /// PT-N, bare integer, or title substring.
    query: String,
    /// Wake date/time: ISO or natural ("tomorrow 9am", "next monday").
    until: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct ReapArgs {
    /// List what would be dismissed without touching anything.
    #[arg(long = "dry-run")]
    dry_run: bool,
    /// Emit the report as JSON (machine callers / timers).
    #[arg(long = "json")]
    json: bool,
}

#[derive(clap::Args, Debug)]
struct DependArgs {
    /// The dependent task (cannot start until --on is done).
    query: String,
    /// The prerequisite task.
    #[arg(long = "on")]
    on: Option<String>,
    /// Remove the edge instead of adding it.
    #[arg(long = "clear", requires = "on")]
    clear: bool,
}

#[derive(clap::Args, Debug)]
struct ReviewArgs {
    /// Days of inactivity that makes a task "stale".
    #[arg(long = "stale-days", default_value_t = 14)]
    stale_days: i64,
}

#[derive(clap::Args, Debug)]
struct WhyArgs {
    /// PT-N, bare integer, or title substring.
    query: String,
}

#[derive(clap::Args, Debug)]
struct SearchArgs {
    /// FTS5 query (words, phrases, AND/OR/NOT).
    query: Vec<String>,
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
}

#[derive(clap::Args, Debug)]
struct BulkArgs {
    /// Filter DSL selecting the tasks (same grammar as `pt list`).
    filter: String,
    /// Set priority on every match.
    #[arg(long = "set-priority")]
    set_priority: Option<String>,
    /// Mark every match done.
    #[arg(long = "done", conflicts_with = "set_priority")]
    done: bool,
    /// Dismiss every match.
    #[arg(long = "dismiss", conflicts_with_all = ["set_priority", "done"])]
    dismiss: bool,
    /// Preview without applying.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(clap::Args, Debug)]
struct LogArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// Max events to show (newest first).
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
}

#[derive(Subcommand, Debug)]
enum TokenCommand {
    /// Mint a token for a client. Prints the plain token ONCE — store it
    /// with the consumer; only its hash is kept.
    Create(TokenCreateArgs),
    /// List all tokens (client, scope, created/last-used/revoked).
    List,
    /// Revoke every active token for a client id.
    Revoke(TokenRevokeArgs),
}

#[derive(clap::Args, Debug)]
struct TokenCreateArgs {
    /// Stable client identity: hal, puresentinel, nexus, dashboard, shell-<host>…
    client_id: String,
    /// Scope: read | capture | write | admin (each implies the previous).
    #[arg(long = "scope", default_value = "write")]
    scope: String,
}

#[derive(clap::Args, Debug)]
struct TokenRevokeArgs {
    client_id: String,
}

#[derive(Subcommand, Debug)]
enum ScoringCommand {
    /// Recompute the four score_* columns + priority_score for every
    /// task with status NOT IN ('done', 'dismissed').
    Run(ScoringRunArgs),
}

#[derive(Subcommand, Debug)]
enum RemoteCommand {
    /// `pt remote add "..."` — create a task on the canonical host
    /// without opening a local DB. Uses PTASK_SYNC_URL (default
    /// http://127.0.0.1:9501).
    Add(RemoteAddArgs),
    /// `pt remote list` — fetch the live task set from the canonical host.
    #[command(alias = "ls")]
    List(RemoteListArgs),
    /// `pt remote done <query>` — mark a task done by PT-N or title substring.
    Done(RemoteDoneArgs),
    /// `pt remote priority <query> <level>` — set priority on the canonical host.
    #[command(alias = "pri")]
    Priority(RemotePriorityArgs),
    /// `pt remote edit <query> --deadline <iso> | --clear-deadline`.
    #[command(alias = "update")]
    Edit(RemoteEditArgs),
    /// `pt remote reopen <query>` — flip a done/dismissed task back to pending.
    Reopen(RemoteReopenArgs),
    /// `pt remote show <query>` — print one task's full row + detail (read-only).
    Show(RemoteShowArgs),
    /// `pt remote next [-n N]` — DAG-ready tasks from the canonical host.
    Next(RemoteNextArgs),
    /// `pt remote dismiss <query>` — soft-close a task (reversible via reopen).
    Dismiss(RemoteDismissArgs),
    /// `pt remote start <query>` — mark in progress on the canonical host.
    Start(RemoteDoneArgs),
    /// `pt remote snooze <query> <until>` — snooze on the canonical host.
    Snooze(RemoteSnoozeArgs),
    /// `pt remote depend <query> --on <target> [--clear]`.
    Depend(RemoteDependArgs),
    /// `pt remote rm <query>` — permanent delete (tombstoned).
    Rm(RemoteDoneArgs),
    /// `pt remote version` — compare this client's version against the
    /// canonical server's `GET /version`. Exits non-zero on skew.
    Version(RemoteVersionArgs),
}

#[derive(clap::Args, Debug)]
struct RemoteSnoozeArgs {
    query: String,
    /// Wake date/time (ISO or natural language, parsed locally).
    until: Vec<String>,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteDependArgs {
    query: String,
    #[arg(long = "on")]
    on: String,
    #[arg(long = "clear")]
    clear: bool,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteVersionArgs {
    /// Override the canonical endpoint.
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteAddArgs {
    /// Quick-add text. Same grammar as local `pt add`.
    text: String,
    /// Override the canonical endpoint.
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteListArgs {
    #[arg(short = 's', long = "status", default_value = "pending")]
    status: String,
    /// Filter DSL evaluated SERVER-side (GET /list).
    #[arg(short = 'f', long = "filter")]
    filter: Option<String>,
    #[arg(short = 'p', long = "priority")]
    priority: Option<String>,
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteDoneArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemotePriorityArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// New level: low|normal|high|urgent|critical or 1..=5.
    level: String,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteEditArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// Set deadline to an ISO date/datetime, e.g. 2026-06-30.
    #[arg(long = "deadline")]
    deadline: Option<String>,
    /// Clear the deadline.
    #[arg(long = "clear-deadline")]
    clear_deadline: bool,
    /// Replace the title.
    #[arg(long = "title")]
    title: Option<String>,
    /// Replace the description.
    #[arg(long = "desc")]
    desc: Option<String>,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteNextArgs {
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteReopenArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteDismissArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteShowArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ScoringRunArgs {
    /// Compute and log scores but don't write them back to the DB.
    #[arg(long = "dry-run")]
    dry_run: bool,
    /// Use the retired v1 formula (comparison escape hatch).
    #[arg(long = "v1")]
    v1: bool,
    /// Print an old-vs-new top-20 rank diff (implies --dry-run semantics
    /// for the comparison pass; final write still follows the chosen mode).
    #[arg(long = "diff")]
    diff: bool,
}

#[derive(clap::Args, Debug)]
struct DistillArgs {
    /// Max raw_items consumed per native run.
    #[arg(long = "batch", default_value_t = 200)]
    batch: usize,
}

#[derive(clap::Args, Debug)]
struct BranchArgs {
    /// PT-N (or bare integer, or title substring).
    query: String,
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
    /// Bind address. Default 127.0.0.1:9501 (leaves :9500 for legacy
    /// Python FastAPI during the parallel-ops window).
    #[arg(long = "bind")]
    bind: Option<String>,
}

#[derive(Subcommand, Debug)]
enum ViewCommand {
    /// Save a filter DSL string under a name.
    Save { name: String, filter: String },
    /// List saved views.
    #[command(alias = "ls")]
    List,
    /// Run a saved view's filter and print matching tasks.
    Show {
        name: String,
        /// Override row limit.
        #[arg(short = 'n', long = "limit", default_value_t = 20)]
        limit: usize,
    },
    /// Delete a saved view.
    Rm { name: String },
}

#[derive(clap::Args, Debug)]
struct NextArgs {
    /// Max ready tasks to show.
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
}

#[derive(clap::Args, Debug)]
struct PlanArgs {
    /// gcalendar.py account (free/busy source).
    #[arg(long, default_value = "ops")]
    account: String,
    /// Planning horizon in days.
    #[arg(long, default_value_t = 1)]
    days: i64,
    /// Working hours HH:MM-HH:MM.
    #[arg(long, default_value = "09:00-18:00")]
    work: String,
    /// Timezone.
    #[arg(long, default_value = "Europe/London")]
    tz: String,
    /// Calendar id.
    #[arg(long, default_value = "primary")]
    calendar: String,
    /// Default minutes for a task with no duration_min.
    #[arg(long, default_value_t = 30)]
    slot_default: i64,
    /// Max ready tasks to consider.
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
    /// Create tentative calendar events for the plan (our calendar only).
    #[arg(long)]
    write: bool,
    /// Path to gcalendar.py.
    #[arg(long, env = "PTASK_GCAL")]
    gcal: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct AddArgs {
    /// Task title (parsed as quick-add unless --raw is set).
    /// Inline tokens: @label, #project, p1..p5, ~30m/~2h/~1d, !HH:MM,
    /// //description (rest of string), date phrases (today/tomorrow/
    /// weekday with this|next|last/ N days/ISO dates).
    title: String,
    /// Priority override (low|normal|high|urgent|critical or 1..=5).
    /// If omitted, uses quick-add priority or "normal".
    #[arg(short = 'p', long = "priority")]
    priority: Option<String>,
    /// Description override.
    #[arg(short = 'd', long = "description")]
    description: Option<String>,
    /// Deadline override (ISO date, e.g. 2026-05-20).
    #[arg(long = "deadline")]
    deadline: Option<String>,
    /// Why this task was created — stored as ai_reasoning.
    #[arg(long = "reason")]
    reason: Option<String>,
    /// Disable quick-add parsing — treat the title literally.
    #[arg(long = "raw")]
    raw: bool,
    /// Task shape: scout (investigation) or ship (implementation, default).
    #[arg(long = "kind")]
    kind: Option<String>,
    /// What finishing it produces: report | pr | none.
    /// Defaults to the kind's deliverable when --kind is given.
    #[arg(long = "deliverable")]
    deliverable: Option<String>,
}

#[derive(clap::Args, Debug)]
struct KindArgs {
    /// PT-N, bare integer, or title substring.
    query: String,
    /// scout | ship.
    kind: String,
    /// report | pr | none. Left unchanged when omitted.
    #[arg(long = "deliverable")]
    deliverable: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ListArgs {
    /// Optional Todoist-style filter DSL.
    /// Examples: "today & p1", "(today | overdue) & #fleet",
    /// "@waiting & no date", "due before: next friday & !recurring",
    /// "search: ceph & @ops".
    filter: Option<String>,
    /// Filter by status (or `all`).
    #[arg(short = 's', long = "status", default_value = "pending")]
    status: String,
    /// Filter by priority.
    #[arg(short = 'p', long = "priority")]
    priority: Option<String>,
    /// Max rows.
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
    /// Show description and UUID.
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
    /// Order: severity (default, critical first), score (composite ranking),
    /// or created (newest first).
    #[arg(long = "sort", default_value = "severity")]
    sort: String,
}

#[derive(clap::Args, Debug)]
struct DoneArgs {
    /// One or more tasks: PT-N (e.g. PT-42), bare integer, or title substring.
    #[arg(required = true)]
    queries: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct PriorityArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// New level: low|normal|high|urgent|critical or 1..=5.
    level: String,
}

#[derive(clap::Args, Debug)]
struct EditArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// Set deadline to an ISO date/datetime, e.g. 2026-06-16.
    #[arg(long = "deadline")]
    deadline: Option<String>,
    /// Clear the deadline.
    #[arg(long = "clear-deadline")]
    clear_deadline: bool,
    /// Replace the title.
    #[arg(long = "title")]
    title: Option<String>,
    /// Replace the description.
    #[arg(long = "desc")]
    desc: Option<String>,
    /// Add a label (repeatable), e.g. --label domain:mgmt.
    #[arg(long = "label")]
    label: Vec<String>,
    /// Remove a label (repeatable).
    #[arg(long = "unlabel")]
    unlabel: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct ReopenArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
}

#[derive(clap::Args, Debug)]
struct ShowArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
}

#[derive(clap::Args, Debug)]
struct DismissArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
}

#[derive(clap::Args, Debug)]
struct RmArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long = "yes")]
    yes: bool,
}

#[derive(clap::Args, Debug)]
struct GenCompletionsArgs {
    /// Target shell.
    #[arg(value_enum)]
    shell: ShellChoice,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum ShellChoice {
    Bash,
    Zsh,
    Fish,
}

/// Process-wide CLI output/attribution overrides, set once at the
/// entrypoint from the parsed global flags. A `pt` process executes exactly
/// one command, so this is entrypoint-time config, not ambient state.
static CLI_JSON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static CLI_IDEMPOTENCY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

fn set_cli_globals(json: bool, idempotency_key: Option<String>) {
    let _ = CLI_JSON.set(json);
    let _ = CLI_IDEMPOTENCY.set(idempotency_key);
}

fn json_mode() -> bool {
    *CLI_JSON.get().unwrap_or(&false)
}

/// Print a value as pretty JSON when --json is set; otherwise run the
/// human-text closure.
fn emit<T: serde::Serialize>(value: &T, text: impl FnOnce()) -> Result<()> {
    if json_mode() {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        text();
    }
    Ok(())
}

/// Attribution for this CLI invocation: actor = $PTASK_ACTOR (the dashboard
/// sidecar sets "dashboard"; HAL sessions "hal"), default "shell"; the
/// --idempotency-key flag keys the event for safe retries.
fn cli_ctx() -> ptask_core::event_log::EventCtx {
    let mut ctx = ptask_core::event_log::EventCtx::local(ptask_core::Config::from_env().actor);
    if let Some(Some(key)) = CLI_IDEMPOTENCY.get().cloned() {
        ctx.event_uuid = Some(key);
    }
    ctx
}

fn main() {
    // A downstream `| head` closes the pipe early; the default Rust behaviour
    // is a panic on the next println!. Restore the Unix default (exit quietly).
    // SAFETY: setting a signal disposition before any thread exists.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = run() {
        eprintln!("{}", ui::section("error", ui::Ink::Red, &format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    set_cli_globals(cli.json, cli.idempotency_key.clone());
    ui::init(match cli.color {
        _ if cli.no_color || cli.json => ui::ColorMode::Never,
        ColorChoice::Auto => ui::ColorMode::Auto,
        ColorChoice::Always => ui::ColorMode::Always,
        ColorChoice::Never => ui::ColorMode::Never,
    });

    // Lightweight tracing: env-controlled, off by default.
    let filter = tracing_subscriber::EnvFilter::try_from_env("PTASK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let command = cli.command;

    // These commands are deliberately DB-free: they must work on fleet client
    // nodes before a local SQLite store exists. Keep them before Db::open_*().
    match command {
        Some(Command::Remote(c)) => cmd_remote(c),
        Some(Command::GenManpage) => cmd_gen_manpage(),
        Some(Command::GenCompletions(a)) => cmd_gen_completions(a),
        other => {
            let db = match cli.db.as_deref() {
                Some(p) => Db::open(p).with_context(|| format!("opening db at {}", p))?,
                None => Db::open_default().context("opening default db")?,
            };

            match other {
                Some(Command::Add(a)) => cmd_add(&db, a),
                Some(Command::List(a)) => cmd_list(&db, a),
                Some(Command::Done(a)) => cmd_done(&db, a),
                Some(Command::Priority(a)) => cmd_priority(&db, a),
                Some(Command::Edit(a)) => cmd_edit(&db, a),
                Some(Command::Reopen(a)) => cmd_reopen(&db, a),
                Some(Command::Show(a)) => cmd_show(&db, a),
                Some(Command::Dismiss(a)) => cmd_dismiss(&db, a),
                Some(Command::Rm(a)) => cmd_rm(&db, a),
                Some(Command::Next(a)) => cmd_next(&db, a),
                Some(Command::Plan(a)) => cmd_plan(&db, a),
                Some(Command::View(c)) => cmd_view(&db, c),
                Some(Command::Tui) => ptask_tui::run(db),
                Some(Command::Serve(a)) => cmd_serve(db, a),
                Some(Command::Bot) => cmd_bot(db),
                Some(Command::Mcp) => cmd_mcp(db),
                Some(Command::Digest(a)) => cmd_digest(&db, a),
                Some(Command::Export(a)) => cmd_export(&db, a),
                Some(Command::Delegate(a)) => cmd_delegate(&db, a),
                Some(Command::Branch(a)) => cmd_branch(&db, a),
                Some(Command::Distill(a)) => cmd_distill(&db, a),
                Some(Command::Accountability(c)) => cmd_accountability(db, c),
                Some(Command::Scoring(c)) => cmd_scoring(&db, c),
                Some(Command::Start(a)) => cmd_start(&db, a),
                Some(Command::Promote(a)) => cmd_promote(&db, a),
                Some(Command::Kind(a)) => cmd_kind(&db, a),
                Some(Command::Snooze(a)) => cmd_snooze(&db, a),
                Some(Command::Reap(a)) => cmd_reap(&db, a),
                Some(Command::Depend(a)) => cmd_depend(&db, a),
                Some(Command::Review(a)) => cmd_review(&db, a),
                Some(Command::Search(a)) => cmd_search(&db, a),
                Some(Command::Why(a)) => cmd_why(&db, a),
                Some(Command::Bulk(a)) => cmd_bulk(&db, a),
                Some(Command::Log(a)) => cmd_log(&db, a),
                Some(Command::Undo) => cmd_undo(&db),
                Some(Command::Token(c)) => cmd_token(&db, c),
                Some(Command::Backfill) => cmd_backfill(&db),
                None if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => {
                    ptask_tui::run(db)
                }
                None => {
                    // Non-interactive fallback: avoid trying to enter alt-screen when
                    // stdin/stdout is not a TTY. Interactive `pt` opens the TUI.
                    for l in ui::banner(
                        "ptask",
                        &format!(
                            "v{}   sovereign task manager   {}",
                            ptask_core::VERSION,
                            ui::utc_stamp()
                        ),
                        Some(("cli", ui::Ink::Cyan)),
                    ) {
                        println!("{l}");
                    }
                    println!(
                        "{}",
                        ui::note(
                            "pt tui · pt add \"...\" · pt list · pt next · pt done PT-N · pt --help"
                        )
                    );
                    Ok(())
                }
                Some(Command::Remote(_))
                | Some(Command::GenManpage)
                | Some(Command::GenCompletions(_)) => unreachable!("handled before DB open"),
            }
        }
    }
}

fn cmd_add(db: &Db, a: AddArgs) -> Result<()> {
    // Default: quick-add parse. --raw disables it for literal titles.
    let q = if a.raw {
        quickadd::QuickAdd {
            title: a.title.clone(),
            priority: Some(2),
            ..Default::default()
        }
    } else {
        quickadd::parse(&a.title).map_err(anyhow::Error::msg)?
    };
    for w in &q.warnings {
        eprintln!("{}", ui::section("warning", ui::Ink::Amber, w));
    }

    // CLI flags override parsed values.
    let priority = match a.priority.as_deref() {
        Some(s) => priority::parse(s).map_err(anyhow::Error::msg)?,
        None => q.priority.unwrap_or(2),
    };
    let description = a.description.clone().unwrap_or(q.description.clone());
    let deadline = a.deadline.clone().or_else(|| q.deadline.clone());

    let new = NewTask {
        title: q.title.clone(),
        description,
        priority,
        deadline,
        source_type: "claude_code".into(),
        ai_confidence: 1.0,
        ai_reasoning: a.reason.unwrap_or_default(),
    };
    let kind = match a.kind.as_deref() {
        Some(k) => Some(
            k.parse::<tasks::TaskKind>()
                .map_err(anyhow::Error::msg)?
                .as_str()
                .to_string(),
        ),
        None => None,
    };
    let deliverable = match a.deliverable.as_deref() {
        Some(d) => Some(
            tasks::validate_deliverable(d)
                .map_err(anyhow::Error::msg)?
                .to_string(),
        ),
        // A declared scout defaults to a report; ship keeps the historical
        // "unset" so existing rows and new ones stay comparable.
        None => match kind.as_deref() {
            Some("scout") => Some("report".to_string()),
            _ => None,
        },
    };
    let ext = Extensions {
        kind,
        deliverable,
        labels: q.labels.clone(),
        project: q.project.clone(),
        duration_min: q.duration_min,
        planned_at: None,
        energy: None,
        recurrence: q.recurrence.clone(),
        due_at: q.due.clone(),
    };

    let task = tasks::create_with_extensions(db, new, ext, &cli_ctx())?;

    // The quick-add derivations (labels, project, duration) live on `q`, not on
    // the row projection, so the JSON shape carries both halves — otherwise a
    // caller that creates with `@label`/`#project` cannot see what it just set.
    #[derive(serde::Serialize)]
    struct AddOutput {
        #[serde(flatten)]
        task: ptask_core::Task,
        labels: Vec<String>,
        project: Option<String>,
        duration_min: Option<i64>,
        reminder: Option<String>,
        recurrence: Option<String>,
        warnings: Vec<String>,
    }
    let out = AddOutput {
        task,
        labels: q.labels.clone(),
        project: q.project.clone(),
        duration_min: q.duration_min,
        reminder: q.reminder.clone(),
        recurrence: q.recurrence.as_ref().map(|r| r.original_input.clone()),
        warnings: q.warnings.clone(),
    };

    emit(&out, || {
        let t = &out.task;
        println!(
            "{}",
            ui::outcome(
                ui::Status::Ok,
                "created",
                t.pt_id.as_deref().unwrap_or_else(|| short_id(&t.id)),
                &t.title,
                ""
            )
        );
        // Echo the parsed interpretation so a silent quick-add mis-parse
        // (the PT-653 class) is visible at the moment of creation.
        let mut pairs: Vec<(&str, String)> = vec![("priority", ui::priority_pill(t.priority))];
        if let Some(d) = &t.deadline {
            pairs.push(("deadline", ui::due_cell(Some(d))));
        }
        if !t.description.is_empty() {
            pairs.push(("desc", t.description.clone()));
        }
        if !out.labels.is_empty() {
            pairs.push(("labels", out.labels.join(", ")));
        }
        if let Some(p) = &out.project {
            pairs.push(("project", p.clone()));
        }
        if let Some(m) = out.duration_min {
            pairs.push(("estimate", format!("{m}m")));
        }
        if let Some(r) = &out.reminder {
            pairs.push(("reminder", r.clone()));
        }
        if let Some(rec) = &out.recurrence {
            pairs.push(("recurs", rec.clone()));
        }
        pairs.push(("uuid", ui::dim(&t.id, ui::Ink::Slate)));
        for l in ui::kv(&pairs, 14) {
            println!("    {}", l.trim_start());
        }
    })
}

fn cmd_list(db: &Db, a: ListArgs) -> Result<()> {
    let p = a
        .priority
        .as_deref()
        .map(priority::parse)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    // When a filter is supplied, default status to "all" so the DSL is
    // authoritative. Explicit -s still wins if the user typed one.
    let status_filter = if a.status == "all" {
        None
    } else {
        Some(a.status.as_str())
    };
    let filter_expr = a
        .filter
        .as_deref()
        .map(ptask_core::filter::parse)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let sort = ptask_core::ordering::SortKey::parse(&a.sort).map_err(anyhow::Error::msg)?;
    let rows =
        tasks::list_with_filter_sorted(db, filter_expr.as_ref(), status_filter, p, a.limit, sort)?;
    if json_mode() {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let mut note = format!("{} · sorted by {}", a.status, a.sort);
    if let Some(f) = a.filter.as_deref() {
        note.push_str(&format!(" · filter {f:?}"));
    }
    note.push_str(&format!(" · {}", ui::utc_stamp()));
    print_lines(ui::headline("ptask · list", None, &note));
    if rows.is_empty() {
        println!("{}", ui::empty("no tasks match"));
        return Ok(());
    }
    let banded = matches!(sort, ptask_core::ordering::SortKey::Severity);
    print_lines(ui::task_table(&rows, banded, true, a.verbose));
    println!(
        "{}",
        ui::footer(rows.len(), "task", "pt show PT-N · pt done PT-N · pt next")
    );
    Ok(())
}

fn print_lines(lines: Vec<String>) {
    for l in lines {
        println!("{l}");
    }
}

fn cmd_done(db: &Db, a: DoneArgs) -> Result<()> {
    let mut results = Vec::new();
    for query in &a.queries {
        let task = tasks::resolve(db, query).map_err(anyhow::Error::msg)?;
        let outcome = tasks::mark_done(db, &task, &cli_ctx())?;
        let pt = task.pt_id.clone().unwrap_or_default();
        match &outcome {
            tasks::DoneOutcome::Completed => {
                if !json_mode() {
                    println!(
                        "{}",
                        ui::outcome(ui::Status::Ok, "done", &pt, &task.title, "")
                    );
                }
                results.push(serde_json::json!({
                    "pt_id": pt, "task_uuid": task.id, "title": task.title,
                    "outcome": "completed"
                }));
            }
            tasks::DoneOutcome::Advanced { next_deadline } => {
                if !json_mode() {
                    println!(
                        "{}",
                        ui::outcome(
                            ui::Status::Changed,
                            "advanced",
                            &pt,
                            &task.title,
                            &format!("recurring · next {next_deadline}")
                        )
                    );
                }
                results.push(serde_json::json!({
                    "pt_id": pt, "task_uuid": task.id, "title": task.title,
                    "outcome": "advanced", "next_deadline": next_deadline
                }));
            }
        }
    }
    if json_mode() {
        println!("{}", serde_json::to_string_pretty(&results)?);
    }
    Ok(())
}

fn cmd_priority(db: &Db, a: PriorityArgs) -> Result<()> {
    let level = priority::parse(&a.level).map_err(anyhow::Error::msg)?;
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let old = task.priority;
    if old == level {
        println!(
            "{}",
            ui::outcome(
                ui::Status::Mute,
                "unchanged",
                task.pt_id.as_deref().unwrap_or(""),
                &task.title,
                &format!("already {} ({})", level, priority::label(level))
            )
        );
        return Ok(());
    }
    tasks::update_priority(db, &task.id, level, &cli_ctx())?;
    // priority feeds manual_score -> the composite priority_score, so recompute
    // immediately; otherwise ordering (and the dashboard's "Critical Now") lags
    // until the next scheduled `pt scoring run`.
    let note = match ptask_core::scoring::run_once(db, false) {
        Ok(r) => format!(" · rescored {}", r.tasks_scored),
        Err(e) => {
            eprintln!(
                "{}",
                ui::section(
                    "warning",
                    ui::Ink::Amber,
                    &format!("priority set but rescore failed: {e}")
                )
            );
            String::new()
        }
    };
    println!(
        "{}",
        ui::outcome(
            ui::Status::Changed,
            "priority",
            task.pt_id.as_deref().unwrap_or(""),
            &task.title,
            &format!(
                "{} ({}) → {} ({}){}",
                old,
                priority::label(old),
                level,
                priority::label(level),
                note
            )
        )
    );
    Ok(())
}

fn cmd_edit(db: &Db, a: EditArgs) -> Result<()> {
    if a.deadline.is_some() && a.clear_deadline {
        anyhow::bail!("use either --deadline or --clear-deadline, not both");
    }
    let has_deadline = a.deadline.is_some() || a.clear_deadline;
    let has_text = a.title.is_some() || a.desc.is_some();
    let has_labels = !a.label.is_empty() || !a.unlabel.is_empty();
    if !has_deadline && !has_text && !has_labels {
        anyhow::bail!(
            "nothing to edit; use --deadline DATE | --clear-deadline | --title T | --desc D | --label L | --unlabel L"
        );
    }
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    if has_text {
        tasks::update_text(
            db,
            &task.id,
            a.title.as_deref(),
            a.desc.as_deref(),
            &cli_ctx(),
        )?;
    }
    if has_deadline {
        let new_deadline = if a.clear_deadline {
            None
        } else {
            a.deadline.as_deref()
        };
        tasks::update_deadline(db, &task.id, new_deadline, &cli_ctx())?;
    }
    if has_labels {
        tasks::modify_labels(db, &task.id, &a.label, &a.unlabel, &cli_ctx())?;
    }
    // Only the deadline feeds a score (urgency); a text-only edit needs no rescore.
    let note = if has_deadline {
        match ptask_core::scoring::run_once(db, false) {
            Ok(r) => format!(" · rescored {}", r.tasks_scored),
            Err(e) => {
                eprintln!(
                    "{}",
                    ui::section(
                        "warning",
                        ui::Ink::Amber,
                        &format!("edit applied but rescore failed: {e}")
                    )
                );
                String::new()
            }
        }
    } else {
        String::new()
    };
    let mut parts: Vec<String> = Vec::new();
    if has_deadline {
        parts.push(format!(
            "deadline {}",
            if a.clear_deadline {
                "cleared".to_string()
            } else {
                a.deadline.clone().unwrap_or_default()
            }
        ));
    }
    if a.title.is_some() {
        parts.push("title".into());
    }
    if a.desc.is_some() {
        parts.push("description".into());
    }
    if !a.label.is_empty() {
        parts.push(format!("+{}", a.label.join(" +")));
    }
    if !a.unlabel.is_empty() {
        parts.push(format!("-{}", a.unlabel.join(" -")));
    }
    println!(
        "{}",
        ui::outcome(
            ui::Status::Changed,
            "edited",
            task.pt_id.as_deref().unwrap_or(""),
            &task.title,
            &format!("{}{}", parts.join(" + "), note)
        )
    );
    Ok(())
}

fn cmd_reopen(db: &Db, a: ReopenArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    tasks::reopen(db, &task.id, &cli_ctx())?;
    // Reopening returns the task to the active set; rescore so it re-enters
    // ordering immediately rather than at the next scoring run.
    let note = match ptask_core::scoring::run_once(db, false) {
        Ok(r) => format!(" · rescored {}", r.tasks_scored),
        Err(e) => {
            eprintln!(
                "{}",
                ui::section(
                    "warning",
                    ui::Ink::Amber,
                    &format!("reopened but rescore failed: {e}")
                )
            );
            String::new()
        }
    };
    println!(
        "{}",
        ui::outcome(
            ui::Status::Changed,
            "reopened",
            task.pt_id.as_deref().unwrap_or(""),
            &task.title,
            &format!("→ pending{note}")
        )
    );
    Ok(())
}

/// Short display handle for a task with no PT-N: the first 8 chars of its id.
/// Char-safe — a raw `&id[..8]` panics when the id is shorter than 8 bytes or
/// when byte 8 lands inside a multi-byte scalar. Remote-path ids come from the
/// canonical server's JSON (`pt remote *`), so an unexpected id shape must not
/// crash the CLI; local ids are 36-char UUIDs where this is a no-op.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn cmd_show(db: &Db, a: ShowArgs) -> Result<()> {
    let t = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let d = tasks::load_detail(db, &t.id)?;
    let blocked = if d.depends_on.is_empty() {
        Vec::new()
    } else {
        tasks::open_blockers(db, &t.id).unwrap_or_default()
    };
    print_lines(render_show(&t, Some(&d), &blocked));
    Ok(())
}

/// The `pt show` page, shared with `pt remote show`: headline with the id and
/// priority badge, the title in paper, then a key/value block. `detail` is
/// optional because a pre-v1.9 remote server has no /detail route.
fn render_show(
    t: &ptask_core::Task,
    d: Option<&tasks::TaskDetail>,
    blocked: &[String],
) -> Vec<String> {
    let pt = t.pt_id.as_deref().unwrap_or_else(|| short_id(&t.id));
    let mut out = ui::headline(
        pt,
        Some((priority::label(t.priority), ui::priority_ink(t.priority))),
        &ui::strip_ansi(&ui::status_pill(&t.status)),
    );
    for l in ui::wrap(&t.title, ui::term_width().saturating_sub(4), "") {
        out.push(format!("  {}", ui::bold(&l, ui::Ink::Paper)));
    }
    out.push(String::new());
    let mut pairs: Vec<(&str, String)> = vec![
        ("status", ui::status_pill(&t.status)),
        (
            "priority",
            format!(
                "{}{}",
                ui::priority_pill(t.priority),
                ui::dim(&format!("  ({})", t.priority), ui::Ink::Slate)
            ),
        ),
        ("deadline", ui::due_cell(t.deadline.as_deref())),
        (
            "kind",
            match t.deliverable.as_deref() {
                Some(dl) => format!("{} → {dl}", t.kind),
                None => t.kind.clone(),
            },
        ),
    ];
    if let Some(d) = d {
        if !d.labels.is_empty() {
            pairs.push(("labels", d.labels.join(", ")));
        }
        if let Some(p) = &d.project {
            pairs.push(("project", p.clone()));
        }
        if let Some(m) = d.duration_min {
            pairs.push(("estimate", format!("{m}m")));
        }
        if !d.depends_on.is_empty() {
            pairs.push(("depends on", d.depends_on.join(", ")));
        }
        if !d.blocks_tasks.is_empty() {
            pairs.push(("blocks", d.blocks_tasks.join(", ")));
        }
        if let Some(r) = &d.recurrence_input {
            pairs.push(("recurs", r.clone()));
        }
    }
    pairs.push(("source", t.source_type.clone()));
    pairs.push(("uuid", ui::dim(&t.id, ui::Ink::Slate)));
    out.extend(ui::kv(&pairs, 14));
    if !blocked.is_empty() {
        out.push(String::new());
        out.push(ui::section(
            "blocked",
            ui::Ink::Amber,
            &format!("cannot close until done: {}", blocked.join(", ")),
        ));
    }
    if !t.description.is_empty() {
        out.push(String::new());
        out.push(ui::section("notes", ui::Ink::Cyan, ""));
        for l in t.description.lines() {
            if l.trim().is_empty() {
                out.push(String::new());
                continue;
            }
            for w in ui::wrap(l, ui::term_width().saturating_sub(4), "") {
                out.push(format!("  {}", ui::paint(&w, ui::Ink::Steel)));
            }
        }
    }
    out
}

fn cmd_dismiss(db: &Db, a: DismissArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    tasks::dismiss(db, &task.id, &cli_ctx())?;
    println!(
        "{}",
        ui::outcome(
            ui::Status::Mute,
            "dismissed",
            task.pt_id.as_deref().unwrap_or(""),
            &task.title,
            ""
        )
    );
    Ok(())
}

fn cmd_rm(db: &Db, a: RmArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let pt = task.pt_id.as_deref().unwrap_or("").to_string();
    if !a.yes {
        use std::io::Write;
        print!(
            "{}",
            ui::prompt(
                &format!(
                    "permanently delete {pt} \"{}\"? This cannot be undone.",
                    task.title
                ),
                "[y/N]"
            )
        );
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("{}", ui::empty("aborted"));
            return Ok(());
        }
    }
    tasks::delete_task(db, &task.id, &cli_ctx())?;
    println!(
        "{}",
        ui::outcome(ui::Status::Bad, "deleted", &pt, &task.title, "permanent")
    );
    Ok(())
}

fn cmd_next(db: &Db, a: NextArgs) -> Result<()> {
    let rows = dag::next_ready(db, a.limit)?;
    if json_mode() {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    print_lines(ui::headline(
        "ptask · next",
        Some(("ready", ui::Ink::Green)),
        &format!("unblocked, highest first · {}", ui::utc_stamp()),
    ));
    if rows.is_empty() {
        println!("{}", ui::empty("no ready tasks"));
        return Ok(());
    }
    print_lines(ui::task_table(&rows, false, true, false));
    println!(
        "{}",
        ui::footer(rows.len(), "ready task", "pt start PT-N · pt done PT-N")
    );
    Ok(())
}

fn gcalendar_path(explicit: Option<&Path>, home: Option<&std::ffi::OsStr>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let home = home
        .filter(|value| !value.is_empty())
        .context("HOME is unset; pass --gcal or set PTASK_GCAL")?;
    Ok(PathBuf::from(home).join(".config/puretensor/gcalendar.py"))
}

fn cmd_plan(db: &Db, a: PlanArgs) -> Result<()> {
    use ptask_core::jiff;
    use std::process::Command as Proc;

    let home = std::env::var_os("HOME");
    let gcal = gcalendar_path(a.gcal.as_deref(), home.as_deref())?;

    #[derive(serde::Deserialize)]
    struct FreeSlotJson {
        start: String,
        minutes: i64,
    }
    #[derive(serde::Deserialize)]
    struct FreeBusy {
        tz: String,
        free_slots: Vec<FreeSlotJson>,
    }
    #[derive(serde::Serialize)]
    struct ScheduledItem {
        pt_id: Option<String>,
        title: String,
        start: String,
        end: String,
        duration_min: i64,
        energy: Option<String>,
    }
    #[derive(serde::Serialize)]
    struct UnscheduledItem {
        pt_id: Option<String>,
        title: String,
        duration_min: i64,
    }
    #[derive(serde::Serialize)]
    struct PlanOutput {
        tz: String,
        scheduled: Vec<ScheduledItem>,
        unscheduled: Vec<UnscheduledItem>,
    }

    // 1. free/busy (Python owns tz/date math)
    let out = Proc::new("python3")
        .arg(&gcal)
        .arg(&a.account)
        .arg("freebusy")
        .arg("--json")
        .args(["--days", &a.days.to_string()])
        .args(["--work", &a.work])
        .args(["--tz", &a.tz])
        .args(["--calendar", &a.calendar])
        .output()
        .with_context(|| format!("running {} freebusy", gcal.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "gcalendar.py freebusy failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let fb: FreeBusy = serde_json::from_slice(&out.stdout).with_context(|| {
        format!(
            "parsing freebusy json: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })?;

    // 2. ready candidates -> pure first-fit pack over slot capacities
    let candidates = ptask_core::planner::ready_candidates(db, a.limit, a.slot_default)?;
    let slot_caps: Vec<i64> = fb.free_slots.iter().map(|s| s.minutes).collect();
    let plan = ptask_core::planner::pack(&candidates, &slot_caps);

    let tz = jiff::tz::TimeZone::get(&fb.tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let fmt = |ts: jiff::Timestamp| {
        ts.to_zoned(tz.clone())
            .strftime("%Y-%m-%d %H:%M")
            .to_string()
    };

    // 3. resolve placements into wall-clock times
    let mut scheduled = Vec::new();
    for p in &plan.scheduled {
        let slot_start: jiff::Timestamp = fb.free_slots[p.slot]
            .start
            .parse()
            .with_context(|| format!("parse slot start {}", fb.free_slots[p.slot].start))?;
        let start_ts = slot_start.checked_add(jiff::Span::new().minutes(p.offset_min))?;
        let end_ts = start_ts.checked_add(jiff::Span::new().minutes(p.duration_min))?;
        let c = &candidates[p.cand];
        scheduled.push(ScheduledItem {
            pt_id: c.pt_id.clone(),
            title: c.title.clone(),
            start: fmt(start_ts),
            end: fmt(end_ts),
            duration_min: p.duration_min,
            energy: c.energy.clone(),
        });
    }
    let unscheduled: Vec<UnscheduledItem> = plan
        .unscheduled
        .iter()
        .map(|&i| UnscheduledItem {
            pt_id: candidates[i].pt_id.clone(),
            title: candidates[i].title.clone(),
            duration_min: candidates[i].duration_min,
        })
        .collect();

    // 4. optional --write: tentative events on OUR calendar only
    if a.write {
        for s in &scheduled {
            let pt = s.pt_id.as_deref().unwrap_or("--");
            let title = format!("[pt] {} {}", pt, s.title);
            let status = Proc::new("python3")
                .arg(&gcal)
                .arg(&a.account)
                .arg("create")
                .args(["--title", &title])
                .args(["--start", &s.start])
                .args(["--end", &s.end])
                .args(["--calendar", &a.calendar])
                .args(["--description", &format!("advisory-plan {}", pt)])
                .status()
                .with_context(|| "creating calendar event")?;
            if !status.success() {
                eprintln!("warning: failed to create event for {}", pt);
            }
        }
    }

    let output = PlanOutput {
        tz: fb.tz.clone(),
        scheduled,
        unscheduled,
    };
    let write = a.write;
    emit(&output, || {
        print_lines(ui::headline(
            "ptask · plan",
            Some(if write {
                ("written", ui::Ink::Green)
            } else {
                ("advisory", ui::Ink::Amber)
            }),
            &format!("free-slot fit · {}", output.tz),
        ));
        if output.scheduled.is_empty() {
            println!("{}", ui::empty("no task fits the available free slots"));
        } else {
            let width = ui::term_width();
            let cols = [
                ui::Column::new("START", 16),
                ui::Column::right("MIN", 4),
                ui::Column::new("ID", 7),
                ui::Column::new(
                    "TITLE",
                    width
                        .saturating_sub(
                            ui::table_width(&[
                                ui::Column::new("", 16),
                                ui::Column::new("", 4),
                                ui::Column::new("", 7),
                            ]) + 3,
                        )
                        .max(24),
                ),
            ];
            let rows: Vec<Vec<String>> = output
                .scheduled
                .iter()
                .map(|s| {
                    vec![
                        ui::paint(&s.start, ui::Ink::Steel),
                        s.duration_min.to_string(),
                        ui::pt_id(s.pt_id.as_deref().unwrap_or("------")),
                        ui::paint(&s.title, ui::Ink::Paper),
                    ]
                })
                .collect();
            print_lines(ui::table(&cols, &rows, &Default::default()));
            println!("{}", ui::footer(rows.len(), "hold", ""));
        }
        if !output.unscheduled.is_empty() {
            println!();
            println!(
                "{}",
                ui::section(
                    "unscheduled",
                    ui::Ink::Amber,
                    &format!("{} · no free slot fits", output.unscheduled.len())
                )
            );
            for u in &output.unscheduled {
                println!(
                    "{}",
                    ui::bullet(
                        u.pt_id.as_deref().unwrap_or("------"),
                        &format!("{:>3}m  {}", u.duration_min, u.title),
                        ui::Ink::Amber,
                        8
                    )
                );
            }
        }
        if !write {
            println!();
            println!(
                "{}",
                ui::note("advisory only — re-run with --write to add tentative holds")
            );
        }
    })
}

fn cmd_view(db: &Db, c: ViewCommand) -> Result<()> {
    match c {
        ViewCommand::Save { name, filter } => {
            let v = views::create(db, &name, &filter).map_err(anyhow::Error::msg)?;
            println!(
                "{}",
                ui::outcome(ui::Status::Ok, "saved", &v.name, &v.filter_dsl, "view")
            );
            Ok(())
        }
        ViewCommand::List => {
            let vs = views::list(db).map_err(anyhow::Error::msg)?;
            print_lines(ui::headline("ptask · views", None, "saved filters"));
            if vs.is_empty() {
                println!("{}", ui::empty("no saved views"));
                return Ok(());
            }
            for v in &vs {
                println!("{}", ui::bullet(&v.name, &v.filter_dsl, ui::Ink::Cyan, 24));
            }
            println!("{}", ui::footer(vs.len(), "view", "pt view show NAME"));
            Ok(())
        }
        ViewCommand::Show { name, limit } => {
            let v = views::get(db, &name).map_err(anyhow::Error::msg)?;
            let expr = ptask_core::filter::parse(&v.filter_dsl).map_err(anyhow::Error::msg)?;
            let rows = tasks::list_with_filter(db, Some(&expr), None, None, limit)?;
            if json_mode() {
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            print_lines(ui::headline(
                &format!("ptask · view {}", v.name),
                None,
                &format!("filter {:?}", v.filter_dsl),
            ));
            if rows.is_empty() {
                println!("{}", ui::empty("no tasks match"));
                return Ok(());
            }
            print_lines(ui::task_table(&rows, false, true, false));
            println!("{}", ui::footer(rows.len(), "task", ""));
            Ok(())
        }
        ViewCommand::Rm { name } => {
            let removed = views::delete(db, &name).map_err(anyhow::Error::msg)?;
            if removed {
                println!(
                    "{}",
                    ui::outcome(ui::Status::Bad, "removed", &name, "", "view")
                );
            } else {
                println!("{}", ui::empty(&format!("no view named {name:?}")));
            }
            Ok(())
        }
    }
}

/// `pt mcp` — the agent-native tool surface over stdio. Actor comes from
/// `$PTASK_ACTOR` (config), source=mcp.
fn cmd_mcp(db: Db) -> Result<()> {
    let config = ptask_core::Config::from_env();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(ptask_server::mcp::serve_stdio(db, config.actor))
}

fn cmd_digest(db: &Db, a: DigestArgs) -> Result<()> {
    let v = ptask_core::digest::build(db, a.days)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

/// JSONL export: tasks + task_links + task_labels, one file each. With
/// --git the export dir becomes/updates a repo — a greppable, diffable,
/// mirrorable projection of the spine (the DB stays canonical).
fn cmd_export(db: &Db, a: ExportArgs) -> Result<()> {
    let out = a.out.unwrap_or_else(|| {
        ptask_core::config::home_dir()
            .join("puretensor-tasks")
            .join("export")
    });
    std::fs::create_dir_all(&out).with_context(|| format!("mkdir {:?}", out))?;
    let conn = db.get()?;
    let dump = |sql: &str, cols: &[&str], file: &str| -> Result<usize> {
        let mut stmt = conn.prepare(sql)?;
        let n_cols = cols.len();
        let mut rows = stmt.query([])?;
        let mut lines = Vec::new();
        while let Some(r) = rows.next()? {
            let mut m = serde_json::Map::new();
            for (i, c) in cols.iter().enumerate().take(n_cols) {
                let v: ptask_core::rusqlite::types::Value = r.get(i)?;
                m.insert(
                    (*c).to_string(),
                    match v {
                        ptask_core::rusqlite::types::Value::Null => serde_json::Value::Null,
                        ptask_core::rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                        ptask_core::rusqlite::types::Value::Real(f) => serde_json::json!(f),
                        ptask_core::rusqlite::types::Value::Text(t) => serde_json::json!(t),
                        ptask_core::rusqlite::types::Value::Blob(_) => serde_json::Value::Null,
                    },
                );
            }
            lines.push(serde_json::to_string(&m)?);
        }
        std::fs::write(out.join(file), lines.join("\n") + "\n")?;
        Ok(lines.len())
    };
    let nt = dump(
        "SELECT id, pt_id, title, description, priority, status_v2, created_at,
                updated_at, deadline, due_at, snoozed_until, source_type, task_type,
                project, parent_uuid, priority_score, escalation_level
         FROM tasks ORDER BY rowid",
        &[
            "id",
            "pt_id",
            "title",
            "description",
            "priority",
            "status",
            "created_at",
            "updated_at",
            "deadline",
            "due_at",
            "snoozed_until",
            "source_type",
            "task_type",
            "project",
            "parent_uuid",
            "priority_score",
            "escalation_level",
        ],
        "tasks.jsonl",
    )?;
    let nl = dump(
        "SELECT from_uuid, to_uuid, kind, created_at FROM task_links ORDER BY rowid",
        &["from_uuid", "to_uuid", "kind", "created_at"],
        "task_links.jsonl",
    )?;
    let nb = dump(
        "SELECT task_uuid, label FROM task_labels ORDER BY rowid",
        &["task_uuid", "label"],
        "task_labels.jsonl",
    )?;
    println!(
        "{}",
        ui::section(
            "exported",
            ui::Ink::Green,
            &format!("{nt} tasks · {nl} links · {nb} labels → {}", out.display())
        )
    );
    if a.git {
        if !out.join(".git").exists() {
            run_git_checked(&out, &["init", "-q"])?;
        }
        run_git_checked(&out, &["add", "-A"])?;
        let msg = format!("pt export: {} tasks, {} links, {} labels", nt, nl, nb);
        if git_has_staged_changes(&out)? {
            run_git_checked(&out, &["commit", "-q", "-m", &msg])?;
            println!("{}", ui::note(&format!("committed: {msg}")));
        } else {
            println!(
                "{}",
                ui::note("nothing to commit (no changes since last export)")
            );
        }
    }
    Ok(())
}

fn run_git(out: &std::path::Path, args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(out)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))
}

fn run_git_checked(out: &std::path::Path, args: &[&str]) -> Result<std::process::Output> {
    let output = run_git(out, args)?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!(
        "git {} failed ({}): {}",
        args.first().copied().unwrap_or("command"),
        output.status,
        stderr.trim()
    );
}

fn git_has_staged_changes(out: &std::path::Path) -> Result<bool> {
    let args = ["diff", "--cached", "--quiet", "--exit-code"];
    let output = run_git(out, &args)?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git diff failed ({}): {}", output.status, stderr.trim());
        }
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn delegation_command(handle: &str, title: &str) -> String {
    let prompt = format!(
        "Work the pTask task {handle}: {title}. When done: pt done {handle}; if blocked, pt capture a note explaining why."
    );
    format!("claude -p {}", shell_single_quote(&prompt))
}

/// `pt delegate` — OPERATOR-GATED skeleton. Prints the headless command;
/// never spawns it. Autonomy is revisited once the loop is proven.
fn cmd_delegate(db: &Db, a: DelegateArgs) -> Result<()> {
    let t = tasks::resolve_for_lookup(db, &a.id, false).map_err(anyhow::Error::msg)?;
    let handle = t.pt_id.clone().unwrap_or_else(|| t.id.clone());
    print_lines(ui::headline(
        &format!("ptask · delegate {handle}"),
        Some(("operator-gated", ui::Ink::Amber)),
        "review, then run it yourself",
    ));
    println!(
        "  {}",
        ui::paint(&delegation_command(&handle, &t.title), ui::Ink::Paper)
    );
    println!();
    println!(
        "{}",
        ui::note("operator-gated by design — pt will not spawn agents autonomously")
    );
    Ok(())
}

fn cmd_serve(db: Db, a: ServeArgs) -> Result<()> {
    let addr = match a.bind.as_deref() {
        Some(s) => s
            .parse()
            .with_context(|| format!("parsing --bind {:?}", s))?,
        None => ptask_server::default_bind(),
    };
    // The one env read for this process — everything downstream is injected.
    let config = ptask_core::Config::from_env();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(ptask_server::serve(
        db,
        addr,
        config.auth,
        config.webhooks,
        config.dash,
    ))
}

fn cmd_bot(db: Db) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(ptask_bot::run(db))
}

fn cmd_branch(db: &Db, a: BranchArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let pt = task.pt_id.clone().unwrap_or_else(|| "PT-?".into());
    println!("{}", tasks::branch_name(&pt, &task.title));
    Ok(())
}

fn cmd_accountability(db: Db, c: AccountabilityCommand) -> Result<()> {
    match c {
        AccountabilityCommand::Run(a) => {
            let mut cfg = ptask_core::Config::from_env().notify;
            if a.dry_run {
                cfg.dry_run = true;
            }
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("building tokio runtime")?;
            let report = rt.block_on(ptask_core::accountability::run_check(
                &db,
                &cfg,
                &ptask_notify::HttpDispatch,
            ))?;
            if report.quiet_hours {
                println!(
                    "{}",
                    ui::section("quiet hours", ui::Ink::Slate, "no dispatch")
                );
                return Ok(());
            }
            let tg = report.dispatched.iter().filter(|d| d.telegram_sent).count();
            let em = report.dispatched.iter().filter(|d| d.email_sent).count();
            // All-channels-dead is a hard failure: the 2026-05→06 incidents
            // (dead Gemini key, 401ing bot token) both hid behind an exit-0
            // "ok" line for weeks. Print the report, then fail the unit.
            let all_dead =
                report.eligible > 0 && report.dispatched.is_empty() && report.send_failures > 0;
            println!(
                "{}",
                ui::section(
                    if all_dead {
                        "accountability failed"
                    } else {
                        "accountability ok"
                    },
                    if all_dead {
                        ui::Ink::Red
                    } else {
                        ui::Ink::Green
                    },
                    &format!(
                        "eligible {} · dispatched {} · telegram {} · email {} · failures {} · budget {}/{}",
                        report.eligible,
                        report.dispatched.len(),
                        tg,
                        em,
                        report.send_failures,
                        report.budget_used_after,
                        ptask_core::accountability::DAILY_BUDGET_MAX,
                    )
                )
            );
            for d in &report.dispatched {
                println!(
                    "{}",
                    ui::bullet(
                        &d.task_uuid,
                        &format!(
                            "level {} · telegram {} · email {}",
                            d.level, d.telegram_sent, d.email_sent
                        ),
                        ui::Ink::Cyan,
                        36
                    )
                );
            }
            if all_dead {
                anyhow::bail!(
                    "accountability dispatch dead — {} eligible, 0 dispatched, {} send failures",
                    report.eligible,
                    report.send_failures
                );
            }
            Ok(())
        }
    }
}

fn cmd_start(db: &Db, a: StartArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    tasks::start(db, &task.id, &cli_ctx())?;
    emit(
        &serde_json::json!({"pt_id": task.pt_id, "task_uuid": task.id, "status": "in_progress"}),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Busy,
                    "started",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    "in progress"
                )
            )
        },
    )
}

fn cmd_promote(db: &Db, a: StartArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    tasks::promote(db, &task.id, &cli_ctx())?;
    emit(
        &serde_json::json!({
            "pt_id": task.pt_id, "task_uuid": task.id,
            "kind": "ship", "deliverable": "pr"
        }),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "promoted",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    "scout → ship (same row)"
                )
            )
        },
    )
}

fn cmd_kind(db: &Db, a: KindArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let kind: tasks::TaskKind = a.kind.parse().map_err(anyhow::Error::msg)?;
    if let Some(d) = a.deliverable.as_deref() {
        tasks::validate_deliverable(d).map_err(anyhow::Error::msg)?;
    }
    tasks::set_kind(db, &task.id, kind, a.deliverable.as_deref(), &cli_ctx())?;
    emit(
        &serde_json::json!({
            "pt_id": task.pt_id, "task_uuid": task.id,
            "kind": kind.as_str(), "deliverable": a.deliverable
        }),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "kind",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &match a.deliverable.as_deref() {
                        Some(d) => format!("{} → {d}", kind.as_str()),
                        None => kind.as_str().to_string(),
                    }
                )
            )
        },
    )
}

fn cmd_snooze(db: &Db, a: SnoozeArgs) -> Result<()> {
    let phrase = a.until.join(" ");
    if phrase.trim().is_empty() {
        anyhow::bail!("snooze needs a wake time, e.g. `pt snooze PT-42 next monday`");
    }
    let until = ptask_core::dates::parse(&phrase).map_err(anyhow::Error::msg)?;
    let until_iso = ptask_core::dates::format_iso(&until);
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    tasks::snooze(db, &task.id, &until_iso, &cli_ctx())?;
    emit(
        &serde_json::json!({
            "pt_id": task.pt_id, "task_uuid": task.id,
            "status": "snoozed", "snoozed_until": until_iso
        }),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Mute,
                    "snoozed",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &format!("until {until_iso}")
                )
            )
        },
    )
}

fn cmd_depend(db: &Db, a: DependArgs) -> Result<()> {
    let Some(on) = a.on.as_deref() else {
        // No --on: show current edges.
        let task = tasks::resolve_for_lookup(db, &a.query, true).map_err(anyhow::Error::msg)?;
        let detail = tasks::load_detail(db, &task.id)?;
        return emit(&detail, || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Mute,
                    "edges",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    ""
                )
            );
            let fmt = |v: &[String]| {
                if v.is_empty() {
                    "--".to_string()
                } else {
                    v.join(", ")
                }
            };
            print_lines(ui::kv(
                &[
                    ("depends on", fmt(&detail.depends_on)),
                    ("blocks", fmt(&detail.blocks_tasks)),
                ],
                14,
            ))
        });
    };
    let from = tasks::resolve_for_lookup(db, &a.query, true).map_err(anyhow::Error::msg)?;
    let to = tasks::resolve_for_lookup(db, on, true).map_err(anyhow::Error::msg)?;
    if a.clear {
        tasks::remove_dependency(db, &from.id, &to.id, &cli_ctx())?;
    } else {
        tasks::add_dependency(db, &from.id, &to.id, &cli_ctx())?;
    }
    emit(
        &serde_json::json!({
            "from": from.pt_id, "on": to.pt_id,
            "action": if a.clear { "removed" } else { "added" }
        }),
        || {
            println!(
                "{}",
                ui::outcome(
                    if a.clear {
                        ui::Status::Mute
                    } else {
                        ui::Status::Changed
                    },
                    if a.clear { "cleared" } else { "depends" },
                    from.pt_id.as_deref().unwrap_or_else(|| short_id(&from.id)),
                    &format!(
                        "→ {}  {}",
                        to.pt_id.as_deref().unwrap_or_else(|| short_id(&to.id)),
                        to.title
                    ),
                    if a.clear { "dependency removed" } else { "" }
                )
            )
        },
    )
}

fn cmd_why(db: &Db, a: WhyArgs) -> Result<()> {
    let task = tasks::resolve_for_lookup(db, &a.query, false).map_err(anyhow::Error::msg)?;
    let b = ptask_core::scoring::why(db, &task.id)?;
    emit(&b, || {
        print_lines(ui::headline(
            &format!("ptask · why {}", b.pt_id.as_deref().unwrap_or("-")),
            Some((&format!("rank {}/{}", b.rank, b.of), ui::Ink::Cyan)),
            &format!("composite {:.3}", b.composite),
        ));
        for l in ui::wrap(&b.title, ui::term_width().saturating_sub(4), "") {
            println!("  {}", ui::bold(&l, ui::Ink::Paper));
        }
        println!();
        let (wu, wd, wn, wm) = b.weights;
        let term = |v: f64, w: f64, why: &str| {
            format!(
                "{} {} {}",
                ui::paint(&format!("{v:.3}"), ui::Ink::Paper),
                ui::paint(&format!("× {w:.2}"), ui::Ink::Steel),
                ui::dim(why, ui::Ink::Slate)
            )
        };
        print_lines(ui::kv(
            &[
                ("urgency", term(b.urgency, wu, "")),
                (
                    "dependency",
                    term(b.dependency, wd, "active tasks blocked by this"),
                ),
                (
                    "neglect",
                    term(b.neglect, wn, "time since last touch / 30d"),
                ),
                ("manual", term(b.manual, wm, "priority")),
                (
                    "effort",
                    format!(
                        "{}  {}",
                        ui::paint(&format!("×{:.3}", b.effort_factor), ui::Ink::Paper),
                        ui::dim(&format!("llm nudge {:+.3}", b.score_llm), ui::Ink::Slate)
                    ),
                ),
            ],
            14,
        ));
    })
}

fn cmd_search(db: &Db, a: SearchArgs) -> Result<()> {
    let q = a.query.join(" ");
    if q.trim().is_empty() {
        anyhow::bail!("search needs a query");
    }
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT t.id, t.pt_id, t.title, t.status_v2, t.priority
         FROM tasks_fts f JOIN tasks t ON t.rowid = f.rowid
         WHERE tasks_fts MATCH ?1
         ORDER BY rank LIMIT ?2",
    )?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map((&q, a.limit as i64), |r| {
            Ok(serde_json::json!({
                "task_uuid": r.get::<_, String>(0)?,
                "pt_id": r.get::<_, Option<String>>(1)?,
                "title": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "priority": r.get::<_, i64>(4)?,
            }))
        })?
        .collect::<std::result::Result<_, _>>()?;
    emit(&rows, || {
        print_lines(ui::headline(
            "ptask · search",
            None,
            &format!("{q:?} · best match first"),
        ));
        if rows.is_empty() {
            println!("{}", ui::empty("no matches"));
            return;
        }
        let hits: Vec<ptask_core::Task> = rows
            .iter()
            .map(|r| ptask_core::Task {
                id: r["task_uuid"].as_str().unwrap_or("").to_string(),
                pt_id: r["pt_id"].as_str().map(str::to_string),
                title: r["title"].as_str().unwrap_or("").to_string(),
                description: String::new(),
                priority: r["priority"].as_i64().unwrap_or(2),
                status: r["status"].as_str().unwrap_or("").to_string(),
                created_at: String::new(),
                updated_at: String::new(),
                deadline: None,
                source_type: String::new(),
                ai_reasoning: String::new(),
                kind: String::new(),
                deliverable: None,
            })
            .collect();
        print_lines(ui::task_table(&hits, false, false, false));
        println!("{}", ui::footer(hits.len(), "match", ""));
    })
}

fn cmd_bulk(db: &Db, a: BulkArgs) -> Result<()> {
    let expr = ptask_core::filter::parse(&a.filter).map_err(anyhow::Error::msg)?;
    let matches = tasks::list_with_filter(db, Some(&expr), Some("pending"), None, 10_000)
        .map_err(anyhow::Error::msg)?;
    if matches.is_empty() {
        println!(
            "{}",
            ui::empty(&format!("bulk: no tasks match {:?}", a.filter))
        );
        return Ok(());
    }
    let action = if let Some(prio) = a.set_priority.as_deref() {
        format!("set priority {}", prio)
    } else if a.done {
        "mark done".into()
    } else if a.dismiss {
        "dismiss".into()
    } else {
        anyhow::bail!("bulk needs one of --set-priority / --done / --dismiss");
    };
    print_lines(ui::headline(
        "ptask · bulk",
        Some(if a.dry_run {
            ("dry run", ui::Ink::Amber)
        } else {
            ("apply", ui::Ink::Magenta)
        }),
        &format!("{} match {:?} · action: {action}", matches.len(), a.filter),
    ));
    print_lines(ui::task_table(&matches, false, false, false));
    if a.dry_run {
        println!("{}", ui::note("dry run — nothing applied"));
        return Ok(());
    }
    let ctx = cli_ctx();
    for t in &matches {
        if let Some(prio) = a.set_priority.as_deref() {
            let level = priority::parse(prio).map_err(anyhow::Error::msg)?;
            tasks::update_priority(db, &t.id, level, &ctx)?;
        } else if a.done {
            match tasks::mark_done(db, t, &ctx)? {
                tasks::DoneOutcome::Completed => {}
                tasks::DoneOutcome::Advanced { next_deadline } => {
                    println!(
                        "{}",
                        ui::outcome(
                            ui::Status::Changed,
                            "advanced",
                            t.pt_id.as_deref().unwrap_or("-"),
                            &t.title,
                            &format!("next {next_deadline}")
                        )
                    );
                }
            }
        } else if a.dismiss {
            tasks::dismiss(db, &t.id, &ctx)?;
        }
    }
    match ptask_core::scoring::run_once(db, false) {
        Ok(r) => println!(
            "{}",
            ui::section(
                "bulk applied",
                ui::Ink::Green,
                &format!("{} task(s) · rescored {}", matches.len(), r.tasks_scored)
            )
        ),
        Err(e) => println!(
            "{}",
            ui::section(
                "bulk applied",
                ui::Ink::Amber,
                &format!("{} task(s) · rescore failed: {e}", matches.len())
            )
        ),
    }
    Ok(())
}

type ReviewRow = (String, Option<String>, String, String, String);

fn stale_review_tasks(db: &Db, cutoff_iso: &str) -> Result<Vec<ReviewRow>> {
    let conn = db.get()?;
    // An unreadable `updated_at` counts as stale: `julianday()` is NULL on
    // junk, so without the guard the row would never reach review at all.
    let mut stmt = conn.prepare(
        "SELECT t.id, t.pt_id, t.title, t.status_v2, t.updated_at
         FROM tasks t
         WHERE t.status_v2 IN ('triage','backlog','todo','in_progress')
           AND (julianday(t.updated_at) IS NULL
                OR julianday(t.updated_at) < julianday(?1))
         ORDER BY t.updated_at ASC",
    )?;
    Ok(stmt
        .query_map([&cutoff_iso], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?)
}

fn cmd_review(db: &Db, a: ReviewArgs) -> Result<()> {
    use std::io::Write;
    let stale_cutoff = ptask_core::dates::now_in_operator_tz()
        .map_err(anyhow::Error::msg)?
        .checked_sub(ptask_core::jiff::Span::new().days(a.stale_days))
        .map_err(|e| anyhow::anyhow!("cutoff math: {e}"))?;
    let cutoff_iso = ptask_core::dates::format_iso(&stale_cutoff);
    let stale = stale_review_tasks(db, &cutoff_iso)?;

    print_lines(ui::headline(
        "ptask · review",
        Some((
            &format!("{} stale", stale.len()),
            if stale.is_empty() {
                ui::Ink::Green
            } else {
                ui::Ink::Amber
            },
        )),
        &format!("untouched > {}d · oldest first", a.stale_days),
    ));
    if stale.is_empty() {
        println!(
            "{}",
            ui::section("clean board", ui::Ink::Green, "nothing stale")
        );
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        for (_, pt, title, status, updated) in &stale {
            println!(
                "{}",
                ui::bullet(
                    pt.as_deref().unwrap_or("-"),
                    &format!(
                        "{}  {}  {}",
                        ui::status_pill(status),
                        ui::paint(title, ui::Ink::Paper),
                        ui::dim(
                            &format!("last touch {}", updated.get(..10).unwrap_or(updated)),
                            ui::Ink::Slate
                        )
                    ),
                    ui::Ink::Amber,
                    8
                )
            );
        }
        println!(
            "{}",
            ui::note("interactive triage needs a TTY: k=keep d=done x=dismiss s=snooze-1w q=quit")
        );
        return Ok(());
    }

    let ctx = cli_ctx();
    println!(
        "{}",
        ui::note("[k]eep  [d]one  [x] dismiss  [s]nooze 1w  [q]uit")
    );
    println!();
    for (uuid, pt, title, status, updated) in &stale {
        print!(
            "{}",
            ui::prompt(
                &format!(
                    "{}  {}  {}  {}",
                    ui::pt_id(pt.as_deref().unwrap_or("-")),
                    ui::status_pill(status),
                    title,
                    ui::dim(
                        &format!("last {}", updated.get(..10).unwrap_or(updated)),
                        ui::Ink::Slate
                    )
                ),
                "[k/d/x/s/q]"
            )
        );
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        match line.trim() {
            "d" => {
                let t = tasks::resolve_for_lookup(db, uuid, true).map_err(anyhow::Error::msg)?;
                match tasks::mark_done(db, &t, &ctx)? {
                    tasks::DoneOutcome::Completed => {
                        println!("      {}", ui::pill(ui::Status::Ok, "done"))
                    }
                    tasks::DoneOutcome::Advanced { next_deadline } => {
                        println!(
                            "      {}",
                            ui::pill(ui::Status::Changed, &format!("advanced to {next_deadline}"))
                        )
                    }
                }
            }
            "x" => {
                tasks::dismiss(db, uuid, &ctx)?;
                println!("      {}", ui::pill(ui::Status::Mute, "dismissed"));
            }
            "s" => {
                let until = ptask_core::dates::now_in_operator_tz()
                    .map_err(anyhow::Error::msg)?
                    .checked_add(ptask_core::jiff::Span::new().days(7))
                    .map_err(|e| anyhow::anyhow!("snooze math: {e}"))?;
                tasks::snooze(db, uuid, &ptask_core::dates::format_iso(&until), &ctx)?;
                println!("      {}", ui::pill(ui::Status::Mute, "snoozed 1 week"));
            }
            "q" => break,
            _ => {}
        }
    }
    Ok(())
}

fn cmd_log(db: &Db, a: LogArgs) -> Result<()> {
    let task = tasks::resolve_for_lookup(db, &a.query, true).map_err(anyhow::Error::msg)?;
    let pt = task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id));
    let events = ptask_core::event_log::history_for_task(db, &task.id, a.limit)?;
    if json_mode() {
        println!("{}", serde_json::to_string_pretty(&events)?);
        return Ok(());
    }
    print_lines(ui::headline(
        &format!("ptask · log {pt}"),
        None,
        &format!("{} · newest first", ui::clip(&task.title, 60)),
    ));
    if events.is_empty() {
        println!("{}", ui::empty("no journal events"));
        return Ok(());
    }
    let width = ui::term_width();
    let fixed = ui::table_width(&[
        ui::Column::new("", 16),
        ui::Column::new("", 22),
        ui::Column::new("", 12),
    ]) + 3;
    let cols = [
        ui::Column::new("WHEN", 16),
        ui::Column::new("EVENT", 22),
        ui::Column::new("ACTOR", 12),
        ui::Column::new("DETAIL", width.saturating_sub(fixed).max(24)),
    ];
    let rows: Vec<Vec<String>> = events
        .iter()
        .map(|e| {
            // ts to the minute is enough for a human trail
            vec![
                ui::paint(e.ts.get(..16).unwrap_or(&e.ts), ui::Ink::Steel),
                ui::paint(&e.event_type, ui::Ink::Paper),
                ui::paint(e.actor.as_deref().unwrap_or("-"), ui::Ink::Cyan),
                ui::paint(&summarize_payload(&e.payload), ui::Ink::Slate),
            ]
        })
        .collect();
    print_lines(ui::table(&cols, &rows, &Default::default()));
    println!("{}", ui::footer(rows.len(), "event", ""));
    Ok(())
}

/// One-line human summary of an event payload (drop envelope keys).
fn summarize_payload(payload: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if matches!(k.as_str(), "actor" | "source" | "task_uuid" | "pt_id") {
                continue;
            }
            parts.push(format!("{}={}", k, val));
            if parts.len() >= 3 {
                break;
            }
        }
    }
    parts.join(" ")
}

fn cmd_undo(db: &Db) -> Result<()> {
    let out = tasks::undo_last(db, &cli_ctx()).map_err(anyhow::Error::msg)?;
    println!(
        "{}",
        ui::section(
            "undo",
            ui::Ink::Green,
            &format!(
                "{} · reversed event #{}",
                out.description, out.reversed_event_id
            )
        )
    );
    Ok(())
}

fn cmd_token(db: &Db, c: TokenCommand) -> Result<()> {
    use ptask_core::tokens;
    match c {
        TokenCommand::Create(a) => {
            let scope = tokens::Scope::parse(&a.scope).ok_or_else(|| {
                anyhow::anyhow!("invalid scope {:?} (read|capture|write|admin)", a.scope)
            })?;
            let plain = tokens::create(db, &a.client_id, scope)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Ok,
                    "minted",
                    &a.client_id,
                    &format!("scope {}", scope.as_str()),
                    "token"
                )
            );
            println!();
            println!("  {}", ui::bold(&plain, ui::Ink::Paper));
            println!();
            println!(
                "{}",
                ui::section(
                    "shown once",
                    ui::Ink::Amber,
                    "store it with the consumer now"
                )
            );
            Ok(())
        }
        TokenCommand::List => {
            let infos = tokens::list(db)?;
            print_lines(ui::headline("ptask · tokens", None, "API clients"));
            if infos.is_empty() {
                println!("{}", ui::empty("no tokens minted"));
                return Ok(());
            }
            let cols = [
                ui::Column::new("CLIENT", 18),
                ui::Column::new("SCOPE", 8),
                ui::Column::new("STATE", 10),
                ui::Column::new("CREATED", 16),
                ui::Column::new("LAST USED", 16),
            ];
            let rows: Vec<Vec<String>> = infos
                .iter()
                .map(|t| {
                    vec![
                        ui::paint(&t.client_id, ui::Ink::Paper),
                        ui::paint(&t.scopes, ui::Ink::Steel),
                        if t.revoked_at.is_some() {
                            ui::pill(ui::Status::Bad, "revoked")
                        } else {
                            ui::pill(ui::Status::Ok, "active")
                        },
                        ui::paint(
                            t.created_at.get(..16).unwrap_or(&t.created_at),
                            ui::Ink::Steel,
                        ),
                        ui::paint(
                            t.last_used_at
                                .as_deref()
                                .map(|s| s.get(..16).unwrap_or(s))
                                .unwrap_or("never"),
                            ui::Ink::Slate,
                        ),
                    ]
                })
                .collect();
            print_lines(ui::table(&cols, &rows, &Default::default()));
            println!("{}", ui::footer(rows.len(), "token", ""));
            Ok(())
        }
        TokenCommand::Revoke(a) => {
            let n = tokens::revoke(db, &a.client_id)?;
            if n == 0 {
                anyhow::bail!("no active tokens for {:?}", a.client_id);
            }
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Bad,
                    "revoked",
                    &a.client_id,
                    &format!("{n} token(s)"),
                    ""
                )
            );
            Ok(())
        }
    }
}

fn cmd_gen_manpage() -> Result<()> {
    let cmd = <Cli as clap::CommandFactory>::command();
    let man = clap_mangen::Man::new(cmd);
    let mut buf: Vec<u8> = Vec::new();
    man.render(&mut buf).context("render manpage")?;
    std::io::Write::write_all(&mut std::io::stdout().lock(), &buf).context("write manpage")?;
    Ok(())
}

fn cmd_gen_completions(args: GenCompletionsArgs) -> Result<()> {
    use clap_complete::Shell;
    let shell = match args.shell {
        ShellChoice::Bash => Shell::Bash,
        ShellChoice::Zsh => Shell::Zsh,
        ShellChoice::Fish => Shell::Fish,
    };
    let mut cmd = <Cli as clap::CommandFactory>::command();
    clap_complete::generate(shell, &mut cmd, "pt", &mut std::io::stdout().lock());
    Ok(())
}

fn cmd_remote(c: RemoteCommand) -> Result<()> {
    match c {
        RemoteCommand::Add(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.add(&a.text)?;
            // Echo the parsed interpretation (priority + deadline) so a silent
            // mis-parse — the PT-653 class — is visible at the moment of creation.
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Ok,
                    "created",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    "remote"
                )
            );
            let mut pairs: Vec<(&str, String)> =
                vec![("priority", ui::priority_pill(task.priority))];
            if let Some(d) = &task.deadline {
                pairs.push(("deadline", ui::due_cell(Some(d))));
            }
            for l in ui::kv(&pairs, 14) {
                println!("    {}", l.trim_start());
            }
            Ok(())
        }
        RemoteCommand::List(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let priority_filter = a
                .priority
                .as_deref()
                .map(priority::parse)
                .transpose()
                .context("parsing --priority")?;
            let status_filter = if a.status == "all" {
                None
            } else {
                Some(a.status.as_str())
            };
            let tasks_out = if a.filter.is_some() {
                client.list_filtered(a.filter.as_deref(), &a.status, a.limit)?
            } else {
                client.list(status_filter, priority_filter, a.limit)?
            };
            if json_mode() {
                println!("{}", serde_json::to_string_pretty(&tasks_out)?);
                return Ok(());
            }
            let mut note = format!("{} · {}", a.status, client.url());
            if let Some(f) = a.filter.as_deref() {
                note.push_str(&format!(" · filter {f:?}"));
            }
            print_lines(ui::headline(
                "ptask · list",
                Some(("remote", ui::Ink::Violet)),
                &note,
            ));
            if tasks_out.is_empty() {
                println!("{}", ui::empty("no tasks"));
                return Ok(());
            }
            print_lines(ui::task_table(&tasks_out, false, true, false));
            println!("{}", ui::footer(tasks_out.len(), "task", ""));
            Ok(())
        }
        RemoteCommand::Done(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.done(&a.query)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Ok,
                    "done",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    "remote"
                )
            );
            Ok(())
        }
        RemoteCommand::Priority(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let level = priority::parse(&a.level).map_err(anyhow::Error::msg)?;
            let task = client.priority(&a.query, level)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "priority",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    &format!(
                        "{} ({}) · remote",
                        task.priority,
                        priority::label(task.priority)
                    )
                )
            );
            Ok(())
        }
        RemoteCommand::Edit(a) => {
            if a.deadline.is_some() && a.clear_deadline {
                anyhow::bail!("use either --deadline or --clear-deadline, not both");
            }
            let has_deadline = a.deadline.is_some() || a.clear_deadline;
            let has_text = a.title.is_some() || a.desc.is_some();
            if !has_deadline && !has_text {
                anyhow::bail!(
                    "nothing to edit; use --deadline DATE | --clear-deadline | --title T | --desc D"
                );
            }
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            // Resolve ONCE: a single /sync request carries both the title/desc
            // and deadline commands against the same resolved task_uuid, so a
            // rename can't drift the deadline onto a different task.
            let deadline_op = if a.clear_deadline {
                Some(None)
            } else {
                a.deadline.as_deref().map(Some)
            };
            let task = client.edit(&a.query, a.title.as_deref(), a.desc.as_deref(), deadline_op)?;
            let pt = task
                .pt_id
                .as_deref()
                .unwrap_or_else(|| short_id(&task.id))
                .to_string();
            let mut parts = Vec::new();
            if has_deadline {
                parts.push(format!(
                    "deadline → {}",
                    task.deadline.as_deref().unwrap_or("--")
                ));
            }
            if a.title.is_some() {
                parts.push("title".to_string());
            }
            if a.desc.is_some() {
                parts.push("description".to_string());
            }
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "edited",
                    &pt,
                    &task.title,
                    &format!("{} · remote", parts.join(" + "))
                )
            );
            Ok(())
        }
        RemoteCommand::Reopen(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.reopen(&a.query)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "reopened",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    &format!("→ {} · remote", task.status)
                )
            );
            Ok(())
        }
        RemoteCommand::Show(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let t = client.show(&a.query)?;
            // Rich side-table detail (best-effort: a pre-v1.9 server has no
            // /detail route, so just skip it and keep the base row).
            let d = client.detail(&t.id).ok();
            print_lines(render_show(&t, d.as_ref(), &[]));
            Ok(())
        }
        RemoteCommand::Next(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let rows = client.next(a.limit)?;
            print_lines(ui::headline(
                "ptask · next",
                Some(("remote", ui::Ink::Violet)),
                &format!("unblocked, highest first · {}", client.url()),
            ));
            if rows.is_empty() {
                println!("{}", ui::empty("no ready tasks"));
                return Ok(());
            }
            print_lines(ui::task_table(&rows, false, true, false));
            println!("{}", ui::footer(rows.len(), "ready task", ""));
            Ok(())
        }
        RemoteCommand::Dismiss(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.dismiss(&a.query)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Mute,
                    "dismissed",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    "remote"
                )
            );
            Ok(())
        }
        RemoteCommand::Start(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.start(&a.query)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Busy,
                    "started",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    "in progress · remote"
                )
            );
            Ok(())
        }
        RemoteCommand::Snooze(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let phrase = a.until.join(" ");
            let until = ptask_core::dates::parse(&phrase).map_err(anyhow::Error::msg)?;
            let until_iso = ptask_core::dates::format_iso(&until);
            let task = client.snooze(&a.query, &until_iso)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Mute,
                    "snoozed",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    &format!("until {until_iso} · remote")
                )
            );
            Ok(())
        }
        RemoteCommand::Depend(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.depend(&a.query, &a.on, a.clear)?;
            println!(
                "{}",
                ui::outcome(
                    if a.clear {
                        ui::Status::Mute
                    } else {
                        ui::Status::Changed
                    },
                    if a.clear { "cleared" } else { "depends" },
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    &format!("on {} · remote", a.on)
                )
            );
            Ok(())
        }
        RemoteCommand::Rm(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.rm(&a.query)?;
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Bad,
                    "deleted",
                    task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                    &task.title,
                    "permanent · remote"
                )
            );
            Ok(())
        }
        RemoteCommand::Version(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let local = ptask_core::VERSION;
            print_lines(ui::headline("ptask · version", None, client.url()));
            match client.server_version() {
                Some(server) if server == local => {
                    print_lines(ui::kv(
                        &[
                            ("client", format!("v{local}")),
                            ("server", format!("v{server}")),
                        ],
                        14,
                    ));
                    println!("{}", ui::section("in sync", ui::Ink::Green, ""));
                    Ok(())
                }
                Some(server) => {
                    print_lines(ui::kv(
                        &[
                            ("client", format!("v{local}")),
                            ("server", format!("v{server}")),
                        ],
                        14,
                    ));
                    println!(
                        "{}",
                        ui::section("version skew", ui::Ink::Red, "redeploy pt")
                    );
                    anyhow::bail!(
                        "client/server version skew (v{local} vs v{server}) — \
                         redeploy pt (scripts/ansible/ptask.yml)"
                    )
                }
                None => anyhow::bail!("server unreachable or predates GET /version"),
            }
        }
    }
}

fn cmd_reap(db: &Db, a: ReapArgs) -> Result<()> {
    let ctx = ptask_core::event_log::EventCtx::system("reap");
    let report = ptask_core::reap::run(db, a.dry_run, &ctx)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if report.reaped.is_empty() {
        println!(
            "{}",
            ui::section("reap ok", ui::Ink::Green, "nothing stale")
        );
        return Ok(());
    }
    for r in &report.reaped {
        println!(
            "{}",
            ui::outcome(
                if report.dry_run {
                    ui::Status::Warn
                } else {
                    ui::Status::Mute
                },
                if report.dry_run {
                    "would drop"
                } else {
                    "dismissed"
                },
                r.pt_id.as_deref().unwrap_or(&r.uuid),
                &r.title,
                &format!("[{}] idle since {}", r.source_type, r.updated_at)
            )
        );
    }
    println!(
        "{}",
        ui::section(
            "reap ok",
            if report.errors > 0 {
                ui::Ink::Amber
            } else {
                ui::Ink::Green
            },
            &format!(
                "{} task(s){}{} · reverse with `pt reopen <PT-N>`",
                report.reaped.len(),
                if report.dry_run { " (dry-run)" } else { "" },
                if report.errors > 0 {
                    format!(", {} error(s)", report.errors)
                } else {
                    String::new()
                }
            )
        )
    );
    Ok(())
}

fn cmd_scoring(db: &Db, c: ScoringCommand) -> Result<()> {
    match c {
        ScoringCommand::Run(a) => {
            if a.diff {
                print_rank_diff(db)?;
            }
            let now = ptask_core::dates::now_in_operator_tz().map_err(anyhow::Error::msg)?;
            let report = ptask_core::scoring::run_once_at_mode(db, a.dry_run, &now, !a.v1)?;
            println!(
                "{}",
                ui::section(
                    "scoring ok",
                    ui::Ink::Green,
                    &format!(
                        "{} tasks scored{}{}",
                        report.tasks_scored,
                        if report.dry_run { " (dry-run)" } else { "" },
                        if a.v1 { " (v1 formula)" } else { "" }
                    )
                )
            );
            Ok(())
        }
    }
}

/// Top-20 rank comparison between the retired v1 formula and v2, computed
/// side-by-side without writing either. The Phase-6 cutover evidence.
fn print_rank_diff(db: &Db) -> Result<()> {
    let now = ptask_core::dates::now_in_operator_tz().map_err(anyhow::Error::msg)?;
    // Score under each mode into memory using dry runs + reading why()-style
    // computation is heavier; simplest faithful approach: run v1 dry (logs
    // only), then compute v2 breakdowns via why() per active task.
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, pt_id, title, priority_score FROM tasks
         WHERE status NOT IN ('done','dismissed')",
    )?;
    let rows: Vec<(String, Option<String>, String, f64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);
    drop(conn);
    let mut v1_rank: Vec<(String, f64)> =
        rows.iter().map(|(id, _, _, s)| (id.clone(), *s)).collect();
    v1_rank.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let v1_pos: std::collections::HashMap<&String, usize> = v1_rank
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id, i + 1))
        .collect();

    let mut v2_scores = Vec::new();
    for (id, pt, title, _) in &rows {
        if let Ok(b) = ptask_core::scoring::why(db, id) {
            v2_scores.push((id.clone(), pt.clone(), title.clone(), b.composite));
        }
    }
    v2_scores.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    print_lines(ui::headline(
        "ptask · rank diff",
        None,
        "v2 top-20 vs current stored v1 ordering",
    ));
    let _ = now;
    let width = ui::term_width();
    let fixed = ui::table_width(&[
        ui::Column::new("", 3),
        ui::Column::new("", 7),
        ui::Column::new("", 6),
        ui::Column::new("", 5),
    ]) + 3;
    let cols = [
        ui::Column::right("#", 3),
        ui::Column::new("ID", 7),
        ui::Column::right("SCORE", 6),
        ui::Column::right("MOVE", 5),
        ui::Column::new("TITLE", width.saturating_sub(fixed).max(24)),
    ];
    let rows: Vec<Vec<String>> = v2_scores
        .iter()
        .take(20)
        .enumerate()
        .map(|(i, (id, pt, title, score))| {
            let old = v1_pos.get(id).copied().unwrap_or(0);
            let delta = old as i64 - (i as i64 + 1);
            let mv = match delta.signum() {
                1 => ui::paint(&format!("↑{}", delta.abs()), ui::Ink::Green),
                -1 => ui::paint(&format!("↓{}", delta.abs()), ui::Ink::Amber),
                _ => ui::dim("=", ui::Ink::Slate),
            };
            vec![
                ui::paint(&(i + 1).to_string(), ui::Ink::Steel),
                ui::pt_id(pt.as_deref().unwrap_or("-")),
                ui::paint(&format!("{score:.3}"), ui::Ink::Paper),
                mv,
                ui::paint(title, ui::Ink::Paper),
            ]
        })
        .collect();
    print_lines(ui::table(&cols, &rows, &Default::default()));
    Ok(())
}

fn cmd_distill_native(db: &Db, batch: usize) -> Result<()> {
    use ptask_distill::providers::{GeminiProvider, LlmProvider, OpenAiCompatProvider};
    let cfg = ptask_core::Config::from_env().distill;
    let provider: Box<dyn LlmProvider> = match cfg.llm_backend.as_str() {
        "local" => Box::new(OpenAiCompatProvider::new(
            cfg.local_llm_url,
            cfg.local_llm_model,
        )?),
        "gemini" => {
            let Some(key) = cfg.gemini_api_key else {
                eprintln!("distill: GOOGLE_API_KEY is not set — failing closed (exit 3)");
                std::process::exit(3);
            };
            Box::new(GeminiProvider::new(key, cfg.gemini_model)?)
        }
        other => anyhow::bail!(
            "unsupported PTASK_LLM_BACKEND={other:?} — expected \"local\" or \"gemini\""
        ),
    };
    let provider_name = provider.name();
    match ptask_distill::pipeline::run_native(db, provider.as_ref(), batch) {
        Ok(r) => {
            println!(
                "{}",
                ui::section(
                    "distill ok",
                    if r.failed > 0 {
                        ui::Ink::Amber
                    } else {
                        ui::Ink::Green
                    },
                    &format!(
                        "{} · consumed {} · kept {} · created {} · deduped {} · failed {} · {}ms",
                        provider_name,
                        r.consumed,
                        r.kept,
                        r.created,
                        r.skipped_dedup,
                        r.failed,
                        r.duration_ms
                    )
                )
            );
            if r.quarantined > 0 {
                println!(
                    "  {} capture(s) quarantined after {} failed attempts — \
                     inspect: SELECT id, distill_error FROM raw_items \
                     WHERE processed=0 AND distill_attempts>={}",
                    r.quarantined,
                    ptask_core::raw_items::MAX_DISTILL_ATTEMPTS,
                    ptask_core::raw_items::MAX_DISTILL_ATTEMPTS
                );
            }
            Ok(())
        }
        Err(e) => {
            ptask_distill::pipeline::record_failure(db, provider_name, &e.to_string());
            // The fail-closed run is precisely the one on which rows cross the
            // ceiling, so the quarantine count matters MORE here than on the Ok
            // path. run_native returns Err without a report, so read the count
            // from the database directly. A failure to read it must not mask
            // the real error, hence the silent fallback.
            match ptask_core::raw_items::quarantined_count(db) {
                Ok(n) if n > 0 => eprintln!(
                    "  {} capture(s) quarantined after {} failed attempts — \
                     inspect: SELECT id, distill_error FROM raw_items \
                     WHERE processed=0 AND distill_attempts>={}",
                    n,
                    ptask_core::raw_items::MAX_DISTILL_ATTEMPTS,
                    ptask_core::raw_items::MAX_DISTILL_ATTEMPTS
                ),
                _ => {}
            }
            anyhow::bail!("distill native FAILED (fail closed): {e:#}")
        }
    }
}

fn cmd_distill(db: &Db, a: DistillArgs) -> Result<()> {
    cmd_distill_native(db, a.batch)
}

fn cmd_backfill(db: &Db) -> Result<()> {
    let n = pt_id::backfill_all(db)?;
    println!(
        "{}",
        ui::section(
            "backfill",
            ui::Ink::Green,
            &format!("PT-N assigned to {n} task(s)")
        )
    );
    let promoted = ptask_core::convert::promote_subtasks_once(db)?;
    if promoted > 0 {
        println!(
            "{}",
            ui::note(&format!(
                "promoted {promoted} subtask(s) to child tasks (schema v2 one-shot)"
            ))
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ExportArgs, cmd_export, delegation_command, gcalendar_path, git_has_staged_changes,
        run_git_checked, short_id, stale_review_tasks,
    };

    #[test]
    fn gcalendar_default_follows_the_current_home() {
        let home = std::ffi::OsStr::new("/srv/ptask-user");
        assert_eq!(
            gcalendar_path(None, Some(home)).unwrap(),
            std::path::Path::new("/srv/ptask-user/.config/puretensor/gcalendar.py")
        );
    }

    #[test]
    fn gcalendar_explicit_path_does_not_require_home() {
        let explicit = std::path::Path::new("/opt/calendar/gcalendar.py");
        assert_eq!(gcalendar_path(Some(explicit), None).unwrap(), explicit);
    }

    #[test]
    fn delegation_command_round_trips_shell_metacharacters() {
        let title =
            "audit $(printf SUBSTITUTED) `printf BACKTICK` O'Brien \\\nnext; printf INJECTED";
        let handle = "PT-42";
        let expected = format!(
            "Work the pTask task {handle}: {title}. When done: pt done {handle}; if blocked, pt capture a note explaining why."
        );
        let command = delegation_command(handle, title);
        let script = format!("claude() {{ printf '%s' \"$2\"; }}; {command}");
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();

        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }

    #[test]
    fn delegation_command_quotes_the_complete_prompt() {
        assert_eq!(
            delegation_command("PT-7", "review Alan's quote"),
            "claude -p 'Work the pTask task PT-7: review Alan'\"'\"'s quote. When done: pt done PT-7; if blocked, pt capture a note explaining why.'"
        );
    }

    #[test]
    fn export_git_propagates_commit_failure() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export");
        std::fs::create_dir(&out).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&out)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", ""]);
        git(&["config", "user.email", ""]);

        let db = ptask_core::Db::open(dir.path().join("tasks.db")).unwrap();
        let error = cmd_export(
            &db,
            ExportArgs {
                out: Some(out),
                git: true,
            },
        )
        .unwrap_err();

        assert!(
            format!("{error:#}").contains("git commit failed"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn export_git_distinguishes_no_change_from_staged_content() {
        let dir = tempfile::tempdir().unwrap();
        run_git_checked(dir.path(), &["init", "-q"]).unwrap();
        assert!(!git_has_staged_changes(dir.path()).unwrap());

        std::fs::write(dir.path().join("tasks.jsonl"), "{}\n").unwrap();
        run_git_checked(dir.path(), &["add", "-A"]).unwrap();
        assert!(git_has_staged_changes(dir.path()).unwrap());
    }

    #[test]
    fn short_id_is_char_boundary_safe() {
        // Normal 36-char UUID → first 8 chars (the common local case).
        assert_eq!(short_id("0123456789abcdef-0000"), "01234567");
        // Shorter than 8 bytes → whole string, no panic (out-of-range slice).
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
        // A remote-supplied id whose byte 8 lands inside a multi-byte scalar
        // must not panic (`&id[..8]` did): "1234567é" has 'é' at bytes 7..9.
        assert_eq!(short_id("1234567é"), "1234567é");
        // Multi-byte chars before byte 8: 'ú' starts at byte 8 (a boundary),
        // so the first 8 bytes are the 4 two-byte scalars "áéíó".
        assert_eq!(short_id("áéíóúab8xyz"), "áéíó");
    }

    #[test]
    fn stale_review_query_compares_instants_across_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("review.db")).unwrap();
        let ctx = ptask_core::event_log::EventCtx::test();
        let stale =
            ptask_core::tasks::create(&db, ptask_core::NewTask::minimal("stale"), &ctx).unwrap();
        let fresh =
            ptask_core::tasks::create(&db, ptask_core::NewTask::minimal("fresh"), &ctx).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET updated_at='2026-07-01T10:30:00Z' WHERE id=?1",
                [&stale.id],
            )?;
            c.execute(
                "UPDATE tasks SET updated_at='2026-07-01T11:30:00Z' WHERE id=?1",
                [&fresh.id],
            )?;
            Ok(())
        })
        .unwrap();

        let rows = stale_review_tasks(&db, "2026-07-01T12:00:00+01:00").unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.0.as_str()).collect();
        assert!(ids.contains(&stale.id.as_str()));
        assert!(!ids.contains(&fresh.id.as_str()));
    }
}
