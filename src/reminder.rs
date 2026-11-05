use std::sync::Arc;

use poise::serenity_prelude::{ChannelId, CreateMessage, Http};

use crate::{
    Error,
    db::Database,
    models::{Task, mentions},
    time::{due_to_utc, format_due, format_duration},
};

/// 60秒ごとにリマインダー・期限切れ・宿題の通知をチェックするループ
pub async fn run(db: Database, http: Arc<Http>, web_url: Option<String>) {
    let web = web_url.as_deref();
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    loop {
        interval.tick().await;
        if let Err(e) = check_reminders(&db, &http).await {
            eprintln!("リマインダーチェックエラー: {:?}", e);
        }
        if let Err(e) = check_overdue(&db, &http).await {
            eprintln!("期限切れチェックエラー: {:?}", e);
        }
        if let Err(e) = crate::hw_notify::generate_repeats(&db).await {
            eprintln!("毎週の宿題の登録エラー: {:?}", e);
        }
        if let Err(e) = crate::hw_notify::check_reminders(&db, &http, web).await {
            eprintln!("宿題通知チェックエラー: {:?}", e);
        }
        if let Err(e) = crate::hw_notify::check_summaries(&db, &http, web).await {
            eprintln!("宿題まとめ投稿エラー: {:?}", e);
        }
    }
}

/// タスクの通知を送る（タイトルに書かれた URL のカードはサーバーの設定に従う）
async fn send_notice(
    db: &Database,
    http: &Arc<Http>,
    channel_id: ChannelId,
    task: &Task,
    content: String,
) -> Result<(), Error> {
    let hide = db
        .hw_settings(task.guild_id.clone())
        .await
        .map(|s| s.hide_link_previews)
        .unwrap_or(true);
    let msg = crate::hw_notify::suppress_link_previews(CreateMessage::new().content(content), hide);
    channel_id.send_message(http, msg).await?;
    Ok(())
}

fn channel_of(task: &Task) -> Option<ChannelId> {
    match task.channel_id.as_deref()?.parse::<u64>() {
        Ok(id) if id > 0 => Some(ChannelId::new(id)),
        _ => None,
    }
}

pub async fn check_reminders(db: &Database, http: &Arc<Http>) -> Result<(), Error> {
    let now = chrono::Utc::now();

    for reminder in db.get_pending_reminders().await? {
        let task = &reminder.task;
        let Some(due) = task.due_date.as_deref().and_then(due_to_utc) else {
            eprintln!("タスク #{} の期限形式が不正: {:?}", task.id, task.due_date);
            continue;
        };

        let notify_at = due - chrono::Duration::seconds(reminder.remind_before);
        if now < notify_at {
            continue;
        }
        // ボット停止中などで期限を過ぎてしまったものは送らない（期限切れ通知に任せる）
        if now >= due {
            db.mark_reminder_sent(reminder.reminder_id).await?;
            continue;
        }

        let Some(channel_id) = channel_of(task) else {
            continue;
        };

        let content = format!(
            "⏰ **タスクリマインダー**\n\
             {} タスク #{}「**{}**」の期限**{}**です！\n\
             📅 期限: {}",
            mentions(&task.notify_targets()),
            task.id,
            task.title,
            format_duration(reminder.remind_before),
            format_due(task.due_date.as_deref()),
        );

        if let Err(e) = send_notice(db, http, channel_id, task, content).await {
            eprintln!(
                "リマインダー送信エラー (reminder #{}): {:?}",
                reminder.reminder_id, e
            );
        } else {
            db.mark_reminder_sent(reminder.reminder_id).await?;
        }
    }

    Ok(())
}

pub async fn check_overdue(db: &Database, http: &Arc<Http>) -> Result<(), Error> {
    let now = chrono::Utc::now();

    for task in db.get_overdue_candidates().await? {
        let Some(due) = task.due_date.as_deref().and_then(due_to_utc) else {
            continue;
        };
        if now < due {
            continue;
        }
        let Some(channel_id) = channel_of(&task) else {
            continue;
        };

        let content = format!(
            "🚨 **期限切れ**\n\
             {} タスク #{}「**{}**」の期限が過ぎました（{} {}）\n\
             📅 期限: {}\n\
             完了したら `/task status id:{} new_status:完了` で更新してください。",
            mentions(&task.notify_targets()),
            task.id,
            task.title,
            task.status.emoji(),
            task.status.display(),
            format_due(task.due_date.as_deref()),
            task.id,
        );

        if let Err(e) = send_notice(db, http, channel_id, &task, content).await {
            eprintln!("期限切れ通知の送信エラー (task #{}): {:?}", task.id, e);
        } else {
            db.mark_overdue_notified(task.id).await?;
        }
    }

    Ok(())
}
