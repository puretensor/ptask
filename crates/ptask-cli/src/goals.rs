//! CLI surface for the goal tree.

use anyhow::Result;
use clap::{Args, Subcommand};
use ptask_core::Db;
use ptask_core::event_log::EventCtx;
use ptask_core::goals::{self, Goal, GoalListItem, GoalShow, GoalTask};

use crate::ui;

#[derive(Subcommand, Debug)]
pub enum GoalCommand {
    /// Create a goal.
    Add(AddArgs),
    /// List goals in tree order (parent before children, siblings by seq).
    #[command(name = "ls", alias = "list")]
    Ls(LsArgs),
    /// Show one goal: ancestors, children, tasks, subtree rollup.
    Show(IdArgs),
    /// Direct-link a task to a goal.
    Link(LinkArgs),
    /// Remove a task's direct goal link (inheritance remains).
    Unlink(UnlinkArgs),
    /// Move a goal under a new parent (rejects self and cycles).
    SetParent(SetParentArgs),
    /// Mark a goal achieved.
    Done(IdArgs),
    /// Mark a goal abandoned.
    Abandon(IdArgs),
    /// List open tasks with no effective goal.
    Orphans,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Goal title.
    pub title: String,
    /// Why this goal exists.
    #[arg(long)]
    pub why: Option<String>,
    /// Parent goal (`G-n`).
    #[arg(long)]
    pub parent: Option<String>,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    /// Include achieved and abandoned goals.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct IdArgs {
    /// `G-n` or the row uuid.
    pub id: String,
}

#[derive(Args, Debug)]
pub struct LinkArgs {
    /// Task handle (`PT-n`).
    pub task: String,
    /// Goal (`G-n`).
    pub goal: String,
}

#[derive(Args, Debug)]
pub struct UnlinkArgs {
    /// Task handle (`PT-n`).
    pub task: String,
}

#[derive(Args, Debug)]
pub struct SetParentArgs {
    /// Goal to move (`G-n`).
    pub id: String,
    /// New parent (`G-n`).
    pub parent: String,
}

fn emit_goal(goal: &Goal, json: bool, text: impl FnOnce()) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&goal.to_json())?);
    } else {
        text();
    }
    Ok(())
}

fn print_goal_line(g: &Goal, extra: &str) {
    println!(
        "{}",
        ui::outcome(
            match g.status.as_str() {
                "achieved" => ui::Status::Ok,
                "abandoned" => ui::Status::Mute,
                _ => ui::Status::Busy,
            },
            &g.status,
            &g.g_id(),
            &g.title,
            extra
        )
    );
}

pub fn run(db: &Db, cmd: GoalCommand, ctx: EventCtx, json: bool) -> Result<()> {
    match cmd {
        GoalCommand::Add(a) => {
            let g = goals::add(db, &a.title, a.why.as_deref(), a.parent.as_deref(), &ctx)?;
            emit_goal(&g, json, || print_goal_line(&g, "created"))
        }
        GoalCommand::Ls(a) => {
            let items = goals::list(db, a.all)?;
            if json {
                let v: Vec<serde_json::Value> = items.iter().map(GoalListItem::to_json).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            if items.is_empty() {
                println!("{}", ui::empty("no goals"));
                return Ok(());
            }
            for item in &items {
                let indent = "  ".repeat(item.depth as usize);
                let why = item
                    .goal
                    .why
                    .as_deref()
                    .map(|w| format!("  {w}"))
                    .unwrap_or_default();
                println!(
                    "{indent}{}  {}  {}{why}",
                    item.goal.g_id(),
                    item.goal.title,
                    item.goal.status
                );
            }
            Ok(())
        }
        GoalCommand::Show(a) => {
            let shown = goals::show(db, &a.id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&shown.to_json())?);
                return Ok(());
            }
            print_human_show(&shown);
            Ok(())
        }
        GoalCommand::Link(a) => {
            let task = goals::link(db, &a.task, &a.goal, &ctx)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": true,
                        "pt_id": task.pt_id,
                        "goal": a.goal,
                    }))?
                );
                return Ok(());
            }
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Ok,
                    "linked",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    &a.goal
                )
            );
            Ok(())
        }
        GoalCommand::Unlink(a) => {
            let task = goals::unlink(db, &a.task, &ctx)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": true,
                        "pt_id": task.pt_id,
                    }))?
                );
                return Ok(());
            }
            println!(
                "{}",
                ui::outcome(
                    ui::Status::Mute,
                    "unlinked",
                    task.pt_id.as_deref().unwrap_or(""),
                    &task.title,
                    ""
                )
            );
            Ok(())
        }
        GoalCommand::SetParent(a) => {
            let g = goals::set_parent(db, &a.id, &a.parent, &ctx)?;
            emit_goal(&g, json, || {
                print_goal_line(&g, &format!("parent {}", a.parent))
            })
        }
        GoalCommand::Done(a) => {
            let g = goals::mark_achieved(db, &a.id, &ctx)?;
            emit_goal(&g, json, || print_goal_line(&g, "achieved"))
        }
        GoalCommand::Abandon(a) => {
            let g = goals::mark_abandoned(db, &a.id, &ctx)?;
            emit_goal(&g, json, || print_goal_line(&g, "abandoned"))
        }
        GoalCommand::Orphans => {
            let items = goals::orphans(db)?;
            if json {
                let v: Vec<serde_json::Value> = items.iter().map(GoalTask::to_json).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            if items.is_empty() {
                println!("{}", ui::empty("no orphan tasks"));
                return Ok(());
            }
            for t in &items {
                println!(
                    "{}",
                    ui::outcome(
                        ui::Status::Busy,
                        "orphan",
                        t.pt_id.as_deref().unwrap_or(""),
                        &t.title,
                        ""
                    )
                );
            }
            Ok(())
        }
    }
}

fn print_human_show(shown: &GoalShow) {
    let g = &shown.goal;
    print_goal_line(g, "");
    let mut pairs: Vec<(&str, String)> = vec![("status", g.status.clone())];
    if let Some(p) = &g.parent {
        pairs.push(("parent", p.clone()));
    }
    if let Some(w) = &g.why {
        pairs.push(("why", w.clone()));
    }
    pairs.push(("uuid", ui::dim(&g.uuid, ui::Ink::Slate)));
    for l in ui::kv(&pairs, 12) {
        println!("    {}", l.trim_start());
    }
    if !shown.chain.is_empty() {
        println!();
        println!("{}", ui::section("chain", ui::Ink::Steel, "nearest first"));
        for a in &shown.chain {
            println!("  {}  {}", a.g_id(), a.title);
        }
    }
    if !shown.children.is_empty() {
        println!();
        println!("{}", ui::section("children", ui::Ink::Cyan, ""));
        for c in &shown.children {
            println!("  {}  {}", c.g_id(), c.title);
        }
    }
    if !shown.tasks.is_empty() {
        println!();
        println!("{}", ui::section("tasks", ui::Ink::Paper, "effective goal"));
        for t in &shown.tasks {
            println!("  {}  {}", t.pt_id.as_deref().unwrap_or(&t.id), t.title);
        }
    }
    println!();
    println!(
        "{}",
        ui::section(
            "rollup",
            ui::Ink::Green,
            &format!("open {} · done {}", shown.rollup.open, shown.rollup.done)
        )
    );
}
