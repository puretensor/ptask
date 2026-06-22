//! `pt` — pTask command-line interface.
//!
//! v0.1.0 covers parity with the legacy Python `cli.py`:
//!   pt add <title> [-p PRIORITY] [-d DESCRIPTION] [--deadline ISO] [--reason TEXT]
//!   pt list        [-s STATUS]   [-p PRIORITY]    [-n LIMIT]       [-v]
//!   pt done <query>
//!
//! Future phases add `show`, `rm`, richer `edit`, `serve`, `bot`, `tui`.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ptask_core::{Db, Extensions, NewTask, dag, priority, pt_id, quickadd, tasks, views};
use std::io::IsTerminal;

mod remote;

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
    /// Show ready-to-start tasks (all dependencies done).
    Next(NextArgs),
    /// Manage saved views.
    #[command(subcommand)]
    View(ViewCommand),
    /// Launch the terminal UI (ratatui).
    Tui,
    /// Run the HTTP server (sync API, capture, webhooks, metrics).
    Serve(ServeArgs),
    /// Run the Telegram bot (teloxide long-poll).
    Bot,
    /// Print a Linear-style branch name for a task.
    Branch(BranchArgs),
    /// Run the distillation pipeline (Python subprocess shim until v0.9).
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
struct AccountabilityRunArgs {
    /// Don't actually send anything; log what would have been dispatched.
    #[arg(long = "dry-run")]
    dry_run: bool,
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
    /// https://ptask.ts.puretensor.local).
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
}

#[derive(clap::Args, Debug)]
struct DistillArgs {
    /// Days of history for the Python pipeline to ingest (defaults to 60,
    /// matching the legacy systemd unit).
    #[arg(long = "days", default_value_t = 60)]
    days: u32,
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
}

#[derive(clap::Args, Debug)]
struct DoneArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
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
}

