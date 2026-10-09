use poise::serenity_prelude::{self as serenity, AutocompleteChoice, Colour, CreateEmbed};

use super::{build_pages, reply_ephemeral, send_paginated, truncate};
use crate::{
    Context, Error,
    db::HwEdit,
    models::{Homework, HwQuery, HwSettings, HwSettingsPatch},
    time::{
        DUE_FORMAT, format_due, format_duration, local_now, parse_due_input, parse_time, timezone,
    },
};

/// `/hw todo` に表示する期限切れの範囲（日）
const TODO_OVERDUE_DAYS: i64 = 7;

// ────────────────────────────────────────────────────────────────────────────
// Choice parameter / helper
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, poise::ChoiceParameter)]
pub enum HwRemindChoice {
    #[name = "オフ"]
    Off,
    #[name = "1時間前"]
    OneHour,
    #[name = "3時間前"]
    ThreeHours,
    #[name = "6時間前"]
    SixHours,
    #[name = "1日前"]
    OneDay,
    #[name = "2日前"]
    TwoDays,
    #[name = "1週間前"]
    OneWeek,
}

impl HwRemindChoice {
    fn to_seconds(&self) -> Option<i64> {
        match self {
            Self::Off => None,
            Self::OneHour => Some(3600),
            Self::ThreeHours => Some(3 * 3600),
            Self::SixHours => Some(6 * 3600),
            Self::OneDay => Some(24 * 3600),
            Self::TwoDays => Some(2 * 24 * 3600),
            Self::OneWeek => Some(7 * 24 * 3600),
        }
    }
}

fn format_remind(secs: Option<i64>) -> String {
    secs.map(format_duration)
        .unwrap_or_else(|| "オフ".to_string())
}

/// 期限の入力を設定に従って解釈し、保存用フォーマットにする
fn parse_hw_due(input: &str, settings: &HwSettings) -> Option<String> {
    let default_time = parse_time(&settings.default_time)?;
    parse_due_input(input, local_now(), default_time).map(|d| d.format(DUE_FORMAT).to_string())
}

fn due_help(settings: &HwSettings) -> String {
    format!(
        "`明日` `明後日 17:00` `金曜` `12/5` `12/5 13:00` `2025-12-05` などで指定してください（時刻を省略すると {}）。",
        settings.default_time
    )
}

async fn reply_hw_not_found(ctx: Context<'_>, id: i64) -> Result<(), Error> {
    reply_ephemeral(ctx, format!("宿題 #{} が見つかりません。", id)).await
}

/// 一覧の1件分の表示（viewer が完了済みかも表示する）
fn hw_field(hw: &Homework, viewer: &str) -> (String, String) {
    let mine = if hw.is_done_by(viewer) { "✅" } else { "⬜" };
    (
        format!("{} [{}] {}", mine, hw.id, truncate(&hw.label(), 80)),
        format!(
            "期限: {} | 完了 {}人",
            format_due(Some(&hw.due_date)),
            hw.done_by.len()
        ),
    )
}

