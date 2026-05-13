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
use ptask_core::{Db, NewTask, priority, pt_id, tasks};

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
    /// One-shot backfill PT-N for any tasks lacking one.
    Backfill,
}

#[derive(clap::Args, Debug)]
struct AddArgs {
    /// Task title.
    title: String,
    /// Priority: low|normal|high|urgent|critical or 1..=5. Default: normal.
    #[arg(short = 'p', long = "priority", default_value = "normal")]
    priority: String,
    /// Task description (long-form body).
    #[arg(short = 'd', long = "description", default_value = "")]
    description: String,
    /// Deadline (ISO date, e.g. 2026-05-20).
    #[arg(long = "deadline")]
    deadline: Option<String>,
    /// Why this task was created — stored as ai_reasoning.
    #[arg(long = "reason")]
    reason: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ListArgs {
    /// Filter by status.
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
    let p = priority::parse(&a.priority).map_err(anyhow::Error::msg)?;
    let task = tasks::create(
        db,
        NewTask {
            title: a.title.clone(),
            description: a.description.clone(),
            priority: p,
            deadline: a.deadline.clone(),
            source_type: "claude_code".into(),
            ai_confidence: 1.0,
            ai_reasoning: a.reason.unwrap_or_default(),
        },
    )?;
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
    Ok(())
}

fn cmd_list(db: &Db, a: ListArgs) -> Result<()> {
    let p = a
        .priority
        .as_deref()
        .map(priority::parse)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let status_filter = if a.status == "all" {
        None
    } else {
        Some(a.status.as_str())
    };
    let rows = tasks::list(db, status_filter, p, a.limit)?;
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
    tasks::mark_done(db, &task)?;
    println!(
        "Marked done: {} {}",
        task.pt_id.as_deref().unwrap_or(""),
        task.title
    );
    Ok(())
}

fn cmd_backfill(db: &Db) -> Result<()> {
    let n = pt_id::backfill_all(db)?;
    println!("Backfilled PT-N for {} task(s).", n);
    Ok(())
}
