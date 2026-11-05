use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Timelike, Utc};
use poise::serenity_prelude::{
    ChannelId, Colour, CreateAllowedMentions, CreateEmbed, CreateEmbedFooter, CreateMessage, Http,
    MessageFlags, RoleId, UserId,
};

use crate::{
    Error,
    commands::{WebTab, done_buttons, web_button},
    db::Database,
    models::{Homework, HwKind, HwQuery, HwSettings, Task, WEEKDAYS_JA, mentions},
    time::{DUE_FORMAT, due_to_utc, local_now, local_to_utc, next_weekly, parse_time, timezone},
};

/// 事前通知の最大（1週間）＋余裕
const LOOKAHEAD_DAYS: i64 = 8;
/// まとめ投稿の時刻を過ぎてからこの時間内なら投稿する（ボット再起動直後の取りこぼし対策）
const SUMMARY_GRACE_MINUTES: i64 = 30;
/// まとめ投稿に載せる宿題の期間（日）
const SUMMARY_DAYS: i64 = 7;
/// まとめ投稿に載せるテストの期間（日）
const SUMMARY_EXAM_DAYS: i64 = 14;

/// 夜中（QUIET_START 時〜翌 QUIET_END 時）は期限前の通知を送らない
const QUIET_START: u32 = 22;
const QUIET_END: u32 = 7;
/// 夜中を避けて前の晩に送るときの時刻（QUIET_START の少し前）
const QUIET_EARLY: (u32, u32) = (21, 30);

/// 期限前の通知を送る時刻。期限の remind_before 秒前が夜中（22:00〜7:00）に当たるときは、
/// 朝 7:00 にずらす。それだと期限に間に合わないときは、その前の晩の 21:30 に早める
/// （例: 23:59 締め切りの「1日前」は前日の 23:59 ではなく当日の 7:00）
pub fn notify_time(due: DateTime<Utc>, remind_before: i64) -> DateTime<Utc> {
    let at = due - Duration::seconds(remind_before);
    let tz = timezone();
    let local = at.with_timezone(&tz).naive_local();
    let hour = local.hour();
    if (QUIET_END..QUIET_START).contains(&hour) {
        return at;
    }
    // 夜中が始まった日（0〜7時なら前の日）
    let night = if hour >= QUIET_START {
        local.date()
    } else {
        local.date() - Duration::days(1)
    };
    let morning = (night + Duration::days(1))
        .and_hms_opt(QUIET_END, 0, 0)
        .and_then(|t| local_to_utc(t, tz));
    if let Some(m) = morning
        && m < due
    {
        return m;
    }
    night
        .and_hms_opt(QUIET_EARLY.0, QUIET_EARLY.1, 0)
        .and_then(|t| local_to_utc(t, tz))
        .unwrap_or(at)
}