#[derive(clap::Args, Debug)]
struct ReopenArgs {
    /// PT-N (e.g. PT-42), bare integer (42), or title substring.
    query: String,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

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
                Some(Command::Next(a)) => cmd_next(&db, a),
                Some(Command::View(c)) => cmd_view(&db, c),
                Some(Command::Tui) => ptask_tui::run(db),
                Some(Command::Serve(a)) => cmd_serve(db, a),
                Some(Command::Bot) => cmd_bot(db),
                Some(Command::Branch(a)) => cmd_branch(&db, a),
                Some(Command::Distill(a)) => cmd_distill(&db, a),
                Some(Command::Accountability(c)) => cmd_accountability(db, c),
                Some(Command::Scoring(c)) => cmd_scoring(&db, c),
                Some(Command::Backfill) => cmd_backfill(&db),
                None if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => {
                    ptask_tui::run(db)
                }
                None => {
                    // Non-interactive fallback: avoid trying to enter alt-screen when
                    // stdin/stdout is not a TTY. Interactive `pt` opens the TUI.
                    println!("pt {} — sovereign task manager.", ptask_core::VERSION);
                    println!("Try: pt tui | pt add \"...\" | pt list | pt done PT-N | pt --help");
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
        eprintln!("warning: {}", w);
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
    let ext = Extensions {
        labels: q.labels.clone(),
        project: q.project.clone(),
        duration_min: q.duration_min,
        planned_at: None,
        energy: None,
        recurrence: q.recurrence.clone(),
    };

    let task = tasks::create_with_extensions(db, new, ext)?;

    let label = priority::label(task.priority).to_ascii_uppercase();
    println!("Task created [{}]: {}", label, task.title);
    if let Some(pid) = &task.pt_id {
        println!("  {}", pid);
    }
    println!("  ID: {}", task.id);
    println!(
        "  Priority: {} ({})",
        task.priority,
        priority::label(task.priority)
    );
    if let Some(d) = &task.deadline {
        println!("  Deadline: {}", d);
    }
    if !task.description.is_empty() {
        println!("  Description: {}", task.description);
    }
    if !q.labels.is_empty() {
        println!("  Labels: {}", q.labels.join(", "));
    }
    if let Some(p) = &q.project {
        println!("  Project: {}", p);
    }
    if let Some(m) = q.duration_min {
        println!("  Duration: {}m", m);
    }
    if let Some(r) = &q.reminder {
        println!("  Reminder: {}", r);
    }
    if let Some(rec) = &q.recurrence {
        println!("  Recurring: {}", rec.original_input);
    }
    Ok(())
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
    let rows = tasks::list_with_filter(db, filter_expr.as_ref(), status_filter, p, a.limit)?;
    if rows.is_empty() {
        println!("No tasks found.");
        return Ok(());
    }
    for t in &rows {
        let label = priority::label(t.priority).to_ascii_uppercase();
        let pt = t.pt_id.as_deref().unwrap_or("------");
        println!("[{:8}] [{:8}] {:7} {}", label, t.status, pt, t.title);
        if a.verbose {
            if !t.description.is_empty() {
                let snippet: String = t.description.chars().take(120).collect();
                println!("           {}", snippet);
            }
            println!("           ID: {}", t.id);
            if let Some(d) = &t.deadline {
                println!("           Deadline: {}", d);
            }
        }
    }
    println!("\nShowing {} tasks", rows.len());
    Ok(())
}

fn cmd_done(db: &Db, a: DoneArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let outcome = tasks::mark_done(db, &task)?;
    let pt = task.pt_id.as_deref().unwrap_or("");
    match outcome {
        tasks::DoneOutcome::Completed => {
            println!("Marked done: {} {}", pt, task.title);
        }
        tasks::DoneOutcome::Advanced { next_deadline } => {
            println!(
                "Recurring task advanced: {} {}\n  Next deadline: {}",
                pt, task.title, next_deadline
            );
        }
    }
    Ok(())
}

fn cmd_priority(db: &Db, a: PriorityArgs) -> Result<()> {
    let level = priority::parse(&a.level).map_err(anyhow::Error::msg)?;
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    let old = task.priority;
    if old == level {
        println!(
            "{} {} · already {} ({})",
            task.pt_id.as_deref().unwrap_or(""),
            task.title,
            level,
            priority::label(level)
        );
        return Ok(());
    }
    tasks::update_priority(db, &task.id, level)?;
    // priority feeds manual_score -> the composite priority_score, so recompute
    // immediately; otherwise ordering (and the dashboard's "Critical Now") lags
    // until the next scheduled `pt scoring run`.
    let note = match ptask_core::scoring::run_once(db, false) {
        Ok(r) => format!(" · rescored {}", r.tasks_scored),
        Err(e) => {
            eprintln!("warning: priority set but rescore failed: {}", e);
            String::new()
        }
    };
    println!(
        "{} {} · {} ({}) -> {} ({}){}",
        task.pt_id.as_deref().unwrap_or(""),
        task.title,
        old,
        priority::label(old),
        level,
        priority::label(level),
        note
    );
    Ok(())
}

fn cmd_edit(db: &Db, a: EditArgs) -> Result<()> {
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
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    if has_text {
        tasks::update_text(db, &task.id, a.title.as_deref(), a.desc.as_deref())?;
    }
    if has_deadline {
        let new_deadline = if a.clear_deadline {
            None
        } else {
            a.deadline.as_deref()
        };
        tasks::update_deadline(db, &task.id, new_deadline)?;
    }
    // Only the deadline feeds a score (urgency); a text-only edit needs no rescore.
    let note = if has_deadline {
        match ptask_core::scoring::run_once(db, false) {
            Ok(r) => format!(" · rescored {}", r.tasks_scored),
            Err(e) => {
                eprintln!("warning: edit applied but rescore failed: {}", e);
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
    println!(
        "{} {} · edited {}{}",
        task.pt_id.as_deref().unwrap_or(""),
        task.title,
        parts.join(" + "),
        note
    );
    Ok(())
}

fn cmd_reopen(db: &Db, a: ReopenArgs) -> Result<()> {
    let task = tasks::resolve(db, &a.query).map_err(anyhow::Error::msg)?;
    tasks::reopen(db, &task.id)?;
    // Reopening returns the task to the active set; rescore so it re-enters
    // ordering immediately rather than at the next scoring run.
    let note = match ptask_core::scoring::run_once(db, false) {
        Ok(r) => format!(" · rescored {}", r.tasks_scored),
        Err(e) => {
            eprintln!("warning: reopened but rescore failed: {}", e);
            String::new()
        }
    };
    println!(
        "{} {} · reopened → pending{}",
        task.pt_id.as_deref().unwrap_or(""),
        task.title,
        note
    );
    Ok(())
}

fn cmd_next(db: &Db, a: NextArgs) -> Result<()> {
    let rows = dag::next_ready(db, a.limit)?;
    if rows.is_empty() {
        println!("No ready tasks.");
        return Ok(());
    }
    for t in &rows {
        let label = priority::label(t.priority).to_ascii_uppercase();
        let pt = t.pt_id.as_deref().unwrap_or("------");
        let due = t.deadline.as_deref().unwrap_or("--");
        println!("[{:8}] {:7} {:10} {}", label, pt, due, t.title);
    }
    println!("\nShowing {} ready task(s).", rows.len());
    Ok(())
}

fn cmd_view(db: &Db, c: ViewCommand) -> Result<()> {
    match c {
        ViewCommand::Save { name, filter } => {
            let v = views::create(db, &name, &filter).map_err(anyhow::Error::msg)?;
            println!("Saved view '{}': {}", v.name, v.filter_dsl);
            Ok(())
        }
        ViewCommand::List => {
            let vs = views::list(db).map_err(anyhow::Error::msg)?;
            if vs.is_empty() {
                println!("No saved views.");
                return Ok(());
            }
            for v in &vs {
                println!("  {:24}  {}", v.name, v.filter_dsl);
            }
            println!("\n{} view(s).", vs.len());
            Ok(())
        }
        ViewCommand::Show { name, limit } => {
            let v = views::get(db, &name).map_err(anyhow::Error::msg)?;
            println!("View '{}': {}", v.name, v.filter_dsl);
            let expr = ptask_core::filter::parse(&v.filter_dsl).map_err(anyhow::Error::msg)?;
            let rows = tasks::list_with_filter(db, Some(&expr), None, None, limit)?;
            if rows.is_empty() {
                println!("(no tasks match)");
                return Ok(());
            }
            for t in &rows {
                let label = priority::label(t.priority).to_ascii_uppercase();
                let pt = t.pt_id.as_deref().unwrap_or("------");
                println!("[{:8}] [{:8}] {:7} {}", label, t.status, pt, t.title);
            }
            println!("\nShowing {} task(s).", rows.len());
            Ok(())
        }
        ViewCommand::Rm { name } => {
            let removed = views::delete(db, &name).map_err(anyhow::Error::msg)?;
            if removed {
                println!("Removed view '{}'.", name);
            } else {
                println!("No view named '{}'.", name);
            }
            Ok(())
        }
    }
}

fn cmd_serve(db: Db, a: ServeArgs) -> Result<()> {
    let addr = match a.bind.as_deref() {
        Some(s) => s
            .parse()
            .with_context(|| format!("parsing --bind {:?}", s))?,
        None => ptask_server::default_bind(),
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(ptask_server::serve(db, addr))
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
            let mut cfg = ptask_core::accountability::DispatchCfg::from_env();
            if a.dry_run {
                cfg.dry_run = true;
            }
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("building tokio runtime")?;
            let report = rt.block_on(ptask_core::accountability::run_check(&db, &cfg))?;
            if report.quiet_hours {
                println!("quiet hours — no dispatch");
                return Ok(());
            }
            let tg = report.dispatched.iter().filter(|d| d.telegram_sent).count();
            let em = report.dispatched.iter().filter(|d| d.email_sent).count();
            println!(
                "accountability ok — eligible={} dispatched={} telegrams={} emails={} budget={}/{}",
                report.eligible,
                report.dispatched.len(),
                tg,
                em,
                report.budget_used_after,
                ptask_core::accountability::DAILY_BUDGET_MAX,
            );
            for d in &report.dispatched {
                println!(
                    "  {} level={} telegram={} email={}",
                    d.task_uuid, d.level, d.telegram_sent, d.email_sent
                );
            }
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
            println!(
                "remote add ok — {} {}",
                task.pt_id.as_deref().unwrap_or(&task.id[..8]),
                task.title
            );
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
            let tasks_out = client.list(status_filter, priority_filter, a.limit)?;
            if tasks_out.is_empty() {
                println!("remote list — no tasks");
                return Ok(());
            }
            for t in &tasks_out {
                let label = t.pt_id.clone().unwrap_or_else(|| t.id[..8].to_string());
                println!("{:8}  p{}  {:9}  {}", label, t.priority, t.status, t.title);
            }
            Ok(())
        }
        RemoteCommand::Done(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.done(&a.query)?;
            println!(
                "remote done ok — {} {}",
                task.pt_id.as_deref().unwrap_or(&task.id[..8]),
                task.title
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
                "remote priority ok — {} {} · p{} ({})",
                task.pt_id.as_deref().unwrap_or(&task.id[..8]),
                task.title,
                task.priority,
                priority::label(task.priority)
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
            // Title/description and deadline are separate commands (one event
            // each); run text first so the deadline call re-resolves the new title.
            let mut task = None;
            if has_text {
                task = Some(client.retext(&a.query, a.title.as_deref(), a.desc.as_deref())?);
            }
            if has_deadline {
                let new_deadline = if a.clear_deadline {
                    None
                } else {
                    a.deadline.as_deref()
                };
                task = Some(client.edit_deadline(&a.query, new_deadline)?);
            }
            let task = task.expect("validated that at least one edit runs");
            let pt = task.pt_id.as_deref().unwrap_or(&task.id[..8]).to_string();
            println!("remote edit ok — {} {}", pt, task.title);
            if has_deadline {
                println!("  deadline → {}", task.deadline.as_deref().unwrap_or("--"));
            }
            if a.title.is_some() {
                println!("  title updated");
            }
            if a.desc.is_some() {
                println!("  description updated");
            }
            Ok(())
        }
        RemoteCommand::Reopen(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.reopen(&a.query)?;
            println!(
                "remote reopen ok — {} {} · {}",
                task.pt_id.as_deref().unwrap_or(&task.id[..8]),
                task.title,
                task.status
            );
            Ok(())
        }
        RemoteCommand::Show(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let t = client.show(&a.query)?;
            let pt = t.pt_id.as_deref().unwrap_or(&t.id[..8]);
            println!(
                "{}  [{}]",
                pt,
                priority::label(t.priority).to_ascii_uppercase()
            );
            println!("  {}", t.title);
            println!("  status:   {}", t.status);
            println!(
                "  priority: {} ({})",
                t.priority,
                priority::label(t.priority)
            );
            println!("  deadline: {}", t.deadline.as_deref().unwrap_or("--"));
            if !t.description.is_empty() {
                println!("  desc:     {}", t.description);
            }
            println!("  source:   {}", t.source_type);
            println!("  uuid:     {}", t.id);
            // Rich side-table detail (best-effort: a pre-v1.9 server has no
            // /detail route, so just skip it and keep the base row).
            if let Ok(d) = client.detail(&t.id) {
                if !d.labels.is_empty() {
                    println!("  labels:   {}", d.labels.join(", "));
                }
                if let Some(p) = &d.project {
                    println!("  project:  {}", p);
                }
                if let Some(m) = d.duration_min {
                    println!("  est:      {}m", m);
                }
                if !d.depends_on.is_empty() {
                    println!("  deps on:  {}", d.depends_on.join(", "));
                }
                if !d.blocks_tasks.is_empty() {
                    println!("  blocks:   {}", d.blocks_tasks.join(", "));
                }
                if let Some(r) = &d.recurrence_input {
                    println!("  recurs:   {}", r);
                }
            }
            Ok(())
        }
        RemoteCommand::Next(a) => {
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let rows = client.next(a.limit)?;
            if rows.is_empty() {
                println!("remote next — no ready tasks");
                return Ok(());
            }
            for t in &rows {
                let label = priority::label(t.priority).to_ascii_uppercase();
                let pt = t.pt_id.as_deref().unwrap_or(&t.id[..8]);
                let due = t.deadline.as_deref().unwrap_or("--");
                println!("[{:8}] {:8}  {}  ({})", label, pt, t.title, due);
            }
            Ok(())
        }
    }
}

fn cmd_scoring(db: &Db, c: ScoringCommand) -> Result<()> {
    match c {
        ScoringCommand::Run(a) => {
            let report = ptask_core::scoring::run_once(db, a.dry_run)?;
            println!(
                "scoring ok — tasks_scored={}{}",
                report.tasks_scored,
                if report.dry_run { " (dry-run)" } else { "" }
            );
            Ok(())
        }
    }
}

fn cmd_distill(db: &Db, a: DistillArgs) -> Result<()> {
    let days = a.days.to_string();
    let report = ptask_distill::run(db, &["--days", &days])?;
    if report.success {
        println!(
            "distill ok ({}ms; {} stdout lines, {} stderr lines)",
            report.duration_ms, report.stdout_lines, report.stderr_lines
        );
        Ok(())
    } else {
        eprintln!(
            "distill FAILED (exit={:?}, {}ms)",
            report.exit_code, report.duration_ms
        );
        if let Some(reason) = &report.soft_failure {
            eprintln!("soft failure: {}", reason);
        }
        if !report.stderr_tail.is_empty() {
            eprintln!("--- stderr tail ---\n{}", report.stderr_tail);
        }
        // A soft failure carries exit_code Some(0); never exit 0 on failure.
        std::process::exit(report.exit_code.filter(|&c| c != 0).unwrap_or(1));
    }
}

fn cmd_backfill(db: &Db) -> Result<()> {
    let n = pt_id::backfill_all(db)?;
    println!("Backfilled PT-N for {} task(s).", n);
    Ok(())
}
