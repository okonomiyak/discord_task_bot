use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Utc};
use poise::serenity_prelude::{
    ChannelId, Colour, CreateEmbed, CreateEmbedFooter, CreateMessage, Http, UserId,
};

use crate::{
    Error,
    commands::done_buttons,
    db::Database,
    models::{Homework, HwKind, HwQuery, HwSettings, Task, WEEKDAYS_JA, mentions},
    time::{DUE_FORMAT, due_to_utc, local_now, next_weekly, parse_time},
};

/// 事前通知の最大（1週間）＋余裕
const LOOKAHEAD_DAYS: i64 = 8;
/// まとめ投稿の時刻を過ぎてからこの時間内なら投稿する（ボット再起動直後の取りこぼし対策）
const SUMMARY_GRACE_MINUTES: i64 = 30;
/// まとめ投稿に載せる宿題の期間（日）
const SUMMARY_DAYS: i64 = 7;
/// まとめ投稿に載せるテストの期間（日）
const SUMMARY_EXAM_DAYS: i64 = 14;

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
    let done = match hw.kind {
        HwKind::Homework => format!("（完了 {}人）", hw.done_by.len()),
        HwKind::Exam => String::new(),
    };
    format!(
        "`#{}` {}{}｜{} — <t:{unix}:f>（<t:{unix}:R>）{}",
        hw.id,
        if hw.kind == HwKind::Exam { "📝 " } else { "" },
        hw.subject,
        hw.title,
        done
    )
}

/// Discord のメッセージ上限（2000文字）に収める
fn truncate_message(s: String) -> String {
    if s.chars().count() <= 2000 {
        s
    } else {
        format!("{}…", s.chars().take(1990).collect::<String>())
    }
}

fn reminder_message(list: &[Homework], header: &str, footer: &str) -> CreateMessage {
    let lines: Vec<String> = list.iter().map(reminder_line).collect();
    CreateMessage::new()
        .content(truncate_message(format!(
            "{header}\n{}\n{footer}",
            lines.join("\n")
        )))
        .components(done_buttons(list, true))
}

// ────────────────────────────────────────────────────────────────────────────
// 毎週の宿題
// ────────────────────────────────────────────────────────────────────────────

/// 毎週の宿題の「次の回」がまだ登録されていなければ登録する
pub async fn generate_repeats(db: &Database) -> Result<(), Error> {
    let now = local_now();
    for r in db.hw_repeats(None).await? {
        let Some(time) = parse_time(&r.time) else {
            continue;
        };
        let next = next_weekly(now, r.weekday, time)
            .format(DUE_FORMAT)
            .to_string();
        if r.last_due.as_ref().is_some_and(|last| *last >= next) {
            continue;
        }
        db.hw_repeat_generate(r.id, next).await?;
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// 期限前の通知
// ────────────────────────────────────────────────────────────────────────────

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
    let mut channel_msgs: BTreeMap<String, Vec<Homework>> = BTreeMap::new();
    let mut dm_msgs: BTreeMap<String, Vec<Homework>> = BTreeMap::new();

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
            channel_msgs.entry(channel).or_default().push(hw.clone());
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
                dm_msgs.entry(user_id.clone()).or_default().push(hw.clone());
            }
        }
    }

    // 送信に失敗しても再送はしない（DM 拒否などで毎分エラーになり続けるのを防ぐ）
    for (channel, list) in channel_msgs {
        let Ok(id) = channel.parse::<u64>() else {
            continue;
        };
        let msg = reminder_message(
            &list,
            "📚 **宿題・テストが近づいています**",
            "宿題は終わったらボタンか `/hw done` で記録しよう！",
        );
        if let Err(e) = ChannelId::new(id).send_message(http, msg).await {
            eprintln!("宿題通知の送信エラー (channel {}): {:?}", channel, e);
        }
    }
    for (user, list) in dm_msgs {
        let Ok(id) = user.parse::<u64>() else {
            continue;
        };
        let msg = reminder_message(
            &list,
            "📚 **まだ完了になっていない宿題・近づいているテストがあります**",
            "DM 通知の変更は `/hw notify` でできます。",
        );
        if let Err(e) = UserId::new(id).direct_message(http, msg).await {
            eprintln!("宿題 DM の送信エラー (user {}): {:?}", user, e);
        }
    }

    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// 毎日のまとめ投稿