/// 今この通知を送るべきか
///
/// - 通知時刻（夜中ならずらした時刻）を過ぎていて、期限はまだ来ていない
/// - チャンネル通知は、通知時刻より後に登録された宿題には送らない（登録時のメッセージで分かるため）
fn should_notify(
    due: DateTime<Utc>,
    remind_before: i64,
    created_at: i64,
    now: DateTime<Utc>,
    skip_if_registered_late: bool,
) -> bool {
    let notify_at = notify_time(due, remind_before);
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

/// 通知ロールだけを鳴らす設定（本文中の他のメンションでは通知しない）
pub fn role_allowed_mentions(settings: Option<&HwSettings>) -> CreateAllowedMentions {
    let roles: Vec<RoleId> = settings
        .and_then(|s| s.notify_role_id.as_deref())
        .and_then(|r| r.parse::<u64>().ok())
        .filter(|r| *r > 0)
        .map(RoleId::new)
        .into_iter()
        .collect();
    CreateAllowedMentions::new().roles(roles)
}

/// 本文の先頭に通知ロールのメンションを付ける
pub fn with_role_mention(settings: Option<&HwSettings>, body: String) -> String {
    match settings.and_then(HwSettings::role_mention) {
        Some(mention) => format!("{mention} {body}"),
        None => body,
    }
}

/// 本文に入った URL（宿題の内容に書かれたものなど）のカードを出さないようにする。
/// 埋め込み（Embed）も消えるので、Embed を使うメッセージには使わない
pub fn suppress_link_previews(msg: CreateMessage, hide: bool) -> CreateMessage {
    if hide {
        msg.flags(MessageFlags::SUPPRESS_EMBEDS)
    } else {
        msg
    }
}

/// 期限前の通知。settings はサーバーの設定（DM では None）。通知ロールは
/// 「登録・期限前の通知でもメンション」の設定のときだけ鳴らす。
/// URL のカードはサーバーの設定に従い、DM では出さない
fn reminder_message(
    list: &[Homework],
    header: &str,
    footer: &str,
    settings: Option<&HwSettings>,
    web: Option<&str>,
) -> CreateMessage {
    let link = list
        .first()
        .and_then(|hw| web_button(web, Some(&hw.guild_id), WebTab::Todo));
    let lines: Vec<String> = list.iter().map(reminder_line).collect();
    let body = format!("{header}\n{}\n{footer}", lines.join("\n"));
    let mention = settings.and_then(HwSettings::for_event_mention);
    let msg = CreateMessage::new()
        .content(truncate_message(with_role_mention(mention, body)))
        .allowed_mentions(role_allowed_mentions(mention))
        .components(done_buttons(list, true, link));
    suppress_link_previews(msg, settings.is_none_or(|s| s.hide_link_previews))
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
pub async fn check_reminders(
    db: &Database,
    http: &Arc<Http>,
    web: Option<&str>,
) -> Result<(), Error> {
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
    let mut channel_msgs: BTreeMap<(String, String), Vec<Homework>> = BTreeMap::new();
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
            channel_msgs
                .entry((channel, hw.guild_id.clone()))
                .or_default()
                .push(hw.clone());
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
    for ((channel, guild_id), list) in channel_msgs {
        // Web から登録した宿題は投稿先が決まっていないことがある（"0"）
        let Some(id) = channel.parse::<u64>().ok().filter(|id| *id > 0) else {
            continue;
        };
        let msg = reminder_message(
            &list,
            "📚 **宿題・テストが近づいています**",
            "宿題は終わったらボタンか `/hw done` で記録しよう！",
            settings.get(&guild_id),
            web,
        );
        if let Err(e) = ChannelId::new(id).send_message(http, msg).await {
            eprintln!("宿題通知の送信エラー (channel {}): {:?}", channel, e);
        }
    }
    for (user, list) in dm_msgs {
        let Some(id) = user.parse::<u64>().ok().filter(|id| *id > 0) else {
            continue;
        };
        let msg = reminder_message(
            &list,
            "📚 **まだ完了になっていない宿題・近づいているテストがあります**",
            "DM 通知の変更は `/hw notify` でできます。",
            None,
            web,
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
pub async fn check_summaries(
    db: &Database,
    http: &Arc<Http>,
    web: Option<&str>,
) -> Result<(), Error> {
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
        let (Some(time), Some(channel)) = (
            s.summary_time.as_deref().and_then(parse_time),
            channel.parse::<u64>().ok().filter(|id| *id > 0),
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
        let mut msg = CreateMessage::new()
            .embed(summary_embed(&homework, &exams, &tasks, now))
            .components(done_buttons(
                &homework,
                true,
                web_button(web, Some(&s.guild_id), WebTab::Todo),
            ))
            .allowed_mentions(role_allowed_mentions(Some(&s)));
        // 載せるものがある日だけ通知ロールを鳴らす
        if !(homework.is_empty() && exams.is_empty() && tasks.is_empty())
            && let Some(mention) = s.role_mention()
        {
            msg = msg.content(format!("{mention} 今日のまとめです"));
        }
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

    /// 日本時間（テストでは TIMEZONE 未設定なので Asia/Tokyo）
    fn jst(s: &str) -> DateTime<Utc> {
        local_to_utc(
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap(),
            timezone(),
        )
        .unwrap()
    }

    #[test]
    fn reminders_avoid_the_night() {
        let hour = 3600;
        let day = 24 * hour;
        // 23:59 締め切りの「1日前」は、前日の 23:59 ではなく当日の朝 7:00
        assert_eq!(
            notify_time(jst("2025-10-10 23:59"), day),
            jst("2025-10-10 07:00")
        );
        // 昼間ならそのまま
        assert_eq!(
            notify_time(jst("2025-10-10 08:30"), day),
            jst("2025-10-09 08:30")
        );
        assert_eq!(
            notify_time(jst("2025-10-10 23:00"), 3 * hour),
            jst("2025-10-10 20:00")
        );
        // 朝 7:00 だと締め切りに間に合わないときは、前の晩の 21:30
        assert_eq!(
            notify_time(jst("2025-10-10 23:59"), hour),
            jst("2025-10-10 21:30")
        );
        assert_eq!(
            notify_time(jst("2025-10-10 06:00"), hour),
            jst("2025-10-09 21:30")
        );
        // 夜中の 2:00 締め切りの「1日前」（前の日の 2:00）→ その朝 7:00
        assert_eq!(
            notify_time(jst("2025-10-10 02:00"), day),
            jst("2025-10-09 07:00")
        );

        // should_notify もずらした時刻で判定する
        let due = jst("2025-10-10 23:59");
        let created = jst("2025-10-01 00:00").timestamp();
        assert!(!should_notify(
            due,
            day,
            created,
            jst("2025-10-09 23:59"),
            true
        ));
        assert!(!should_notify(
            due,
            day,
            created,
            jst("2025-10-10 06:59"),
            true
        ));
        assert!(should_notify(
            due,
            day,
            created,
            jst("2025-10-10 07:00"),
            true
        ));
        // 夜中に登録したものは、朝の通知時刻より前の登録なので通知される
        let created_at_night = jst("2025-10-10 00:30").timestamp();
        assert!(should_notify(
            due,
            day,
            created_at_night,
            jst("2025-10-10 07:00"),
            true
        ));
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
    fn reminder_pings_only_the_notify_role() {
        let mut settings = HwSettings::default_for("G".into());
        settings.notify_role_id = Some("777".into());
        // タイトルに別のロールや @everyone が書かれていても鳴らさない
        let mut item = hw(1, HwKind::Homework, "2999-01-01 08:30");
        item.title = "<@&999> @everyone 課題".into();

        let json = serde_json::to_value(reminder_message(
            &[item.clone()],
            "見出し",
            "フッター",
            Some(&settings),
            Some("https://hw.example.com"),
        ))
        .unwrap();
        // 完了ボタンの後ろに「Web で開く」
        let buttons = &json["components"][0]["components"];
        assert_eq!(buttons[0]["custom_id"], "hwdone:G:1");
        assert_eq!(
            buttons[1]["url"],
            "https://hw.example.com/?guild=G&tab=todo"
        );
        // 初期値（まとめ投稿だけ）では期限前の通知でロールを鳴らさない
        assert!(json["content"].as_str().unwrap().starts_with("見出し"));
        assert_eq!(json["allowed_mentions"]["roles"], serde_json::json!([]));
        assert_eq!(json["allowed_mentions"]["parse"], serde_json::json!([]));

        // 「登録・期限前の通知でも」の設定なら通知ロールだけ鳴らす
        let mut all = settings.clone();
        all.mention_all = true;
        let json = serde_json::to_value(reminder_message(
            &[item.clone()],
            "見出し",
            "フッター",
            Some(&all),
            None,
        ))
        .unwrap();
        assert!(
            json["content"]
                .as_str()
                .unwrap()
                .starts_with("<@&777> 見出し")
        );
        assert_eq!(
            json["allowed_mentions"]["roles"],
            serde_json::json!(["777"])
        );
        assert_eq!(json["allowed_mentions"]["parse"], serde_json::json!([]));
        // 内容に URL があってもカードを出さない（初期値）
        assert_eq!(json["flags"], 4);

        // 設定で「出す」にしたらフラグなし
        let mut show = settings.clone();
        show.hide_link_previews = false;
        let json = serde_json::to_value(reminder_message(
            &[item.clone()],
            "見出し",
            "フッター",
            Some(&show),
            None,
        ))
        .unwrap();
        assert!(json.get("flags").is_none_or(|f| f.is_null()));

        // ロール未設定・DM では誰も鳴らさない
        let json =
            serde_json::to_value(reminder_message(&[item], "見出し", "フッター", None, None))
                .unwrap();
        // DM でもカードは出さない
        assert_eq!(json["flags"], 4);
        // Web ダッシュボードが無効ならリンクなし
        assert_eq!(
            json["components"][0]["components"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(json["content"].as_str().unwrap().starts_with("見出し"));
        assert_eq!(json["allowed_mentions"]["roles"], serde_json::json!([]));
        assert_eq!(json["allowed_mentions"]["parse"], serde_json::json!([]));
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