fn hw_embed(hw: &Homework, title: &str, colour: Colour) -> CreateEmbed {
    let done = match hw.done_by.len() {
        0 => "まだいません".to_string(),
        n if n <= 30 => format!(
            "{}人: {}",
            n,
            hw.done_by
                .iter()
                .map(|u| format!("<@{u}>"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        n => format!("{}人", n),
    };
    CreateEmbed::new()
        .title(title)
        .colour(colour)
        .field("ID", hw.id.to_string(), true)
        .field("科目", &hw.subject, true)
        .field("期限", format_due(Some(&hw.due_date)), true)
        .field("内容", &hw.title, false)
        .field("メモ", hw.description.as_deref().unwrap_or("なし"), false)
        .field("登録者", format!("<@{}>", hw.created_by), true)
        .field("完了", done, true)
}

// ────────────────────────────────────────────────────────────────────────────
// Autocomplete
// ────────────────────────────────────────────────────────────────────────────

async fn ac_subject(ctx: Context<'_>, partial: &str) -> Vec<String> {
    let Some(guild_id) = ctx.guild_id() else {
        return vec![];
    };
    let subjects = ctx
        .data()
        .db
        .hw_subjects(guild_id.to_string())
        .await
        .unwrap_or_default();
    subjects
        .into_iter()
        .filter(|s| s.contains(partial))
        .take(25)
        .collect()
}

async fn ac_homework(ctx: Context<'_>, partial: &str, query: HwQuery) -> Vec<AutocompleteChoice> {
    let Some(guild_id) = ctx.guild_id() else {
        return vec![];
    };
    let list = ctx
        .data()
        .db
        .hw_list(guild_id.to_string(), query)
        .await
        .unwrap_or_default();
    list.into_iter()
        .map(|hw| {
            let due = hw
                .due_date
                .get(5..)
                .unwrap_or(&hw.due_date)
                .replace('-', "/");
            let label = truncate(&format!("#{} {} (〆 {})", hw.id, hw.label(), due), 100);
            (label, hw.id)
        })
        .filter(|(label, _)| label.contains(partial))
        .take(25)
        .map(|(label, id)| AutocompleteChoice::new(label, id))
        .collect()
}

fn recent_from() -> String {
    (local_now() - chrono::Duration::days(TODO_OVERDUE_DAYS))
        .format(DUE_FORMAT)
        .to_string()
}

/// 自分が未完了の宿題（最近の期限切れを含む）
async fn ac_my_todo(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let query = HwQuery {
        due_from: Some(recent_from()),
        not_done_by: Some(ctx.author().id.to_string()),
        ..Default::default()
    };
    ac_homework(ctx, partial, query).await
}

/// 自分が完了済みの宿題（新しい順）
async fn ac_my_done(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let query = HwQuery {
        done_by: Some(ctx.author().id.to_string()),
        newest_first: true,
        ..Default::default()
    };
    ac_homework(ctx, partial, query).await
}

/// 最近〜今後の宿題すべて
async fn ac_any(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let query = HwQuery {
        due_from: Some(recent_from()),
        ..Default::default()
    };
    ac_homework(ctx, partial, query).await
}

// ────────────────────────────────────────────────────────────────────────────
// Command group
// ────────────────────────────────────────────────────────────────────────────

/// 宿題の管理（課題はみんなで共有、完了は自分ごと）
#[poise::command(
    slash_command,
    subcommands(
        "add", "todo", "list", "done", "undo", "view", "edit", "delete", "settings", "notify"
    )
)]
pub async fn hw(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw add
// ────────────────────────────────────────────────────────────────────────────

/// 宿題を登録する（サーバーのみんなに共有されます）
#[poise::command(slash_command)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "科目（例: 数学）"]
    #[autocomplete = "ac_subject"]
    #[max_length = 30]
    subject: String,
    #[description = "内容（例: 問題集 p.32〜35）"]
    #[max_length = 100]
    title: String,
    #[description = "期限（例: 明日 / 金曜 / 12/5 13:00）"] due: String,
    #[description = "メモ（提出方法・リンクなど）"]
    #[max_length = 1000]
    memo: Option<String>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;
    let settings = db.hw_settings(guild_id.clone()).await?;

    let Some(due_date) = parse_hw_due(&due, &settings) else {
        reply_ephemeral(
            ctx,
            format!(
                "期限 `{}` を解釈できませんでした。{}",
                due,
                due_help(&settings)
            ),
        )
        .await?;
        return Ok(());
    };

    let id = db
        .hw_add(
            guild_id.clone(),
            ctx.channel_id().to_string(),
            ctx.author().id.to_string(),
            subject.trim().to_string(),
            title,
            memo,
            due_date,
        )
        .await?;

    let Some(hw) = db.hw_get(id, guild_id).await? else {
        return reply_hw_not_found(ctx, id).await;
    };
    let embed = hw_embed(&hw, "📚 宿題を登録しました", Colour(0x2ECC71)).footer(
        serenity::CreateEmbedFooter::new("終わったら /hw done で自分の分を完了にできます"),
    );
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw todo
// ────────────────────────────────────────────────────────────────────────────

/// 自分がまだ終わっていない宿題を表示する（自分にだけ表示）
#[poise::command(slash_command)]
pub async fn todo(
    ctx: Context<'_>,
    #[description = "科目で絞り込み"]
    #[autocomplete = "ac_subject"]
    subject: Option<String>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let me = ctx.author().id.to_string();

    let query = HwQuery {
        subject,
        due_from: Some(recent_from()),
        not_done_by: Some(me.clone()),
        ..Default::default()
    };
    let list = ctx.data().db.hw_list(guild_id, query).await?;

    if list.is_empty() {
        reply_ephemeral(ctx, "🎉 未完了の宿題はありません！").await?;
        return Ok(());
    }

    let now = local_now().format(DUE_FORMAT).to_string();
    let pages = build_pages(
        "📝 あなたの未完了の宿題",
        Colour(0xE67E22),
        &list,
        |hw| {
            let (name, value) = hw_field(hw, &me);
            if hw.due_date < now {
                (format!("⚠️ 期限切れ {}", name), value)
            } else {
                (name, value)
            }
        },
    );
    send_paginated(ctx, pages, true).await
}

// ────────────────────────────────────────────────────────────────────────────
// /hw list
// ────────────────────────────────────────────────────────────────────────────

/// 宿題の一覧を表示する
#[poise::command(slash_command)]
pub async fn list(
    ctx: Context<'_>,
    #[description = "科目で絞り込み"]
    #[autocomplete = "ac_subject"]
    subject: Option<String>,
    #[description = "期限が過ぎたものを表示する (デフォルト: false)"] past: Option<bool>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let past = past.unwrap_or(false);
    let now = local_now().format(DUE_FORMAT).to_string();

    let query = HwQuery {
        subject: subject.clone(),
        due_from: (!past).then(|| now.clone()),
        due_until: past.then(|| now.clone()),
        newest_first: past,
        ..Default::default()
    };
    let list = ctx.data().db.hw_list(guild_id, query).await?;

    if list.is_empty() {
        let msg = if past {
            "期限が過ぎた宿題はありません。"
        } else {
            "これからの宿題はありません。`/hw add` で登録できます。"
        };
        reply_ephemeral(ctx, msg).await?;
        return Ok(());
    }

    let mut title = if past {
        "🗂️ 期限が過ぎた宿題".to_string()
    } else {
        "📚 宿題一覧".to_string()
    };
    if let Some(s) = &subject {
        title += &format!("（{}）", truncate(s, 30));
    }
    let me = ctx.author().id.to_string();
    let pages = build_pages(&title, Colour(0x9B59B6), &list, |hw| hw_field(hw, &me));
    send_paginated(ctx, pages, false).await
}

// ────────────────────────────────────────────────────────────────────────────
// /hw done, /hw undo
// ────────────────────────────────────────────────────────────────────────────

/// 自分の分の宿題を完了にする
#[poise::command(slash_command)]
pub async fn done(
    ctx: Context<'_>,
    #[description = "宿題（入力すると候補が出ます）"]
    #[autocomplete = "ac_my_todo"]
    id: i64,
) -> Result<(), Error> {
    set_done(ctx, id, true).await
}

/// 自分の分の完了を取り消す
#[poise::command(slash_command)]
pub async fn undo(
    ctx: Context<'_>,
    #[description = "宿題（入力すると候補が出ます）"]
    #[autocomplete = "ac_my_done"]
    id: i64,
) -> Result<(), Error> {
    set_done(ctx, id, false).await
}

async fn set_done(ctx: Context<'_>, id: i64, done: bool) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;
    let me = ctx.author().id.to_string();

    let Some(changed) = db.hw_set_done(id, guild_id.clone(), me, done).await? else {
        return reply_hw_not_found(ctx, id).await;
    };
    let Some(hw) = db.hw_get(id, guild_id).await? else {
        return reply_hw_not_found(ctx, id).await;
    };

    let msg = match (done, changed) {
        (true, true) => format!(
            "✅ #{} {} を完了にしました！（完了 {}人）",
            hw.id,
            hw.label(),
            hw.done_by.len()
        ),
        (true, false) => format!("#{} {} はもう完了になっています。", hw.id, hw.label()),
        (false, true) => format!("↩️ #{} {} を未完了に戻しました。", hw.id, hw.label()),
        (false, false) => format!("#{} {} はまだ完了になっていません。", hw.id, hw.label()),
    };
    reply_ephemeral(ctx, msg).await
}

// ────────────────────────────────────────────────────────────────────────────
// /hw view
// ────────────────────────────────────────────────────────────────────────────

/// 宿題の詳細と完了した人を表示する
#[poise::command(slash_command)]
pub async fn view(
    ctx: Context<'_>,
    #[description = "宿題（入力すると候補が出ます）"]
    #[autocomplete = "ac_any"]
    id: i64,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let Some(hw) = ctx.data().db.hw_get(id, guild_id).await? else {
        return reply_hw_not_found(ctx, id).await;
    };
    let colour = if hw.is_done_by(&ctx.author().id.to_string()) {
        Colour(0x2ECC71)
    } else {
        Colour(0xF1C40F)
    };
    let embed = hw_embed(&hw, &format!("📘 宿題 #{}", hw.id), colour);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw edit
// ────────────────────────────────────────────────────────────────────────────

/// 宿題を編集する（期限を変えると通知も再設定されます）
#[poise::command(slash_command)]
pub async fn edit(
    ctx: Context<'_>,
    #[description = "宿題（入力すると候補が出ます）"]
    #[autocomplete = "ac_any"]
    id: i64,
    #[description = "新しい科目"]
    #[autocomplete = "ac_subject"]
    #[max_length = 30]
    subject: Option<String>,
    #[description = "新しい内容"]
    #[max_length = 100]
    title: Option<String>,
    #[description = "新しい期限（例: 明日 / 金曜 / 12/5 13:00）"] due: Option<String>,
    #[description = "新しいメモ"]
    #[max_length = 1000]
    memo: Option<String>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;

    if subject.is_none() && title.is_none() && due.is_none() && memo.is_none() {
        reply_ephemeral(ctx, "変更する項目を少なくとも1つ指定してください。").await?;
        return Ok(());
    }

    let due_date = match due {
        None => None,
        Some(input) => {
            let settings = db.hw_settings(guild_id.clone()).await?;
            match parse_hw_due(&input, &settings) {
                Some(d) => Some(d),
                None => {
                    reply_ephemeral(
                        ctx,
                        format!(
                            "期限 `{}` を解釈できませんでした。{}",
                            input,
                            due_help(&settings)
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
    };

    let edit = HwEdit {
        subject: subject.map(|s| s.trim().to_string()),
        title,
        description: memo,
        due_date,
    };
    let Some(hw) = db.hw_edit(id, guild_id, edit).await? else {
        return reply_hw_not_found(ctx, id).await;
    };

    let embed = hw_embed(
        &hw,
        &format!("✏️ 宿題 #{} を更新しました", hw.id),
        Colour(0x3498DB),
    );
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw delete
// ────────────────────────────────────────────────────────────────────────────

/// 宿題を削除する（登録した人か、メッセージの管理権限がある人のみ）
#[poise::command(slash_command)]
pub async fn delete(
    ctx: Context<'_>,
    #[description = "宿題（入力すると候補が出ます）"]
    #[autocomplete = "ac_any"]
    id: i64,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;

    let Some(hw) = db.hw_get(id, guild_id.clone()).await? else {
        return reply_hw_not_found(ctx, id).await;
    };

    let is_owner = hw.created_by == ctx.author().id.to_string();
    let can_manage = ctx
        .author_member()
        .await
        .and_then(|m| m.permissions)
        .is_some_and(|p| p.manage_messages());
    if !is_owner && !can_manage {
        reply_ephemeral(
            ctx,
            format!(
                "宿題 #{} を削除できるのは登録した <@{}> か、メッセージの管理権限がある人だけです。",
                id, hw.created_by
            ),
        )
        .await?;
        return Ok(());
    }

    if db.hw_delete(id, guild_id).await?.is_none() {
        return reply_hw_not_found(ctx, id).await;
    }
    ctx.say(format!(
        "🗑️ 宿題 #{} {} を削除しました。",
        hw.id,
        hw.label()
    ))
    .await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw settings
// ────────────────────────────────────────────────────────────────────────────

fn settings_embed(s: &HwSettings) -> CreateEmbed {
    let channel = s
        .channel_id
        .as_ref()
        .map(|c| format!("<#{c}>"))
        .unwrap_or_else(|| "未設定（宿題を登録したチャンネル）".to_string());
    let summary = match (&s.summary_time, &s.channel_id) {
        (None, _) => "オフ".to_string(),
        (Some(t), Some(_)) => format!("毎日 {t}"),
        (Some(t), None) => format!("毎日 {t}（⚠️ 通知チャンネルが未設定のため投稿されません）"),
    };
    CreateEmbed::new()
        .title("⚙️ 宿題の設定")
        .colour(Colour(0x95A5A6))
        .field("通知チャンネル", channel, false)
        .field("期限前の通知", format_remind(s.remind_before), true)
        .field("まとめ投稿", summary, true)
        .field("時刻を省略したときの期限", &s.default_time, true)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "時刻は {} / 個人の DM 通知は /hw notify で設定",
            timezone()
        )))
}

/// サーバーの宿題の通知・まとめ投稿を設定する（何も指定しないと現在の設定を表示）
#[poise::command(slash_command)]
pub async fn settings(
    ctx: Context<'_>,
    #[description = "通知・まとめ投稿を送るチャンネル"]
    #[channel_types("Text")]
    channel: Option<serenity::GuildChannel>,
    #[description = "期限前にチャンネルへ通知するタイミング"] remind: Option<HwRemindChoice>,
    #[description = "毎日のまとめ投稿の時刻（例: 21:00 / オフ）"] summary: Option<String>,
    #[description = "期限の時刻を省略したときの時刻（例: 08:30）"] default_time: Option<String>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;

    let summary_time = match summary.as_deref().map(str::trim) {
        None => None,
        Some("オフ" | "off" | "OFF" | "なし") => Some(None),
        Some(input) => match parse_time(input) {
            Some(t) => Some(Some(t.format("%H:%M").to_string())),
            None => {
                reply_ephemeral(
                    ctx,
                    format!("まとめ投稿の時刻 `{input}` を解釈できませんでした。`21:00` や `オフ` で指定してください。"),
                )
                .await?;
                return Ok(());
            }
        },
    };
    let default_time = match default_time.as_deref() {
        None => None,
        Some(input) => {
            match parse_time(input) {
                Some(t) => Some(t.format("%H:%M").to_string()),
                None => {
                    reply_ephemeral(
                    ctx,
                    format!("時刻 `{input}` を解釈できませんでした。`08:30` のように指定してください。"),
                )
                .await?;
                    return Ok(());
                }
            }
        }
    };

    let mut patch = HwSettingsPatch {
        channel_id: channel.map(|c| c.id.to_string()),
        remind_before: remind.map(|r| r.to_seconds()),
        summary_time,
        default_time,
    };

    let changed = patch.channel_id.is_some()
        || patch.remind_before.is_some()
        || patch.summary_time.is_some()
        || patch.default_time.is_some();
    if !changed {
        let current = db.hw_settings(guild_id).await?;
        ctx.send(poise::CreateReply::default().embed(settings_embed(&current)))
            .await?;
        return Ok(());
    }

    // まとめ投稿をオンにしたのに通知チャンネルが未設定なら、このチャンネルを使う
    if matches!(patch.summary_time, Some(Some(_))) && patch.channel_id.is_none() {
        let current = db.hw_settings(guild_id.clone()).await?;
        if current.channel_id.is_none() {
            patch.channel_id = Some(ctx.channel_id().to_string());
        }
    }

    let updated = db.hw_update_settings(guild_id, patch).await?;
    ctx.send(
        poise::CreateReply::default()
            .content("✅ 設定を更新しました")
            .embed(settings_embed(&updated)),
    )
    .await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw notify
// ────────────────────────────────────────────────────────────────────────────

/// 自分が未完了の宿題を DM で知らせてもらう（自分だけの設定）
#[poise::command(slash_command)]
pub async fn notify(
    ctx: Context<'_>,
    #[description = "期限のどれくらい前に DM するか（オフで停止）"] timing: Option<HwRemindChoice>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;
    let me = ctx.author().id.to_string();

    let Some(timing) = timing else {
        let current = db.hw_get_dm(guild_id, me).await?;
        reply_ephemeral(
            ctx,
            format!(
                "🔔 あなたの DM 通知: **{}**\n`/hw notify timing:` で変更できます。",
                format_remind(current)
            ),
        )
        .await?;
        return Ok(());
    };

    let secs = timing.to_seconds();
    db.hw_set_dm(guild_id, me, secs).await?;
    let msg = match secs {
        Some(secs) => format!(
            "🔔 未完了の宿題を期限の **{}** に DM でお知らせします。\n（サーバーメンバーからの DM を許可しておいてください）",
            format_duration(secs)
        ),
        None => "🔕 DM 通知をオフにしました。".to_string(),
    };
    reply_ephemeral(ctx, msg).await
}
