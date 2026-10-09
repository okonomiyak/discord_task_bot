use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use poise::serenity_prelude::{ChannelId, Colour, CreateEmbed, CreateMessage, Http, UserId};

use crate::{
    Error,
    db::Database,
    models::{Homework, HwSettings},
    time::{DUE_FORMAT, due_to_utc, local_now, parse_time},
};

/// 事前通知の最大（1週間）＋余裕
const LOOKAHEAD_DAYS: i64 = 8;
/// まとめ投稿の時刻を過ぎてからこの時間内なら投稿する（ボット再起動直後の取りこぼし対策）
const SUMMARY_GRACE_MINUTES: i64 = 30;
/// まとめ投稿の対象期間（日）
const SUMMARY_DAYS: i64 = 7;

/// 今この通知を送るべきか
///
/// - 通知時刻を過ぎていて、期限はまだ来ていない
/// - チャンネル通知は、通知時刻より後に登録された宿題には送らない（登録時のメッセージで分かるため）
fn should_notify(
    due: DateTime<Utc>,
    remind_before: i64,
    created_at: i64,
    now: DateTime<Utc>,
    skip_if_registered_late: bool,
) -> bool {
    let notify_at = due - Duration::seconds(remind_before);
    if now < notify_at || now >= due {
        return false;
    }
    !(skip_if_registered_late && created_at >= notify_at.timestamp())
}

fn reminder_line(hw: &Homework) -> String {
    let unix = due_to_utc(&hw.due_date).map(|d| d.timestamp()).unwrap_or(0);
    format!(
        "`#{}` **{}**｜{} — <t:{unix}:f>（<t:{unix}:R>）",
        hw.id, hw.subject, hw.title
    )
}

/// 期限前の通知（チャンネル・個人 DM）をまとめて送る
pub async fn check_reminders(db: &Database, http: &Arc<Http>) -> Result<(), Error> {
    let now = Utc::now();
    let local = local_now();
    let candidates = db
        .hw_due_between(
            local.format(DUE_FORMAT).to_string(),
            (local + Duration::days(LOOKAHEAD_DAYS))
                .format(DUE_FORMAT)
                .to_string(),
        )
        .await?;
    if candidates.is_empty() {
        return Ok(());
    }

    let mut settings: HashMap<String, HwSettings> = HashMap::new();
    let mut subscribers: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    // 1回のチェックで同じ宛先への通知は1通にまとめる
    let mut channel_msgs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dm_msgs: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for hw in &candidates {
        let Some(due) = due_to_utc(&hw.due_date) else {
            continue;
        };

        if !settings.contains_key(&hw.guild_id) {
            let s = db.hw_settings(hw.guild_id.clone()).await?;
            settings.insert(hw.guild_id.clone(), s);
        }
        let s = &settings[&hw.guild_id];
        if let Some(secs) = s.remind_before
            && should_notify(due, secs, hw.created_at, now, true)
            && db
                .hw_claim_notification(hw.id, "channel".to_string(), secs)
                .await?
        {
            let channel = s
                .channel_id
                .clone()
                .unwrap_or_else(|| hw.channel_id.clone());
            channel_msgs.entry(channel).or_default().push(format!(
                "{}（完了 {}人）",
                reminder_line(hw),
                hw.done_by.len()
            ));
        }

        if !subscribers.contains_key(&hw.guild_id) {
            let subs = db.hw_dm_subscribers(hw.guild_id.clone()).await?;
            subscribers.insert(hw.guild_id.clone(), subs);
        }
        for (user_id, secs) in &subscribers[&hw.guild_id] {
            if hw.is_done_by(user_id) || !should_notify(due, *secs, hw.created_at, now, false) {
                continue;
            }
            if db
                .hw_claim_notification(hw.id, user_id.clone(), *secs)
                .await?
            {
                dm_msgs.entry(user_id.clone()).or_default().push(format!(
                    "{}（<#{}>）",
                    reminder_line(hw),
                    hw.channel_id
                ));
            }
        }
    }

    // 送信に失敗しても再送はしない（DM 拒否などで毎分エラーになり続けるのを防ぐ）
    for (channel, lines) in channel_msgs {
        let Ok(id) = channel.parse::<u64>() else {
            continue;
        };
        let content = format!(
            "📚 **宿題の締切が近づいています**\n{}\n終わったら `/hw done` で記録しよう！",
            lines.join("\n")
        );
        if let Err(e) = ChannelId::new(id)
            .say(http, truncate_message(content))
            .await
        {
            eprintln!("宿題通知の送信エラー (channel {}): {:?}", channel, e);
        }
    }
    for (user, lines) in dm_msgs {
        let Ok(id) = user.parse::<u64>() else {
            continue;
        };
        let content = format!(
            "📚 **まだ完了になっていない宿題があります**\n{}\n終わったら `/hw done` で記録できます（DM 通知の変更は `/hw notify`）",
            lines.join("\n")
        );
        if let Err(e) = UserId::new(id)
            .direct_message(
                http,
                CreateMessage::new().content(truncate_message(content)),
            )
            .await
        {
            eprintln!("宿題 DM の送信エラー (user {}): {:?}", user, e);
        }
    }

    Ok(())
}

