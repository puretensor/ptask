//! Command surface + dispatchers.

use ptask_core::Db;
use ptask_core::tasks::DoneOutcome;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tracing::info;

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "pTask commands. PT-N IDs are sticky — share them in any chat."
)]
pub enum PtCommand {
    #[command(
        description = "Quick-add a new task. Inline tokens: @label #project p1..p4 ~Nm //desc, plus date phrases."
    )]
    Add(String),
    #[command(description = "List tasks. Optional Todoist-style filter DSL.")]
    List(String),
    #[command(
        description = "Mark done by PT-N or title substring. Recurring tasks advance in place."
    )]
    Done(String),
    #[command(description = "DAG-ready tasks (all dependencies satisfied).")]
    Next(String),
    #[command(description = "Show this help.")]
    Help,
}

pub async fn dispatch(
    bot: Bot,
    msg: Message,
    cmd: PtCommand,
    db: Db,
) -> Result<(), teloxide::RequestError> {
    let chat_id = msg.chat.id;
    info!(
        target: "ptask::bot",
        chat_id = chat_id.0,
        cmd = ?cmd,
        "command received"
    );
    match cmd {
        PtCommand::Help => {
            let text = format!(
                "{}\n\nExamples:\n  /add Buy bread tomorrow 10am @home p1 ~30m\n  /list today | overdue\n  /done PT-42\n  /next",
                PtCommand::descriptions()
            );
            send(&bot, chat_id, text).await?;
        }
        PtCommand::Add(text) => handle_add(&bot, chat_id, &db, &text).await?,
        PtCommand::List(filter) => handle_list(&bot, chat_id, &db, &filter).await?,
        PtCommand::Done(query) => handle_done(&bot, chat_id, &db, &query).await?,
        PtCommand::Next(rest) => handle_next(&bot, chat_id, &db, &rest).await?,
    }
    Ok(())
}

async fn handle_add(
    bot: &Bot,
    chat_id: ChatId,
    db: &Db,
    text: &str,
) -> Result<(), teloxide::RequestError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        send(bot, chat_id, "usage: /add <quick-add text>").await?;
        return Ok(());
    }
    let q = match ptask_core::quickadd::parse(trimmed) {
        Ok(q) => q,
        Err(e) => {
            send(bot, chat_id, format!("parse failed: {}", e)).await?;
            return Ok(());
        }
    };
    let new = ptask_core::NewTask {
        title: q.title.clone(),
        description: q.description.clone(),
        priority: q.priority.unwrap_or(2),
        deadline: q.deadline.clone(),
        source_type: "telegram".into(),
        ai_confidence: 1.0,
        ai_reasoning: String::new(),
    };
    let ext = ptask_core::Extensions {
        labels: q.labels.clone(),
        project: q.project.clone(),
        duration_min: q.duration_min,
        planned_at: None,
        energy: None,
        recurrence: q.recurrence.clone(),
    };
    match ptask_core::tasks::create_with_extensions(db, new, ext) {
        Ok(t) => {
            let pt = t.pt_id.as_deref().unwrap_or("?");
            let mut msg = format!("✓ {} {}", pt, t.title);
            if let Some(d) = &t.deadline {
                msg.push_str(&format!("\n  due: {}", d));
            }
            if let Some(rec) = &q.recurrence {
                msg.push_str(&format!("\n  recurring: {}", rec.original_input));
            }
            send(bot, chat_id, msg).await?;
        }
        Err(e) => {
            send(bot, chat_id, format!("create failed: {}", e)).await?;
        }
    }
    Ok(())
}

async fn handle_list(
    bot: &Bot,
    chat_id: ChatId,
    db: &Db,
    filter: &str,
) -> Result<(), teloxide::RequestError> {
    let expr = if filter.trim().is_empty() {
        None
    } else {
        match ptask_core::filter::parse(filter) {
            Ok(e) => Some(e),
            Err(e) => {
                send(bot, chat_id, format!("filter parse failed: {}", e)).await?;
                return Ok(());
            }
        }
    };
    let rows = match ptask_core::tasks::list_with_filter(
        db,
        expr.as_ref(),
        if expr.is_some() {
            None
        } else {
            Some("pending")
        },
        None,
        20,
    ) {
        Ok(r) => r,
        Err(e) => {
            send(bot, chat_id, format!("list failed: {}", e)).await?;
            return Ok(());
        }
    };
    if rows.is_empty() {
        send(bot, chat_id, "no tasks").await?;
        return Ok(());
    }
    let mut out = String::new();
    for t in &rows {
        let pt = t.pt_id.as_deref().unwrap_or("?");
        let label = ptask_core::priority::label(t.priority);
        out.push_str(&format!("{} [{}] {}\n", pt, label, t.title));
    }
    out.push_str(&format!("\n{} task(s)", rows.len()));
    send(bot, chat_id, out).await?;
    Ok(())
}

async fn handle_done(
    bot: &Bot,
    chat_id: ChatId,
    db: &Db,
    query: &str,
) -> Result<(), teloxide::RequestError> {
    let q = query.trim();
    if q.is_empty() {
        send(bot, chat_id, "usage: /done <PT-N | substring>").await?;
        return Ok(());
    }
    let task = match ptask_core::tasks::resolve(db, q) {
        Ok(t) => t,
        Err(e) => {
            send(bot, chat_id, format!("{}", e)).await?;
            return Ok(());
        }
    };
    let pt = task.pt_id.as_deref().unwrap_or("?");
    match ptask_core::tasks::mark_done(db, &task) {
        Ok(DoneOutcome::Completed) => {
            send(bot, chat_id, format!("✓ done: {} {}", pt, task.title)).await?;
        }
        Ok(DoneOutcome::Advanced { next_deadline }) => {
            send(
                bot,
                chat_id,
                format!(
                    "↻ advanced: {} {}\n  next: {}",
                    pt, task.title, next_deadline
                ),
            )
            .await?;
        }
        Err(e) => {
            send(bot, chat_id, format!("done failed: {}", e)).await?;
        }
    }
    Ok(())
}

async fn handle_next(
    bot: &Bot,
    chat_id: ChatId,
    db: &Db,
    rest: &str,
) -> Result<(), teloxide::RequestError> {
    let limit: usize = rest.trim().parse().unwrap_or(10);
    let rows = match ptask_core::dag::next_ready(db, limit) {
        Ok(r) => r,
        Err(e) => {
            send(bot, chat_id, format!("next failed: {}", e)).await?;
            return Ok(());
        }
    };
    if rows.is_empty() {
        send(bot, chat_id, "no ready tasks").await?;
        return Ok(());
    }
    let mut out = String::new();
    for t in &rows {
        let pt = t.pt_id.as_deref().unwrap_or("?");
        let label = ptask_core::priority::label(t.priority);
        out.push_str(&format!("{} [{}] {}\n", pt, label, t.title));
    }
    out.push_str(&format!("\n{} ready", rows.len()));
    send(bot, chat_id, out).await?;
    Ok(())
}

async fn send(
    bot: &Bot,
    chat_id: ChatId,
    text: impl Into<String>,
) -> Result<(), teloxide::RequestError> {
    // Plain text — none of pTask's output needs HTML or Markdown rendering.
    bot.send_message(chat_id, text.into()).await?;
    Ok(())
}
