use poise::serenity_prelude::{
    self as serenity, Colour, CreateActionRow, CreateButton, CreateEmbed, CreateEmbedFooter,
    CreateInteractionResponse, CreateInteractionResponseMessage, GuildId,
};

use crate::{Context, Error, time::timezone};

/// 一覧の1ページあたりの件数
const PAGE_SIZE: usize = 10;
/// ページ送りボタンの受付時間
const PAGINATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

// ────────────────────────────────────────────────────────────────────────────
// /now
// ────────────────────────────────────────────────────────────────────────────

/// 現在の日時を表示する
#[poise::command(slash_command)]
pub async fn now(ctx: Context<'_>) -> Result<(), Error> {
    let now = chrono::Utc::now();
    let unix = now.timestamp();
    let tz = timezone();
    let msg = format!(
        "🕐 **現在時刻**\n\
         日時: <t:{unix}:F>\n\
         相対: <t:{unix}:R>\n\
         ボットのタイムゾーン ({tz}): `{}`",
        now.with_timezone(&tz).format("%Y-%m-%d %H:%M"),
    );
    ctx.say(msg).await?;
    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum RemindChoice {
    #[name = "30分前"]
    ThirtyMin,
    #[name = "1時間前"]
    OneHour,
    #[name = "3時間前"]
    ThreeHours,
    #[name = "1日前"]
    OneDay,
    #[name = "3日前"]
    ThreeDays,
    #[name = "1週間前"]
    OneWeek,
}

impl RemindChoice {
    pub fn to_seconds(&self) -> i64 {
        match self {
            Self::ThirtyMin => 30 * 60,
            Self::OneHour => 3600,
            Self::ThreeHours => 3 * 3600,
            Self::OneDay => 24 * 3600,
            Self::ThreeDays => 3 * 24 * 3600,
            Self::OneWeek => 7 * 24 * 3600,
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Helper
// ────────────────────────────────────────────────────────────────────────────

macro_rules! require_guild {
    ($ctx:expr) => {
        match $ctx.guild_id() {
            Some(id) => id.to_string(),
            None => {
                reply_ephemeral($ctx, "このコマンドはサーバー内でのみ使用できます。").await?;
                return Ok(());
            }
        }
    };
}

/// 期限を検証・正規化する。不正な書式ならエラーを返信して呼び出し元を return させる
macro_rules! require_valid_due {
    ($ctx:expr, $due:expr) => {
        match $due {
            None => None,
            Some(raw) => match normalize_due(&raw) {
                Some(due) => Some(due),
                None => {
                    reply_ephemeral(
                        $ctx,
                        format!(
                            "期限 `{}` を解釈できませんでした。`YYYY-MM-DD HH:MM`（例: `2025-12-31 15:00`）の形式で指定してください。",
                            raw
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            },
        }
    };
}

async fn reply_ephemeral(ctx: Context<'_>, content: impl Into<String>) -> Result<(), Error> {
    ctx.send(
        poise::CreateReply::default()
            .ephemeral(true)
            .content(content),
    )
    .await?;
    Ok(())
}

async fn reply_not_found(ctx: Context<'_>, id: i64) -> Result<(), Error> {
    reply_ephemeral(ctx, format!("タスク #{} が見つかりません。", id)).await
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_chars - 1).collect::<String>())
    }
}

fn guild(guild_id: &str) -> GuildId {
    GuildId::new(guild_id.parse().unwrap_or(1))
}

/// 一覧をページごとの Embed に分ける（1ページ PAGE_SIZE 件、フッターに件数とページ番号）
fn build_pages<T>(
    title: &str,
    colour: Colour,
    items: &[T],
    to_field: impl Fn(&T) -> (String, String),
) -> Vec<CreateEmbed> {
    let pages = items.len().div_ceil(PAGE_SIZE).max(1);
    (0..pages)
        .map(|page| {
            let mut embed = CreateEmbed::new().title(title).colour(colour);
            for item in items.iter().skip(page * PAGE_SIZE).take(PAGE_SIZE) {
                let (name, value) = to_field(item);
                embed = embed.field(name, value, false);
            }
            let footer = if pages > 1 {
                format!("合計: {} 件 | ページ {}/{}", items.len(), page + 1, pages)
            } else {
                format!("合計: {} 件", items.len())
            };
            embed.footer(CreateEmbedFooter::new(footer))
        })
        .collect()
}

/// Embed のページを送信する。複数ページならボタンでページ送りできるようにする
async fn send_paginated(
    ctx: Context<'_>,
    pages: Vec<CreateEmbed>,
    ephemeral: bool,
) -> Result<(), Error> {
    let reply = |page: usize| {
        poise::CreateReply::default()
            .ephemeral(ephemeral)
            .embed(pages[page].clone())
    };

    if pages.len() <= 1 {
        ctx.send(reply(0)).await?;
        return Ok(());
    }

    let ctx_id = ctx.id();
    let prev_id = format!("{ctx_id}:prev");
    let next_id = format!("{ctx_id}:next");
    let buttons = |page: usize| {
        vec![CreateActionRow::Buttons(vec![
            CreateButton::new(&prev_id).emoji('◀').disabled(page == 0),
            CreateButton::new(&next_id)
                .emoji('▶')
                .disabled(page + 1 >= pages.len()),
        ])]
    };

    let mut page = 0;
    let handle = ctx.send(reply(page).components(buttons(page))).await?;

    let prefix = format!("{ctx_id}:");
    while let Some(press) = serenity::ComponentInteractionCollector::new(ctx)
        .filter({
            let prefix = prefix.clone();
            move |press| press.data.custom_id.starts_with(&prefix)
        })
        .timeout(PAGINATION_TIMEOUT)
        .await
    {
        if press.data.custom_id == next_id {
            page = (page + 1).min(pages.len() - 1);
        } else if press.data.custom_id == prev_id {
            page = page.saturating_sub(1);
        } else {
            continue;
        }

        press
            .create_response(
                ctx.serenity_context(),
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .embed(pages[page].clone())
                        .components(buttons(page)),
                ),
            )
            .await?;
    }

    // タイムアウトしたらボタンを消す（インタラクションの有効期限切れで失敗しても無視する）
    if let Err(e) = handle.edit(ctx, reply(page).components(vec![])).await {
        eprintln!("ページ送りボタンの削除に失敗: {:?}", e);
    }
    Ok(())
}

mod hw;
mod task;

pub use hw::hw;
pub use task::task;

// ────────────────────────────────────────────────────────────────────────────
// /help
// ────────────────────────────────────────────────────────────────────────────

/// コマンド一覧と使い方を表示する
#[poise::command(slash_command)]
pub async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let embed = CreateEmbed::new()
        .title("📖 タスクボット コマンド一覧")
        .colour(Colour(0x5865F2))
        .field(
            "/task add",
            "タスクを追加する\n\
             `title` タイトル（必須）\n\
             `description` 説明\n\
             `priority` 優先度（低/中/高、デフォルト: 中）\n\
             `due_date` 期限（例: `2025-12-31 15:00`）\n\
             `assignee` 担当者\n\
             `remind1〜3` リマインダー（30分前〜1週間前）\n\
             `create_event` Discord イベントも作成する（true/false）",
            false,
        )
        .field(
            "/task list",
            "タスク一覧を表示する（10件ごとにページ送り）\n\
             `filter` ステータスで絞り込み（すべて/未完了/待機中/進行中/完了）\n\
             `assignee` 担当者で絞り込み\n\
             `sort` 並び順（優先度順/期限が近い順/新しい順）",
            false,
        )
        .field(
            "/task search",
            "タイトル・説明からキーワード検索する\n\
             `keyword` キーワード（必須）\n\
             `filter` ステータスで絞り込み",
            false,
        )
        .field(
            "/task view",
            "タスクの詳細を表示する\n\
             `id` タスクID（必須）",
            false,
        )
        .field(
            "/task status",
            "タスクのステータスを変更する\n\
             `id` タスクID（必須）\n\
             `new_status` 新しいステータス（待機中/進行中/完了）",
            false,
        )
        .field(
            "/task assign ・ /task unassign",
            "担当者を追加する（最大3人まで同時指定）・外す\n\
             リマインダーや期限切れ通知は担当者にメンションされる",
            false,
        )
        .field(
            "/task edit",
            "タスクを編集する\n\
             `id` タスクID（必須）\n\
             `title` / `description` / `priority` / `due_date` 各項目\n\
             `remind1〜3` を1つでも指定するとリマインダーが全置き換えされる\n\
             Discord イベントが紐づいていれば内容を同期する",
            false,
        )
        .field(
            "/task delete",
            "タスクを削除する（紐づく Discord イベントも削除）\n\
             `id` タスクID（必須）",
            false,
        )
        .field("/now", "現在の日時を表示する", false)
        .field(
            "期限の書式",
            format!(
                "`YYYY-MM-DD HH:MM` （例: `2025-12-31 09:00`）\n\
                 時刻を省略すると `00:00` 扱い、`/` 区切りも可\n\
                 時刻は **{}** として解釈される",
                timezone()
            ),
            false,
        )
        .field(
            "自動通知",
            "⏰ 設定したリマインダーの時刻になると作成したチャンネルに通知\n\
             🚨 未完了のまま期限を過ぎたタスクを1回だけ通知",
            false,
        );

    let hw_embed = CreateEmbed::new()
        .title("📚 宿題コマンド（課題はみんなで共有、完了は自分ごと）")
        .colour(Colour(0x9B59B6))
        .field(
            "/hw add",
            "宿題を登録する\n\
             `subject` 科目 / `title` 内容 / `due` 期限 / `memo` メモ",
            false,
        )
        .field(
            "/hw todo ・ /hw list",
            "`todo` 自分が終わっていない宿題（自分にだけ表示）\n\
             `list` みんなの宿題一覧（`past:true` で期限切れ）",
            false,
        )
        .field(
            "/hw done ・ /hw undo",
            "自分の分を完了にする・取り消す（候補から選べます）",
            false,
        )
        .field(
            "/hw view ・ /hw edit ・ /hw delete",
            "詳細と完了した人の表示・編集・削除（削除は登録者か管理者）",
            false,
        )
        .field(
            "/hw settings",
            "サーバーの設定: 通知チャンネル / 期限前の通知 / 毎日のまとめ投稿 / 時刻省略時の時刻",
            false,
        )
        .field(
            "/hw notify",
            "自分が未完了の宿題を期限前に DM で知らせてもらう",
            false,
        )
        .field(
            "期限の書き方",
            "`明日` `明後日 17:00` `明日17時` `金曜` `12/5` `12/5 13:00` `2025-12-05`",
            false,
        );

    ctx.send(poise::CreateReply::default().embed(embed).embed(hw_embed))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discord のスラッシュコマンドの制約（名前・説明の長さ、件数、必須引数の順序）を満たすか
    fn check_command(cmd: &poise::Command<crate::Data, Error>, path: &str) {
        let path = format!("{path} {}", cmd.name);
        assert!(cmd.name.len() <= 32, "{path}: 名前が長すぎる");
        let desc = cmd.description.as_deref().unwrap_or("");
        assert!(
            (1..=100).contains(&desc.chars().count()),
            "{path}: 説明は1〜100文字 ({desc})"
        );
        assert!(
            cmd.subcommands.len() <= 25,
            "{path}: サブコマンドが多すぎる"
        );
        assert!(cmd.parameters.len() <= 25, "{path}: 引数が多すぎる");

        let mut seen_optional = false;
        for p in &cmd.parameters {
            assert!(p.name.len() <= 32, "{path} {}: 名前が長すぎる", p.name);
            let d = p.description.as_deref().unwrap_or("");
            assert!(
                (1..=100).contains(&d.chars().count()),
                "{path} {}: 説明は1〜100文字",
                p.name
            );
            assert!(p.choices.len() <= 25, "{path} {}: 選択肢が多すぎる", p.name);
            if p.required {
                assert!(
                    !seen_optional,
                    "{path} {}: 必須引数は任意引数より前に",
                    p.name
                );
            } else {
                seen_optional = true;
            }
        }
        for sub in &cmd.subcommands {
            check_command(sub, &path);
        }
    }

    #[test]
    fn commands_satisfy_discord_limits() {
        let commands = vec![task(), hw(), now(), help()];
        for cmd in &commands {
            check_command(cmd, "");
        }
        // 登録用 JSON が組み立てられること
        let built = poise::builtins::create_application_commands(&commands);
        assert_eq!(built.len(), commands.len());
    }

    #[test]
    fn truncates_by_chars() {
        assert_eq!(truncate("あいうえお", 5), "あいうえお");
        assert_eq!(truncate("あいうえおか", 5), "あいうえ…");
    }
}
