use poise::serenity_prelude::{
    self as serenity, AutocompleteChoice, ButtonStyle, Colour, ComponentInteraction,
    CreateActionRow, CreateButton, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseMessage,
};

use super::{build_pages, due_help, parse_due_with, reply_ephemeral, send_paginated, truncate};
use crate::{
    Context, Data, Error,
    db::HwEdit,
    models::{
        Homework, HwKind, HwQuery, HwRepeat, HwSettings, HwSettingsPatch, NewHomework, WEEKDAYS_JA,
    },
    time::{DUE_FORMAT, format_due, format_duration, local_now, next_weekly, parse_time, timezone},
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

#[derive(Debug, poise::ChoiceParameter)]
pub enum WeekdayChoice {
    #[name = "月曜日"]
    Mon,
    #[name = "火曜日"]
    Tue,
    #[name = "水曜日"]
    Wed,
    #[name = "木曜日"]
    Thu,
    #[name = "金曜日"]
    Fri,
    #[name = "土曜日"]
    Sat,
    #[name = "日曜日"]
    Sun,
}

impl WeekdayChoice {
    fn index(&self) -> u32 {
        match self {
            Self::Mon => 0,
            Self::Tue => 1,
            Self::Wed => 2,
            Self::Thu => 3,
            Self::Fri => 4,
            Self::Sat => 5,
            Self::Sun => 6,
        }
    }
}

fn format_remind(secs: Option<i64>) -> String {
    secs.map(format_duration)
        .unwrap_or_else(|| "オフ".to_string())
}

async fn reply_hw_not_found(ctx: Context<'_>, id: i64) -> Result<(), Error> {
    reply_ephemeral(ctx, format!("宿題・テスト #{} が見つかりません。", id)).await
}

/// 登録者か「メッセージの管理」権限がある人か
async fn can_modify(ctx: Context<'_>, created_by: &str) -> bool {
    created_by == ctx.author().id.to_string()
        || ctx
            .author_member()
            .await
            .and_then(|m| m.permissions)
            .is_some_and(|p| p.manage_messages())
}

/// 一覧の1件分の表示（viewer が完了済みかも表示する）
fn hw_field(hw: &Homework, viewer: &str) -> (String, String) {
    let repeat = if hw.repeat_id.is_some() { "🔁 " } else { "" };
    match hw.kind {
        HwKind::Homework => {
            let mine = if hw.is_done_by(viewer) { "✅" } else { "⬜" };
            (
                format!(
                    "{} [{}] {}{}",
                    mine,
                    hw.id,
                    repeat,
                    truncate(&hw.label(), 80)
                ),
                format!(
                    "期限: {} | 完了 {}人",
                    format_due(Some(&hw.due_date)),
                    hw.done_by.len()
                ),
            )
        }
        HwKind::Exam => (
            format!("[{}] {}", hw.id, truncate(&hw.label(), 80)),
            format!("日時: {}", format_due(Some(&hw.due_date))),
        ),
    }
}

fn hw_embed(hw: &Homework, title: &str, colour: Colour) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title(title)
        .colour(colour)
        .field("ID", hw.id.to_string(), true)
        .field("科目", &hw.subject, true)
        .field(
            if hw.kind == HwKind::Exam {
                "日時"
            } else {
                "期限"
            },
            format_due(Some(&hw.due_date)),
            true,
        )
        .field("内容", &hw.title, false)
        .field("メモ", hw.description.as_deref().unwrap_or("なし"), false)
        .field("登録者", format!("<@{}>", hw.created_by), true);

    if hw.kind == HwKind::Homework {
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
        embed = embed.field("完了", done, true);
    }
    if let Some(rid) = hw.repeat_id {
        embed = embed.field(
            "毎週",
            format!("🔁 毎週の宿題 #{} から自動登録", rid),
            false,
        );
    }
    embed
}

// ────────────────────────────────────────────────────────────────────────────
// 完了ボタン
// ────────────────────────────────────────────────────────────────────────────

const DONE_PREFIX: &str = "hwdone";
const UNDO_PREFIX: &str = "hwundo";

fn button_id(prefix: &str, hw: &Homework) -> String {
    format!("{prefix}:{}:{}", hw.guild_id, hw.id)
}

