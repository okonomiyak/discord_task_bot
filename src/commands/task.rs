use poise::serenity_prelude::{
    self as serenity, Colour, CreateEmbed, CreateScheduledEvent, EditScheduledEvent,
    ScheduledEventId, ScheduledEventType, Timestamp,
};

use super::{
    RemindChoice, build_pages, due_help, guild, parse_due_with, reply_ephemeral, reply_not_found,
    send_paginated, truncate,
};
use crate::{
    Context, Error,
    db::TaskEdit,
    models::{Priority, Task, TaskQuery, TaskSort, TaskStatus, mentions},
    time::{due_to_utc, format_due, format_duration},
};

// ────────────────────────────────────────────────────────────────────────────
// Choice parameter enums
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, poise::ChoiceParameter)]
pub enum PriorityChoice {
    #[name = "低 🟢"]
    Low,
    #[name = "中 🟡"]
    Medium,
    #[name = "高 🔴"]
    High,
}

impl From<PriorityChoice> for Priority {
    fn from(c: PriorityChoice) -> Self {
        match c {
            PriorityChoice::Low => Priority::Low,
            PriorityChoice::Medium => Priority::Medium,
            PriorityChoice::High => Priority::High,
        }
    }
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum StatusFilterChoice {
    #[name = "すべて"]
    All,
    #[name = "未完了（待機中＋進行中）"]
    Open,
    #[name = "待機中 ⏳"]
    Pending,
    #[name = "進行中 🔄"]
    InProgress,
    #[name = "完了 ✅"]
    Done,
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum StatusUpdateChoice {
    #[name = "待機中 ⏳"]
    Pending,
    #[name = "進行中 🔄"]
    InProgress,
    #[name = "完了 ✅"]
    Done,
}

impl From<StatusUpdateChoice> for TaskStatus {
    fn from(c: StatusUpdateChoice) -> Self {
        match c {
            StatusUpdateChoice::Pending => TaskStatus::Pending,
            StatusUpdateChoice::InProgress => TaskStatus::InProgress,
            StatusUpdateChoice::Done => TaskStatus::Done,
        }
    }
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum SortChoice {
    #[name = "優先度順"]
    Priority,
    #[name = "期限が近い順"]
    DueDate,
    #[name = "新しい順"]
    Created,
}

impl From<SortChoice> for TaskSort {
    fn from(c: SortChoice) -> Self {
        match c {
            SortChoice::Priority => TaskSort::Priority,
            SortChoice::DueDate => TaskSort::DueDate,
            SortChoice::Created => TaskSort::Created,
        }
    }
}

fn collect_reminders(
    r1: Option<RemindChoice>,
    r2: Option<RemindChoice>,
    r3: Option<RemindChoice>,
) -> Vec<i64> {
    let mut secs: Vec<i64> = [r1, r2, r3]
        .into_iter()
        .flatten()
        .map(|r| r.to_seconds())
        .collect();
    secs.sort();
    secs.dedup();
    secs
}

fn format_reminders(reminders: &[(i64, bool)]) -> String {
    if reminders.is_empty() {
        return "なし".to_string();
    }
    reminders
        .iter()
        .map(|(secs, reminded)| {
            if *reminded {
                format!("~~⏰ {}~~ ✔", format_duration(*secs))
            } else {
                format!("⏰ {}", format_duration(*secs))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_assignees(assignees: &[String]) -> String {
    if assignees.is_empty() {
        "未割り当て".to_string()
    } else {
        mentions(assignees)
    }
}

fn event_url(guild_id: &str, event_id: &str) -> String {
    format!("https://discord.com/events/{guild_id}/{event_id}")
}

/// 期限からイベントの開始・終了時刻（1時間）を作る
fn event_times(due: &str) -> Option<(Timestamp, Timestamp)> {
    let start = due_to_utc(due)?;
    let end = start + chrono::Duration::hours(1);
    Some((
        Timestamp::from_unix_timestamp(start.timestamp()).ok()?,
        Timestamp::from_unix_timestamp(end.timestamp()).ok()?,
    ))
}

/// Discord スケジュールイベントを作成し、イベント ID を返す
async fn create_event(
    ctx: Context<'_>,
    guild_id: &str,
    title: &str,
    description: Option<&str>,
    due: &str,
) -> Option<String> {
    let (start, end) = event_times(due)?;
    let builder = CreateScheduledEvent::new(ScheduledEventType::External, title, start)
        .description(truncate(description.unwrap_or(""), 1000))
        .location(truncate(title, 100))
        .end_time(end);

    match guild(guild_id)
        .create_scheduled_event(ctx.serenity_context(), builder)
        .await
    {
        Ok(event) => Some(event.id.to_string()),
        Err(e) => {
            eprintln!("イベント作成エラー: {:?}", e);
            None
        }
    }
}

/// タスクの内容に合わせて Discord スケジュールイベントを更新する
async fn sync_event(ctx: Context<'_>, task: &Task) {
    let Some(event_id) = task
        .discord_event_id
        .as_deref()
        .and_then(|e| e.parse::<u64>().ok())
    else {
        return;
    };
    let mut builder = EditScheduledEvent::new()
        .name(&task.title)
        .description(truncate(task.description.as_deref().unwrap_or(""), 1000));
    if let Some((start, end)) = task.due_date.as_deref().and_then(event_times) {
        builder = builder.start_time(start).end_time(end);
    }
    if let Err(e) = guild(&task.guild_id)
        .edit_scheduled_event(
            ctx.serenity_context(),
            ScheduledEventId::new(event_id),
            builder,
        )
        .await
    {
        eprintln!("イベント更新エラー: {:?}", e);
    }
}

/// タスク一覧を送信する。件数が多い場合はボタンでページ送りできるようにする
async fn send_task_list(ctx: Context<'_>, title: &str, tasks: &[Task]) -> Result<(), Error> {
    let pages = build_pages(title, Colour(0x3498DB), tasks, |task| {
        let has_pending_reminder = task.reminders.iter().any(|(_, reminded)| !reminded);
        let remind_icon = if has_pending_reminder { "⏰ " } else { "" };
        let assignees = if task.assignees.is_empty() {
            format!("作成: <@{}>", task.user_id)
        } else {
            format!("担当: {}", mentions(&task.assignees))
        };
        (
            format!("[{}] {}{}", task.id, remind_icon, truncate(&task.title, 80)),
            format!(
                "{} {} {} | {} | 期限: {}",
                task.status.emoji(),
                task.status.display(),
                task.priority.emoji(),
                assignees,
                format_due(task.due_date.as_deref()),
            ),
        )
    });
    send_paginated(ctx, pages, false).await
}

fn status_filter(filter: Option<StatusFilterChoice>) -> (Option<TaskStatus>, bool) {
    match filter {
        None | Some(StatusFilterChoice::All) => (None, false),
        Some(StatusFilterChoice::Open) => (None, true),
        Some(StatusFilterChoice::Pending) => (Some(TaskStatus::Pending), false),
        Some(StatusFilterChoice::InProgress) => (Some(TaskStatus::InProgress), false),
        Some(StatusFilterChoice::Done) => (Some(TaskStatus::Done), false),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Command group
// ────────────────────────────────────────────────────────────────────────────

/// タスク管理コマンド
#[poise::command(
    slash_command,
    subcommands(
        "add", "list", "search", "view", "status", "assign", "unassign", "edit", "delete"
    )
)]
pub async fn task(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /task add
// ────────────────────────────────────────────────────────────────────────────

/// 新しいタスクを追加する
#[allow(clippy::too_many_arguments)]
#[poise::command(slash_command)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "タスクのタイトル"]
    #[max_length = 100]
    title: String,
    #[description = "タスクの説明"]
    #[max_length = 1000]
    description: Option<String>,
    #[description = "優先度 (デフォルト: 中)"] priority: Option<PriorityChoice>,
    #[description = "期限 (例: 明日 / 金曜 17:00 / 2025-12-31 15:00)"] due_date: Option<String>,
    #[description = "担当者"] assignee: Option<serenity::User>,
    #[description = "リマインダー1"] remind1: Option<RemindChoice>,
    #[description = "リマインダー2"] remind2: Option<RemindChoice>,
    #[description = "リマインダー3"] remind3: Option<RemindChoice>,
    #[description = "Discord スケジュールイベントも作成する (デフォルト: false)"]
    create_event: Option<bool>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let due_date = require_valid_due!(ctx, guild_id, due_date);

    let reminders = collect_reminders(remind1, remind2, remind3);
    if !reminders.is_empty() && due_date.is_none() {
        reply_ephemeral(
            ctx,
            "リマインダーを設定するには期限 (due_date) も指定してください。",
        )
        .await?;
        return Ok(());
    }
    if create_event == Some(true) && due_date.is_none() {
        reply_ephemeral(
            ctx,
            "イベントを作成するには期限 (due_date) も指定してください。",
        )
        .await?;
        return Ok(());
    }

    let priority = priority.map(Priority::from).unwrap_or(Priority::Medium);
    let db = &ctx.data().db;

    let task_id = db
        .add_task(
            ctx.author().id.to_string(),
            guild_id.clone(),
            ctx.channel_id().to_string(),
            title.clone(),
            description.clone(),
            priority.clone(),
            due_date.clone(),
            reminders.clone(),
        )
        .await?;

    if let Some(user) = &assignee {
        db.assign(task_id, guild_id.clone(), vec![user.id.to_string()])
            .await?;
    }

    // Discord スケジュールイベント作成
    let mut event_link: Option<String> = None;
    if let (Some(true), Some(due)) = (create_event, due_date.as_deref()) {
        match self::create_event(ctx, &guild_id, &title, description.as_deref(), due).await {
            Some(eid) => {
                event_link = Some(format!(
                    "[Discord イベント]({})",
                    event_url(&guild_id, &eid)
                ));
                db.set_event_id(task_id, eid).await?;
            }
            None => {
                event_link = Some(
                    "⚠️ 作成に失敗しました（ボットの「イベントの管理」権限と、期限が未来の日時かを確認してください）"
                        .to_string(),
                );
            }
        }
    }

    let remind_str = format_reminders(&reminders.iter().map(|s| (*s, false)).collect::<Vec<_>>());

    let mut embed = CreateEmbed::new()
        .title("✅ タスクを追加しました")
        .colour(Colour(0x2ECC71))
        .field("ID", task_id.to_string(), true)
        .field("タイトル", &title, true)
        .field(
            "優先度",
            format!("{} {}", priority.emoji(), priority.display()),
            true,
        )
        .field("説明", description.as_deref().unwrap_or("なし"), false)
        .field("期限", format_due(due_date.as_deref()), true)
        .field(
            "担当者",
            assignee
                .map(|u| format!("<@{}>", u.id))
                .unwrap_or_else(|| "未割り当て".to_string()),
            true,
        )
        .field("リマインダー", remind_str, true);

    if let Some(link) = event_link {
        embed = embed.field("イベント", link, false);
    }

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /task list
// ────────────────────────────────────────────────────────────────────────────

/// タスク一覧を表示する
#[poise::command(slash_command)]
pub async fn list(
    ctx: Context<'_>,
    #[description = "絞り込み (デフォルト: すべて)"] filter: Option<StatusFilterChoice>,
    #[description = "担当者で絞り込み"] assignee: Option<serenity::User>,
    #[description = "並び順 (デフォルト: 優先度順)"] sort: Option<SortChoice>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let (status, open_only) = status_filter(filter);

    let query = TaskQuery {
        status,
        assignee: assignee.as_ref().map(|u| u.id.to_string()),
        keyword: None,
        sort: sort.map(TaskSort::from).unwrap_or_default(),
    };
    let mut tasks = ctx.data().db.list_tasks(guild_id, query).await?;
    if open_only {
        tasks.retain(|t| t.status != TaskStatus::Done);
    }

    if tasks.is_empty() {
        reply_ephemeral(
            ctx,
            "該当するタスクはありません。`/task add` でタスクを追加してください。",
        )
        .await?;
        return Ok(());
    }

    let title = match &assignee {
        Some(user) => format!("📋 タスク一覧（担当: {}）", user.name),
        None => "📋 タスク一覧".to_string(),
    };
    send_task_list(ctx, &title, &tasks).await
}

// ────────────────────────────────────────────────────────────────────────────
// /task search
// ────────────────────────────────────────────────────────────────────────────

/// タイトル・説明からタスクを検索する
#[poise::command(slash_command)]
pub async fn search(
    ctx: Context<'_>,
    #[description = "検索キーワード"]
    #[min_length = 1]
    #[max_length = 100]
    keyword: String,
    #[description = "絞り込み (デフォルト: すべて)"] filter: Option<StatusFilterChoice>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let (status, open_only) = status_filter(filter);

    let query = TaskQuery {
        status,
        keyword: Some(keyword.clone()),
        ..Default::default()
    };
    let mut tasks = ctx.data().db.list_tasks(guild_id, query).await?;
    if open_only {
        tasks.retain(|t| t.status != TaskStatus::Done);
    }

    if tasks.is_empty() {
        reply_ephemeral(
            ctx,
            format!("「{}」に一致するタスクはありません。", keyword),
        )
        .await?;
        return Ok(());
    }

    send_task_list(
        ctx,
        &format!("🔍 「{}」の検索結果", truncate(&keyword, 50)),
        &tasks,
    )
    .await
}

// ────────────────────────────────────────────────────────────────────────────
// /task view
// ────────────────────────────────────────────────────────────────────────────

/// タスクの詳細を表示する
#[poise::command(slash_command)]
pub async fn view(ctx: Context<'_>, #[description = "タスクID"] id: i64) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);

    let Some(task) = ctx.data().db.get_task(id, guild_id.clone()).await? else {
        return reply_not_found(ctx, id).await;
    };

    let color = match task.status {
        TaskStatus::Pending => Colour(0xF1C40F),
        TaskStatus::InProgress => Colour(0x3498DB),
        TaskStatus::Done => Colour(0x2ECC71),
    };

    let mut embed = CreateEmbed::new()
        .title(format!("📌 タスク #{}", task.id))
        .colour(color)
        .field("タイトル", &task.title, false)
        .field(
            "ステータス",
            format!("{} {}", task.status.emoji(), task.status.display()),
            true,
        )
        .field(
            "優先度",
            format!("{} {}", task.priority.emoji(), task.priority.display()),
            true,
        )
        .field("説明", task.description.as_deref().unwrap_or("なし"), false)
        .field("期限", format_due(task.due_date.as_deref()), true)
        .field("作成者", format!("<@{}>", task.user_id), true)
        .field("担当者", format_assignees(&task.assignees), true)
        .field("リマインダー", format_reminders(&task.reminders), true)
        .field("作成日時", &task.created_at, true);

    if let Some(eid) = &task.discord_event_id {
        embed = embed.field(
            "イベント",
            format!("[Discord イベント]({})", event_url(&guild_id, eid)),
            false,
        );
    }

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /task status
// ────────────────────────────────────────────────────────────────────────────

/// タスクのステータスを更新する
#[poise::command(slash_command)]
pub async fn status(
    ctx: Context<'_>,
    #[description = "タスクID"] id: i64,
    #[description = "新しいステータス"] new_status: StatusUpdateChoice,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let task_status = TaskStatus::from(new_status);
    let display = format!("{} {}", task_status.emoji(), task_status.display());

    let updated = ctx
        .data()
        .db
        .update_task_status(id, guild_id, task_status)
        .await?;

    if updated == 0 {
        reply_not_found(ctx, id).await?;
    } else {
        ctx.say(format!(
            "タスク #{} のステータスを {} に更新しました！",
            id, display
        ))
        .await?;
    }

    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /task assign, /task unassign
// ────────────────────────────────────────────────────────────────────────────

/// タスクに担当者を追加する
#[poise::command(slash_command)]
pub async fn assign(
    ctx: Context<'_>,
    #[description = "タスクID"] id: i64,
    #[description = "担当者"] user: serenity::User,
    #[description = "担当者2"] user2: Option<serenity::User>,
    #[description = "担当者3"] user3: Option<serenity::User>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);

    let mut user_ids: Vec<String> = [Some(user), user2, user3]
        .into_iter()
        .flatten()
        .map(|u| u.id.to_string())
        .collect();
    user_ids.sort();
    user_ids.dedup();

    match ctx.data().db.assign(id, guild_id, user_ids.clone()).await? {
        None => reply_not_found(ctx, id).await?,
        Some(_) => {
            ctx.say(format!(
                "👤 タスク #{} の担当者に {} を追加しました！",
                id,
                mentions(&user_ids)
            ))
            .await?;
        }
    }
    Ok(())
}

/// タスクの担当者を外す
#[poise::command(slash_command)]
pub async fn unassign(
    ctx: Context<'_>,
    #[description = "タスクID"] id: i64,
    #[description = "外す担当者"] user: serenity::User,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);

    match ctx
        .data()
        .db
        .unassign(id, guild_id, user.id.to_string())
        .await?
    {
        None => reply_not_found(ctx, id).await?,
        Some(0) => {
            reply_ephemeral(
                ctx,
                format!("<@{}> はタスク #{} の担当者ではありません。", user.id, id),
            )
            .await?
        }
        Some(_) => {
            ctx.say(format!(
                "👋 タスク #{} の担当者から <@{}> を外しました。",
                id, user.id
            ))
            .await?;
        }
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /task edit
// ────────────────────────────────────────────────────────────────────────────

/// タスクを編集する（remind を1つでも指定するとリマインダーが全置き換えされます）
#[allow(clippy::too_many_arguments)]
#[poise::command(slash_command)]
pub async fn edit(
    ctx: Context<'_>,
    #[description = "タスクID"] id: i64,
    #[description = "新しいタイトル"]
    #[max_length = 100]
    title: Option<String>,
    #[description = "新しい説明"]
    #[max_length = 1000]
    description: Option<String>,
    #[description = "新しい優先度"] priority: Option<PriorityChoice>,
    #[description = "新しい期限 (例: 明日 / 金曜 17:00 / 2025-12-31 15:00)"] due_date: Option<
        String,
    >,
    #[description = "リマインダー1 (指定するとリマインダーが全置き換え)"] remind1: Option<
        RemindChoice,
    >,
    #[description = "リマインダー2"] remind2: Option<RemindChoice>,
    #[description = "リマインダー3"] remind3: Option<RemindChoice>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let due_date = require_valid_due!(ctx, guild_id, due_date);

    let remind_changed = remind1.is_some() || remind2.is_some() || remind3.is_some();
    if title.is_none()
        && description.is_none()
        && priority.is_none()
        && due_date.is_none()
        && !remind_changed
    {
        reply_ephemeral(ctx, "変更する項目を少なくとも1つ指定してください。").await?;
        return Ok(());
    }

    let event_fields_changed = title.is_some() || description.is_some() || due_date.is_some();
    let edit = TaskEdit {
        title,
        description,
        priority: priority.map(Priority::from),
        due_date,
        reminders: remind_changed.then(|| collect_reminders(remind1, remind2, remind3)),
    };

    let Some(task) = ctx.data().db.edit_task(id, guild_id, edit).await? else {
        return reply_not_found(ctx, id).await;
    };

    if remind_changed && task.due_date.is_none() {
        ctx.say(format!(
            "✏️ タスク #{} を更新しました！（⚠️ 期限が未設定のためリマインダーは通知されません）",
            id
        ))
        .await?;
    } else {
        ctx.say(format!("✏️ タスク #{} を更新しました！", id))
            .await?;
    }

    if event_fields_changed {
        sync_event(ctx, &task).await;
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /task delete
// ────────────────────────────────────────────────────────────────────────────

/// タスクを削除する
#[poise::command(slash_command)]
pub async fn delete(ctx: Context<'_>, #[description = "タスクID"] id: i64) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);

    let Some(task) = ctx.data().db.delete_task(id, guild_id.clone()).await? else {
        return reply_not_found(ctx, id).await;
    };

    // Discord イベントも削除
    if let Some(eid) = task.discord_event_id.and_then(|e| e.parse::<u64>().ok())
        && let Err(e) = guild(&guild_id)
            .delete_scheduled_event(ctx.serenity_context(), ScheduledEventId::new(eid))
            .await
    {
        eprintln!("イベント削除エラー: {:?}", e);
    }

    ctx.say(format!(
        "🗑️ タスク #{}「{}」を削除しました。",
        id, task.title
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_unique_sorted_reminders() {
        let secs = collect_reminders(
            Some(RemindChoice::OneDay),
            Some(RemindChoice::ThirtyMin),
            Some(RemindChoice::OneDay),
        );
        assert_eq!(secs, vec![30 * 60, 24 * 3600]);
    }
}
