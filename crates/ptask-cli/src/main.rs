//! `pt` — pTask command-line interface.
//!
//! v0.1.0 covers parity with the legacy Python `cli.py`:
//!   pt add <title> [-p PRIORITY] [-d DESCRIPTION] [--deadline ISO] [--reason TEXT]
//!   pt list        [-s STATUS]   [-p PRIORITY]    [-n LIMIT]       [-v]
//!   pt done <query>
//!
//! Future phases add `next`, `edit`, `show`, `rm`, `view`, `serve`, `bot`, `tui`.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ptask_core::{Db, Extensions, NewTask, dag, priority, pt_id, quickadd, tasks, views};

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
    /// Show ready-to-start tasks (all dependencies done).
    Next(NextArgs),
    /// Manage saved views.
    #[command(subcommand)]
    View(ViewCommand),
    /// Launch the terminal UI (ratatui).
    Tui,
    /// One-shot backfill PT-N for any tasks lacking one.
    Backfill,
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
    /// Inline tokens: @label, #project, p1..p4, ~30m/~2h/~1d, !HH:MM,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Lightweight tracing: env-controlled, off by default.
    let filter = tracing_subscriber::EnvFilter::try_from_env("PTASK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let db = match cli.db.as_deref() {
        Some(p) => Db::open(p).with_context(|| format!("opening db at {}", p))?,
        None => Db::open_default().context("opening default db")?,
    };

    match cli.command {
        Some(Command::Add(a)) => cmd_add(&db, a),
        Some(Command::List(a)) => cmd_list(&db, a),
        Some(Command::Done(a)) => cmd_done(&db, a),
        Some(Command::Next(a)) => cmd_next(&db, a),
        Some(Command::View(c)) => cmd_view(&db, c),
        Some(Command::Tui) => ptask_tui::run(db),
        Some(Command::Backfill) => cmd_backfill(&db),
        None => {
            // No subcommand → quick help banner. TUI lands in v0.3.0.
            println!("pt {} — sovereign task manager.", ptask_core::VERSION);
            println!("Try: pt add \"...\" | pt list | pt done PT-N | pt --help");
            Ok(())
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

fn cmd_backfill(db: &Db) -> Result<()> {
    let n = pt_id::backfill_all(db)?;
    println!("Backfilled PT-N for {} task(s).", n);
    Ok(())
}