// ────────────────────────────────────────────────────────────────────────────

/// 毎日のまとめ投稿（宿題・テスト・タスク）
pub async fn check_summaries(db: &Database, http: &Arc<Http>) -> Result<(), Error> {
    let now = local_now();
    let today = now.date();
    let today_str = today.format("%Y-%m-%d").to_string();
    let days_later = |days: i64| {
        (today + Duration::days(days + 1))
            .and_hms_opt(0, 0, 0)
            .unwrap_or(now)
            .format(DUE_FORMAT)
            .to_string()
    };
    let range = |days: i64| HwQuery {
        due_from: Some(now.format(DUE_FORMAT).to_string()),
        due_until: Some(days_later(days)),
        ..Default::default()
    };

    for (s, channel) in db.hw_summary_targets().await? {
        let (Some(time), Ok(channel)) = (
            s.summary_time.as_deref().and_then(parse_time),
            channel.parse::<u64>(),
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

        let homework = db
            .hw_list(
                s.guild_id.clone(),
                HwQuery {
                    kind: Some(HwKind::Homework),
                    ..range(SUMMARY_DAYS)
                },
            )
            .await?;
        let exams = db
            .hw_list(
                s.guild_id.clone(),
                HwQuery {
                    kind: Some(HwKind::Exam),
                    ..range(SUMMARY_EXAM_DAYS)
                },
            )
            .await?;
        let tasks = db
            .open_tasks_due_until(s.guild_id.clone(), days_later(SUMMARY_DAYS))
            .await?;

        // ボタンは期限が近い宿題から（最大25個）
        let msg = CreateMessage::new()
            .embed(summary_embed(&homework, &exams, &tasks, now))
            .components(done_buttons(&homework, true));
        if let Err(e) = ChannelId::new(channel).send_message(http, msg).await {
            eprintln!("まとめ投稿の送信エラー (guild {}): {:?}", s.guild_id, e);
        }
    }
    Ok(())
}

/// フィールドの上限（1024文字）を超えないように行をまとめる
fn join_lines(lines: &[String]) -> String {
    let mut value = String::new();
    for (i, line) in lines.iter().enumerate() {
        if value.chars().count() + line.chars().count() > 950 {
            value += &format!("…ほか {} 件", lines.len() - i);
            break;
        }
        value += line;
        value += "\n";
    }
    value
}

fn task_line(task: &Task, now: &str) -> String {
    let due = task.due_date.as_deref().unwrap_or("");
    let unix = due_to_utc(due).map(|d| d.timestamp()).unwrap_or(0);
    let overdue = if due < now {
        "⚠️ 期限切れ "
    } else {
        ""
    };
    let who = if task.assignees.is_empty() {
        String::new()
    } else {
        format!("（担当: {}）", mentions(&task.assignees))
    };
    format!(
        "{overdue}`#{}` {} {}｜{} — <t:{unix}:f>（<t:{unix}:R>）{who}",
        task.id,
        task.status.emoji(),
        task.priority.emoji(),
        task.title,
    )
}

fn summary_embed(
    homework: &[Homework],
    exams: &[Homework],
    tasks: &[Task],
    now: NaiveDateTime,
) -> CreateEmbed {
    let today = now.date();
    let title = format!(
        "📅 今日のまとめ（{}/{} {}）",
        today.month(),
        today.day(),
        WEEKDAYS_JA[today.weekday().num_days_from_monday() as usize]
    );
    let mut embed = CreateEmbed::new().title(title).colour(Colour(0x9B59B6));

    if homework.is_empty() && exams.is_empty() && tasks.is_empty() {
        return embed.description(format!(
            "今後{}日間の宿題・テスト・タスクはありません 🎉",
            SUMMARY_DAYS
        ));
    }

    let today_str = today.format("%Y-%m-%d").to_string();
    let tomorrow_str = today
        .succ_opt()
        .unwrap_or(today)
        .format("%Y-%m-%d")
        .to_string();
    let mut groups: [(&str, Vec<String>); 3] = [
        ("🔴 宿題：今日まで", vec![]),
        ("🟡 宿題：明日まで", vec![]),
        ("🟢 宿題：1週間以内", vec![]),
    ];
    for hw in homework {
        let date = hw.due_date.get(..10).unwrap_or("");
        let idx = if date == today_str {
            0
        } else if date == tomorrow_str {
            1
        } else {
            2
        };
        groups[idx].1.push(reminder_line(hw));
    }

    for (name, lines) in groups {
        if !lines.is_empty() {
            embed = embed.field(name, join_lines(&lines), false);
        }
    }
    if !exams.is_empty() {
        let lines: Vec<String> = exams.iter().map(reminder_line).collect();
        embed = embed.field(
            format!("📝 テスト（{}日以内）", SUMMARY_EXAM_DAYS),
            join_lines(&lines),
            false,
        );
    }
    if !tasks.is_empty() {
        let now_str = now.format(DUE_FORMAT).to_string();
        let lines: Vec<String> = tasks.iter().map(|t| task_line(t, &now_str)).collect();
        embed = embed.field(
            format!("📋 タスク（期限切れ・{}日以内）", SUMMARY_DAYS),
            join_lines(&lines),
            false,
        );
    }
    embed.footer(CreateEmbedFooter::new(
        "ボタンで自分の分を完了にできます / 自分の未完了は /hw todo / 時刻の変更は /hw settings",
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

    fn hw(id: i64, kind: HwKind, due: &str) -> Homework {
        Homework {
            id,
            kind,
            guild_id: "G".into(),
            channel_id: "1".into(),
            created_by: "1".into(),
            subject: "数学".into(),
            title: format!("課題{id}"),
            description: None,
            due_date: due.into(),
            created_at: 0,
            done_by: vec![],
            repeat_id: None,
        }
    }

    fn task(id: i64, due: &str) -> Task {
        Task {
            id,
            user_id: "1".into(),
            guild_id: "G".into(),
            title: format!("タスク{id}"),
            description: None,
            status: crate::models::TaskStatus::Pending,
            priority: crate::models::Priority::High,
            due_date: Some(due.into()),
            created_at: String::new(),
            channel_id: None,
            reminders: vec![],
            discord_event_id: None,
            assignees: vec!["7".into()],
        }
    }

    fn embed_json(embed: CreateEmbed) -> serde_json::Value {
        serde_json::to_value(embed).unwrap()
    }

    #[test]
    fn summary_groups_homework_exams_and_tasks() {
        // 2025-10-09 は木曜日
        let now =
            chrono::NaiveDateTime::parse_from_str("2025-10-09 21:00", "%Y-%m-%d %H:%M").unwrap();
        let homework = [
            hw(1, HwKind::Homework, "2025-10-09 23:59"),
            hw(2, HwKind::Homework, "2025-10-10 08:30"),
            hw(3, HwKind::Homework, "2025-10-14 08:30"),
        ];
        let exams = [hw(4, HwKind::Exam, "2025-10-20 10:40")];
        let tasks = [task(5, "2025-10-01 00:00"), task(6, "2025-10-11 12:00")];

        let json = embed_json(summary_embed(&homework, &exams, &tasks, now));
        assert_eq!(json["title"], "📅 今日のまとめ（10/9 木）");
        let fields = json["fields"].as_array().unwrap();
        let names: Vec<_> = fields.iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "🔴 宿題：今日まで",
                "🟡 宿題：明日まで",
                "🟢 宿題：1週間以内",
                "📝 テスト（14日以内）",
                "📋 タスク（期限切れ・7日以内）",
            ]
        );
        let task_field = fields[4]["value"].as_str().unwrap();
        assert!(task_field.contains("⚠️ 期限切れ `#5`"));
        assert!(task_field.contains("`#6`") && !task_field.contains("⚠️ 期限切れ `#6`"));
        assert!(task_field.contains("担当: <@7>"));
    }

    #[test]
    fn summary_says_so_when_empty() {
        let now =
            chrono::NaiveDateTime::parse_from_str("2025-10-09 21:00", "%Y-%m-%d %H:%M").unwrap();
        let json = embed_json(summary_embed(&[], &[], &[], now));
        assert!(json["description"].as_str().unwrap().contains("ありません"));
        assert!(
            json.get("fields")
                .is_none_or(|f| f.as_array().is_some_and(|a| a.is_empty()))
        );
    }

    #[test]
    fn join_lines_stays_within_field_limit() {
        let lines: Vec<String> = (0..100)
            .map(|i| format!("{i:03} {}", "あ".repeat(40)))
            .collect();
        let value = join_lines(&lines);
        assert!(value.chars().count() <= 1024);
        assert!(value.contains("…ほか"));
    }
}