/// Discord のメッセージ上限（2000文字）に収める
fn truncate_message(s: String) -> String {
    if s.chars().count() <= 2000 {
        s
    } else {
        format!("{}…", s.chars().take(1990).collect::<String>())
    }
}

/// 毎日のまとめ投稿
pub async fn check_summaries(db: &Database, http: &Arc<Http>) -> Result<(), Error> {
    let now = local_now();
    let today = now.date();
    let today_str = today.format("%Y-%m-%d").to_string();

    for s in db.hw_summary_settings().await? {
        let (Some(time), Some(channel)) = (
            s.summary_time.as_deref().and_then(parse_time),
            s.channel_id.as_deref().and_then(|c| c.parse::<u64>().ok()),
        ) else {
            continue;
        };
        if s.last_summary_date.as_deref() == Some(today_str.as_str()) {
            continue;
        }
        let at = today.and_time(time);
        if now < at || now >= at + Duration::minutes(SUMMARY_GRACE_MINUTES) {
            continue;
        }

        // 先に記録して二重投稿を防ぐ
        db.hw_set_last_summary_date(s.guild_id.clone(), today_str.clone())
            .await?;

        let until = (today + Duration::days(SUMMARY_DAYS + 1))
            .and_hms_opt(0, 0, 0)
            .unwrap_or(now);
        let list = db
            .hw_list(
                s.guild_id.clone(),
                crate::models::HwQuery {
                    due_from: Some(now.format(DUE_FORMAT).to_string()),
                    due_until: Some(until.format(DUE_FORMAT).to_string()),
                    ..Default::default()
                },
            )
            .await?;

        let embed = summary_embed(&list, today);
        if let Err(e) = ChannelId::new(channel)
            .send_message(http, CreateMessage::new().embed(embed))
            .await
        {
            eprintln!("まとめ投稿の送信エラー (guild {}): {:?}", s.guild_id, e);
        }
    }
    Ok(())
}

fn summary_embed(list: &[Homework], today: chrono::NaiveDate) -> CreateEmbed {
    const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];
    use chrono::Datelike;

    let title = format!(
        "📚 宿題まとめ（{}/{} {}）",
        today.month(),
        today.day(),
        WEEKDAYS[today.weekday().num_days_from_monday() as usize]
    );
    let mut embed = CreateEmbed::new().title(title).colour(Colour(0x9B59B6));

    if list.is_empty() {
        return embed.description(format!("今後{}日間の宿題はありません 🎉", SUMMARY_DAYS));
    }

    let tomorrow = today.succ_opt().unwrap_or(today);
    let mut groups: [(&str, Vec<String>); 3] = [
        ("🔴 今日まで", vec![]),
        ("🟡 明日まで", vec![]),
        ("🟢 1週間以内", vec![]),
    ];
    for hw in list {
        let date = hw.due_date.get(..10).unwrap_or("");
        let idx = if date == today.format("%Y-%m-%d").to_string() {
            0
        } else if date == tomorrow.format("%Y-%m-%d").to_string() {
            1
        } else {
            2
        };
        groups[idx].1.push(format!(
            "{}（完了 {}人）",
            reminder_line(hw),
            hw.done_by.len()
        ));
    }

    for (name, lines) in groups {
        if lines.is_empty() {
            continue;
        }
        let mut value = String::new();
        for (i, line) in lines.iter().enumerate() {
            // フィールドの上限（1024文字）を超えないようにする
            if value.chars().count() + line.chars().count() > 950 {
                value += &format!("…ほか {} 件", lines.len() - i);
                break;
            }
            value += line;
            value += "\n";
        }
        embed = embed.field(name, value, false);
    }
    embed.footer(poise::serenity_prelude::CreateEmbedFooter::new(
        "自分の未完了は /hw todo で確認できます",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
            .unwrap()
            .and_utc()
    }

    #[test]
    fn notify_window() {
        let due = utc("2025-10-10 09:00");
        let created = utc("2025-10-01 00:00").timestamp();
        let day = 24 * 3600;

        assert!(!should_notify(
            due,
            day,
            created,
            utc("2025-10-09 08:59"),
            true
        ));
        assert!(should_notify(
            due,
            day,
            created,
            utc("2025-10-09 09:00"),
            true
        ));
        assert!(should_notify(
            due,
            day,
            created,
            utc("2025-10-10 08:59"),
            true
        ));
        assert!(!should_notify(
            due,
            day,
            created,
            utc("2025-10-10 09:00"),
            true
        ));
    }

    #[test]
    fn late_registration_skips_channel_but_not_dm() {
        let due = utc("2025-10-10 09:00");
        let created = utc("2025-10-09 20:00").timestamp();
        let now = utc("2025-10-09 20:01");
        let day = 24 * 3600;

        assert!(!should_notify(due, day, created, now, true));
        assert!(should_notify(due, day, created, now, false));
    }
}