/// 宿題の「完了」ボタン（テストには付けない、最大25個）
pub fn done_buttons(list: &[Homework], with_label: bool) -> Vec<CreateActionRow> {
    let buttons: Vec<CreateButton> = list
        .iter()
        .filter(|hw| hw.kind == HwKind::Homework)
        .take(25)
        .map(|hw| {
            let label = if with_label {
                truncate(&format!("完了 #{} {}", hw.id, hw.subject), 80)
            } else {
                "完了にする".to_string()
            };
            CreateButton::new(button_id(DONE_PREFIX, hw))
                .style(ButtonStyle::Success)
                .emoji('✅')
                .label(label)
        })
        .collect();
    buttons
        .chunks(5)
        .map(|row| CreateActionRow::Buttons(row.to_vec()))
        .collect()
}

/// 完了・取り消しボタンが押されたときの処理（メッセージが古くても、DM でも動く）
pub async fn handle_component(
    ctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<(), Error> {
    let mut parts = interaction.data.custom_id.split(':');
    let (Some(prefix), Some(guild_id), Some(id)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(());
    };
    let done = match prefix {
        DONE_PREFIX => true,
        UNDO_PREFIX => false,
        _ => return Ok(()),
    };
    let Ok(id) = id.parse::<i64>() else {
        return Ok(());
    };
    // サーバー内で押された場合は、そのサーバーの宿題のボタンだけ受け付ける
    if interaction
        .guild_id
        .is_some_and(|g| g.to_string() != guild_id)
    {
        return Ok(());
    }

    let user = interaction.user.id.to_string();
    let changed = data
        .db
        .hw_set_done(id, guild_id.to_string(), user, done)
        .await?;
    let hw = data.db.hw_get(id, guild_id.to_string()).await?;

    let mut reply = CreateInteractionResponseMessage::new().ephemeral(true);
    reply = match (hw, changed) {
        (Some(hw), Some(changed)) => {
            let content = match (done, changed) {
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
            let mut reply = reply.content(content);
            if done {
                reply = reply.components(vec![CreateActionRow::Buttons(vec![
                    CreateButton::new(button_id(UNDO_PREFIX, &hw))
                        .style(ButtonStyle::Secondary)
                        .label("取り消す"),
                ])]);
            }
            reply
        }
        _ => reply.content(format!(
            "宿題 #{} が見つかりません（削除された可能性があります）。",
            id
        )),
    };

    interaction
        .create_response(ctx, CreateInteractionResponse::Message(reply))
        .await?;
    Ok(())
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
        kind: Some(HwKind::Homework),
        due_from: Some(recent_from()),
        not_done_by: Some(ctx.author().id.to_string()),
        ..Default::default()
    };
    ac_homework(ctx, partial, query).await
}

/// 自分が完了済みの宿題（新しい順）
async fn ac_my_done(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let query = HwQuery {
        kind: Some(HwKind::Homework),
        done_by: Some(ctx.author().id.to_string()),
        newest_first: true,
        ..Default::default()
    };
    ac_homework(ctx, partial, query).await
}

/// 最近〜今後の宿題・テストすべて
async fn ac_any(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let query = HwQuery {
        due_from: Some(recent_from()),
        ..Default::default()
    };
    ac_homework(ctx, partial, query).await
}

async fn ac_repeat(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let Some(guild_id) = ctx.guild_id() else {
        return vec![];
    };
    let list = ctx
        .data()
        .db
        .hw_repeats(Some(guild_id.to_string()))
        .await
        .unwrap_or_default();
    list.into_iter()
        .map(|r| (truncate(&repeat_label(&r), 100), r.id))
        .filter(|(label, _)| label.contains(partial))
        .take(25)
        .map(|(label, id)| AutocompleteChoice::new(label, id))
        .collect()
}

fn repeat_label(r: &HwRepeat) -> String {
    format!(
        "#{} 毎週{}曜 {} {}｜{}",
        r.id,
        WEEKDAYS_JA[r.weekday as usize % 7],
        r.time,
        r.subject,
        r.title
    )
}

// ────────────────────────────────────────────────────────────────────────────
// Command group
// ────────────────────────────────────────────────────────────────────────────

/// 宿題の管理（課題はみんなで共有、完了は自分ごと）
#[poise::command(
    slash_command,
    subcommands(
        "add", "todo", "list", "done", "undo", "view", "edit", "delete", "exam", "repeat",
        "settings", "notify"
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
    register(ctx, HwKind::Homework, subject, title, due, memo).await
}

async fn register(
    ctx: Context<'_>,
    kind: HwKind,
    subject: String,
    title: String,
    due: String,
    memo: Option<String>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;
    let Some(due_date) = require_valid_due!(ctx, guild_id, Some(due)) else {
        return Ok(());
    };

    let id = db
        .hw_add(NewHomework {
            kind,
            guild_id: guild_id.clone(),
            channel_id: ctx.channel_id().to_string(),
            created_by: ctx.author().id.to_string(),
            subject: subject.trim().to_string(),
            title,
            description: memo,
            due_date,
            repeat_id: None,
        })
        .await?;

    let Some(hw) = db.hw_get(id, guild_id).await? else {
        return reply_hw_not_found(ctx, id).await;
    };
    let reply = match kind {
        HwKind::Homework => poise::CreateReply::default()
            .embed(
                hw_embed(&hw, "📚 宿題を登録しました", Colour(0x2ECC71)).footer(
                    serenity::CreateEmbedFooter::new(
                        "終わったらボタンか /hw done で自分の分を完了にできます",
                    ),
                ),
            )
            .components(done_buttons(std::slice::from_ref(&hw), false)),
        HwKind::Exam => poise::CreateReply::default().embed(hw_embed(
            &hw,
            "📝 テストを登録しました",
            Colour(0xE74C3C),
        )),
    };
    ctx.send(reply).await?;
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
        kind: Some(HwKind::Homework),
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
    show_list(ctx, HwKind::Homework, subject, past.unwrap_or(false)).await
}

async fn show_list(
    ctx: Context<'_>,
    kind: HwKind,
    subject: Option<String>,
    past: bool,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let now = local_now().format(DUE_FORMAT).to_string();

    let query = HwQuery {
        kind: Some(kind),
        subject: subject.clone(),
        due_from: (!past).then(|| now.clone()),
        due_until: past.then(|| now.clone()),
        newest_first: past,
        ..Default::default()
    };
    let list = ctx.data().db.hw_list(guild_id, query).await?;

    if list.is_empty() {
        let msg = match (kind, past) {
            (HwKind::Homework, true) => "期限が過ぎた宿題はありません。",
            (HwKind::Homework, false) => "これからの宿題はありません。`/hw add` で登録できます。",
            (HwKind::Exam, true) => "終わったテストはありません。",
            (HwKind::Exam, false) => {
                "これからのテストはありません。`/hw exam add` で登録できます。"
            }
        };
        reply_ephemeral(ctx, msg).await?;
        return Ok(());
    }

    let mut title = match (kind, past) {
        (HwKind::Homework, true) => "🗂️ 期限が過ぎた宿題",
        (HwKind::Homework, false) => "📚 宿題一覧",
        (HwKind::Exam, true) => "🗂️ 終わったテスト",
        (HwKind::Exam, false) => "📝 テストの予定",
    }
    .to_string();
    if let Some(s) = &subject {
        title += &format!("（{}）", truncate(s, 30));
    }
    let me = ctx.author().id.to_string();
    let colour = match kind {
        HwKind::Homework => Colour(0x9B59B6),
        HwKind::Exam => Colour(0xE74C3C),
    };
    let pages = build_pages(&title, colour, &list, |hw| hw_field(hw, &me));
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

    let Some(hw) = db.hw_get(id, guild_id.clone()).await? else {
        return reply_hw_not_found(ctx, id).await;
    };
    if hw.kind == HwKind::Exam {
        return reply_ephemeral(ctx, "テストには完了の記録はありません。").await;
    }
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

/// 宿題・テストの詳細と完了した人を表示する
#[poise::command(slash_command)]
pub async fn view(
    ctx: Context<'_>,
    #[description = "宿題・テスト（入力すると候補が出ます）"]
    #[autocomplete = "ac_any"]
    id: i64,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let Some(hw) = ctx.data().db.hw_get(id, guild_id).await? else {
        return reply_hw_not_found(ctx, id).await;
    };
    let colour = match hw.kind {
        HwKind::Exam => Colour(0xE74C3C),
        HwKind::Homework if hw.is_done_by(&ctx.author().id.to_string()) => Colour(0x2ECC71),
        HwKind::Homework => Colour(0xF1C40F),
    };
    let title = format!("{} {} #{}", hw.kind.emoji(), hw.kind.display(), hw.id);
    let embed = hw_embed(&hw, &title, colour);
    ctx.send(
        poise::CreateReply::default()
            .embed(embed)
            .components(done_buttons(std::slice::from_ref(&hw), false)),
    )
    .await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw edit
// ────────────────────────────────────────────────────────────────────────────

/// 宿題・テストを編集する（期限を変えると通知も再設定されます）
#[poise::command(slash_command)]
pub async fn edit(
    ctx: Context<'_>,
    #[description = "宿題・テスト（入力すると候補が出ます）"]
    #[autocomplete = "ac_any"]
    id: i64,
    #[description = "新しい科目"]
    #[autocomplete = "ac_subject"]
    #[max_length = 30]
    subject: Option<String>,
    #[description = "新しい内容"]
    #[max_length = 100]
    title: Option<String>,
    #[description = "新しい期限・日時（例: 明日 / 金曜 / 12/5 13:00）"] due: Option<String>,
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
    let due_date = require_valid_due!(ctx, guild_id, due);

    let edit = HwEdit {
        subject: subject.map(|s| s.trim().to_string()),
        title,
        description: memo,
        due_date,
    };
    let Some(hw) = db.hw_edit(id, guild_id, edit).await? else {
        return reply_hw_not_found(ctx, id).await;
    };

    let title = format!("✏️ {} #{} を更新しました", hw.kind.display(), hw.id);
    let embed = hw_embed(&hw, &title, Colour(0x3498DB));
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw delete
// ────────────────────────────────────────────────────────────────────────────

/// 宿題・テストを削除する（登録した人か、メッセージの管理権限がある人のみ）
#[poise::command(slash_command)]
pub async fn delete(
    ctx: Context<'_>,
    #[description = "宿題・テスト（入力すると候補が出ます）"]
    #[autocomplete = "ac_any"]
    id: i64,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;

    let Some(hw) = db.hw_get(id, guild_id.clone()).await? else {
        return reply_hw_not_found(ctx, id).await;
    };
    if !can_modify(ctx, &hw.created_by).await {
        reply_ephemeral(
            ctx,
            format!(
                "#{} を削除できるのは登録した <@{}> か、メッセージの管理権限がある人だけです。",
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
        "🗑️ {} #{} {} を削除しました。",
        hw.kind.display(),
        hw.id,
        hw.label()
    ))
    .await?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// /hw exam
// ────────────────────────────────────────────────────────────────────────────

/// テスト・小テストの予定（編集・削除は /hw edit・/hw delete）
#[poise::command(slash_command, subcommands("exam_add", "exam_list"))]
pub async fn exam(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// テスト・小テストを登録する（まとめ投稿や通知にも載ります）
#[poise::command(slash_command, rename = "add")]
pub async fn exam_add(
    ctx: Context<'_>,
    #[description = "科目（例: 英語）"]
    #[autocomplete = "ac_subject"]
    #[max_length = 30]
    subject: String,
    #[description = "内容（例: 単語テスト Unit 3）"]
    #[max_length = 100]
    title: String,
    #[description = "日時（例: 明日 / 金曜 10:40 / 12/5）"] date: String,
    #[description = "メモ（範囲・持ち物など）"]
    #[max_length = 1000]
    memo: Option<String>,
) -> Result<(), Error> {
    register(ctx, HwKind::Exam, subject, title, date, memo).await
}

/// テストの予定を表示する
#[poise::command(slash_command, rename = "list")]
pub async fn exam_list(
    ctx: Context<'_>,
    #[description = "科目で絞り込み"]
    #[autocomplete = "ac_subject"]
    subject: Option<String>,
    #[description = "終わったテストを表示する (デフォルト: false)"] past: Option<bool>,
) -> Result<(), Error> {
    show_list(ctx, HwKind::Exam, subject, past.unwrap_or(false)).await
}

// ────────────────────────────────────────────────────────────────────────────
// /hw repeat
// ────────────────────────────────────────────────────────────────────────────

/// 毎週出る宿題（次の回を自動で登録します）
#[poise::command(
    slash_command,
    subcommands("repeat_add", "repeat_list", "repeat_delete")
)]
pub async fn repeat(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// 毎週の宿題を登録する（期限が過ぎると翌週の分が自動で登録されます）
#[poise::command(slash_command, rename = "add")]
pub async fn repeat_add(
    ctx: Context<'_>,
    #[description = "科目（例: 英語）"]
    #[autocomplete = "ac_subject"]
    #[max_length = 30]
    subject: String,
    #[description = "内容（例: 単語テストの勉強）"]
    #[max_length = 100]
    title: String,
    #[description = "期限の曜日"] weekday: WeekdayChoice,
    #[description = "期限の時刻（例: 08:30、省略時はサーバーの設定）"] time: Option<String>,
    #[description = "メモ"]
    #[max_length = 1000]
    memo: Option<String>,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;

    let time = match time {
        Some(input) => {
            match parse_time(&input) {
                Some(t) => t,
                None => {
                    return reply_ephemeral(
                    ctx,
                    format!("時刻 `{input}` を解釈できませんでした。`08:30` のように指定してください。"),
                )
                .await;
                }
            }
        }
        None => {
            let settings = db.hw_settings(guild_id.clone()).await?;
            parse_time(&settings.default_time).unwrap_or_default()
        }
    };

    let repeat = HwRepeat {
        id: 0,
        guild_id: guild_id.clone(),
        channel_id: ctx.channel_id().to_string(),
        created_by: ctx.author().id.to_string(),
        subject: subject.trim().to_string(),
        title,
        description: memo,
        weekday: weekday.index(),
        time: time.format("%H:%M").to_string(),
        last_due: None,
    };
    let rid = db.hw_repeat_add(repeat.clone()).await?;

    // 最初の回をすぐに登録する
    let first_due = next_weekly(local_now(), repeat.weekday, time)
        .format(DUE_FORMAT)
        .to_string();
    let first = match db.hw_repeat_generate(rid, first_due).await? {
        Some(id) => db.hw_get(id, guild_id).await?,
        None => None,
    };

    let repeat = HwRepeat { id: rid, ..repeat };
    let mut embed = CreateEmbed::new()
        .title("🔁 毎週の宿題を登録しました")
        .colour(Colour(0x1ABC9C))
        .field("設定", repeat_label(&repeat), false)
        .footer(serenity::CreateEmbedFooter::new(
            "期限が過ぎると翌週の分が自動で登録されます。止めるときは /hw repeat delete",
        ));
    let mut reply = poise::CreateReply::default();
    if let Some(hw) = &first {
        embed = embed.field(
            "今回の分",
            format!("#{} 期限: {}", hw.id, format_due(Some(&hw.due_date))),
            false,
        );
        reply = reply.components(done_buttons(std::slice::from_ref(hw), false));
    }
    ctx.send(reply.embed(embed)).await?;
    Ok(())
}

/// 毎週の宿題の一覧
#[poise::command(slash_command, rename = "list")]
pub async fn repeat_list(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let list = ctx.data().db.hw_repeats(Some(guild_id)).await?;
    if list.is_empty() {
        return reply_ephemeral(
            ctx,
            "毎週の宿題はありません。`/hw repeat add` で登録できます。",
        )
        .await;
    }
    let pages = build_pages("🔁 毎週の宿題", Colour(0x1ABC9C), &list, |r| {
        (
            truncate(&repeat_label(r), 100),
            format!(
                "登録者: <@{}> | メモ: {}",
                r.created_by,
                truncate(r.description.as_deref().unwrap_or("なし"), 200)
            ),
        )
    });
    send_paginated(ctx, pages, false).await
}

/// 毎週の宿題を止める（登録済みの宿題は残ります）
#[poise::command(slash_command, rename = "delete")]
pub async fn repeat_delete(
    ctx: Context<'_>,
    #[description = "毎週の宿題（入力すると候補が出ます）"]
    #[autocomplete = "ac_repeat"]
    id: i64,
) -> Result<(), Error> {
    let guild_id = require_guild!(ctx);
    let db = &ctx.data().db;

    let Some(r) = db.hw_repeat_get(id, guild_id.clone()).await? else {
        return reply_ephemeral(ctx, format!("毎週の宿題 #{} が見つかりません。", id)).await;
    };
    if !can_modify(ctx, &r.created_by).await {
        return reply_ephemeral(
            ctx,
            format!(
                "止められるのは登録した <@{}> か、メッセージの管理権限がある人だけです。",
                r.created_by
            ),
        )
        .await;
    }
    db.hw_repeat_delete(id, guild_id).await?;
    ctx.say(format!(
        "⏹️ 毎週の宿題 {} を止めました（登録済みの宿題は残ります）。",
        repeat_label(&r)
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
        .field(
            "時刻を省略したときの期限（/task も共通）",
            &s.default_time,
            true,
        )
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

/// 自分が未完了の宿題とテストを DM で知らせてもらう（自分だけの設定）
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
            "🔔 未完了の宿題とテストを **{}** に DM でお知らせします。\n（サーバーメンバーからの DM を許可しておいてください）",
            format_duration(secs)
        ),
        None => "🔕 DM 通知をオフにしました。".to_string(),
    };
    reply_ephemeral(ctx, msg).await
}
