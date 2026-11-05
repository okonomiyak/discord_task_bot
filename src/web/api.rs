use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, patch, post, put},
};
use poise::serenity_prelude::{ChannelId, CreateMessage};
use serde::{Deserialize, Serialize};

use super::{ApiError, AppState, Auth, WebGuild, discord::ChannelInfo};
use crate::{
    commands::{WebTab, delete_event, done_buttons, gcal_button, sync_event, web_button},
    db::{HwEdit, TaskEdit},
    hw_notify::{role_allowed_mentions, with_role_mention},
    models::{
        Homework, HwKind, HwQuery, HwRepeat, HwSettings, HwSettingsPatch, NewHomework, Priority,
        Task, TaskQuery, TaskSort, TaskStatus,
    },
    stats::{self as hw_stats, ClassStats, HwStats, StatsPeriod},
    time::{DUE_FORMAT, due_to_utc, local_now, next_weekly, parse_due_input, parse_time, timezone},
};

type ApiResult<T> = Result<Json<T>, ApiError>;

/// `/hw todo` と同じく、期限切れでも7日以内なら「やること」に表示する
const TODO_OVERDUE_DAYS: i64 = 7;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/guilds/{gid}/subjects", get(subjects))
        .route("/guilds/{gid}/stats", get(stats))
        .route("/guilds/{gid}/search", get(search))
        .route(
            "/guilds/{gid}/homework",
            get(list_homework).post(create_homework),
        )
        .route(
            "/guilds/{gid}/homework/{id}",
            patch(edit_homework).delete(delete_homework),
        )
        .route("/guilds/{gid}/homework/{id}/done", post(set_done))
        .route("/guilds/{gid}/homework/{id}/done_by", get(done_by))
        .route(
            "/guilds/{gid}/settings",
            get(get_settings).patch(update_settings),
        )
        .route("/guilds/{gid}/me/notify", put(set_my_notify))
        .route(
            "/guilds/{gid}/repeats",
            get(list_repeats).post(create_repeat),
        )
        .route("/guilds/{gid}/repeats/{id}", delete(remove_repeat))
        .route("/guilds/{gid}/tasks", get(list_tasks).post(create_task))
        .route(
            "/guilds/{gid}/tasks/{id}",
            patch(edit_task).delete(remove_task),
        )
}

// ────────────────────────────────────────────────────────────────────────────
// 共通
// ────────────────────────────────────────────────────────────────────────────

/// 期限の入力を解釈する（`明日` `金曜 17:00` のほか、日時ピッカーの `2025-12-05T13:00` も可）
fn parse_due(input: &str, settings: &HwSettings) -> Result<String, ApiError> {
    let default_time = parse_time(&settings.default_time).unwrap_or_default();
    parse_due_input(&input.replace('T', " "), local_now(), default_time)
        .map(|d| d.format(DUE_FORMAT).to_string())
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "期限「{input}」を解釈できませんでした（例: 明日 / 金曜 17:00 / 12/5）"
            ))
        })
}

fn required(value: &str, name: &str, max_chars: usize) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::bad_request(format!("{name}を入力してください")));
    }
    optional(Some(value.to_string()), name, max_chars).map(|v| v.unwrap_or_default())
}

/// 空文字は None 扱い
fn optional(
    value: Option<String>,
    name: &str,
    max_chars: usize,
) -> Result<Option<String>, ApiError> {
    match value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        Some(v) if v.chars().count() > max_chars => Err(ApiError::bad_request(format!(
            "{name}は{max_chars}文字以内にしてください"
        ))),
        v => Ok(v),
    }
}

fn now_str() -> String {
    local_now().format(DUE_FORMAT).to_string()
}

fn due_unix(due: &str) -> i64 {
    due_to_utc(due).map(|d| d.timestamp()).unwrap_or(0)
}

/// Discord のチャンネルにも投稿する（メンションで通知は飛ばさない）
/// settings を渡すと、その通知ロールだけを鳴らす（None なら誰も鳴らさない）
async fn announce(
    state: &AppState,
    channel: Option<&str>,
    msg: CreateMessage,
    settings: Option<&HwSettings>,
) {
    let Some(id) = channel
        .and_then(|c| c.parse::<u64>().ok())
        .filter(|id| *id > 0)
    else {
        return;
    };
    let msg = msg.allowed_mentions(role_allowed_mentions(settings));
    if let Err(e) = ChannelId::new(id).send_message(&state.http, msg).await {
        eprintln!("Web からの登録の投稿に失敗しました: {e:?}");
    }
}

// ────────────────────────────────────────────────────────────────────────────
// /api/me
// ────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct MeDto {
    id: String,
    name: String,
    avatar_url: Option<String>,
    guilds: Vec<GuildDto>,
}

#[derive(Serialize)]
struct GuildDto {
    id: String,
    name: String,
    icon_url: Option<String>,
    can_manage: bool,
}

async fn me(State(state): State<AppState>, auth: Auth) -> ApiResult<MeDto> {
    let avatar_url = auth.avatar.as_ref().map(|a| {
        format!(
            "https://cdn.discordapp.com/avatars/{}/{a}.png?size=64",
            auth.user_id
        )
    });
    let guilds = auth
        .guilds
        .iter()
        .filter(|g| state.bot_in_guild(&g.id))
        .map(|g: &WebGuild| GuildDto {
            id: g.id.clone(),
            name: g.name.clone(),
            icon_url: g
                .icon
                .as_ref()
                .map(|i| format!("https://cdn.discordapp.com/icons/{}/{i}.png?size=64", g.id)),
            can_manage: g.can_manage,
        })
        .collect();
    Ok(Json(MeDto {
        id: auth.user_id.clone(),
        name: auth.username.clone(),
        avatar_url,
        guilds,
    }))
}

async fn subjects(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
) -> ApiResult<Vec<String>> {
    auth.guild(&state, &gid)?;
    Ok(Json(state.db.hw_subjects(gid).await?))
}

#[derive(Deserialize)]
struct StatsQuery {
    /// week / month / all（省略時 month）
    period: Option<String>,
}

#[derive(Serialize)]
struct StatsDto {
    label: String,
    #[serde(flatten)]
    stats: HwStats,
    class: ClassStats,
}

/// ログインしている人の宿題の達成率
async fn stats(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<StatsDto> {
    auth.guild(&state, &gid)?;
    let period = StatsPeriod::from_str(q.period.as_deref().unwrap_or("month"))
        .ok_or_else(|| ApiError::bad_request("period は week / month / all のどれかです"))?;
    let now = local_now();
    let (due_from, due_until, label) = period.range(now);
    let list = state
        .db
        .hw_list(
            gid.clone(),
            HwQuery {
                kind: Some(HwKind::Homework),
                due_from,
                due_until,
                ..Default::default()
            },
        )
        .await?;
    let participants = state.db.hw_participants(gid).await?;
    Ok(Json(StatsDto {
        label,
        stats: hw_stats::compute(&list, &auth.user_id, now),
        class: hw_stats::class_average(&list, &participants),
    }))
}

// ────────────────────────────────────────────────────────────────────────────
// 宿題・テスト
// ────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct HwDto {
    id: i64,
    kind: &'static str,
    subject: String,
    title: String,
    memo: Option<String>,
    due: String,
    due_unix: i64,
    done_count: usize,
    /// クラスの人数（宿題の完了を記録したことがある人）。done_count / class_size がクラスの完了率
    class_size: usize,
    done_by_me: bool,
    created_by: String,
    repeat: bool,
    can_delete: bool,
    /// Google カレンダーの「予定を作成」画面のリンク
    gcal_url: Option<String>,
}

fn hw_dto(
    state: &AppState,
    hw: Homework,
    auth: &Auth,
    guild: &WebGuild,
    class_size: usize,
) -> HwDto {
    HwDto {
        gcal_url: crate::gcal::calendar_url(&hw, Some(&state.config.base_url), None),
        class_size,
        id: hw.id,
        kind: hw.kind.as_str(),
        due_unix: due_unix(&hw.due_date),
        done_count: hw.done_by.len(),
        done_by_me: hw.is_done_by(&auth.user_id),
        can_delete: hw.created_by == auth.user_id || guild.can_manage,
        repeat: hw.repeat_id.is_some(),
        subject: hw.subject,
        title: hw.title,
        memo: hw.description,
        due: hw.due_date,
        created_by: hw.created_by,
    }
}

fn parse_kind(kind: Option<&str>) -> Result<Option<HwKind>, ApiError> {
    match kind {
        None | Some("") | Some("all") => Ok(None),
        Some("homework") => Ok(Some(HwKind::Homework)),
        Some("exam") => Ok(Some(HwKind::Exam)),
        Some(k) => Err(ApiError::bad_request(format!("kind `{k}` は不正です"))),
    }
}

#[derive(Deserialize)]
struct HwListQuery {
    /// todo（自分の未完了）/ upcoming（これから）/ past（期限切れ）/ range（from〜until）
    scope: Option<String>,
    kind: Option<String>,
    from: Option<String>,
    until: Option<String>,
}

async fn list_homework(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Query(q): Query<HwListQuery>,
) -> ApiResult<Vec<HwDto>> {
    let guild = auth.guild(&state, &gid)?.clone();
    let kind = parse_kind(q.kind.as_deref())?;
    let query = match q.scope.as_deref().unwrap_or("upcoming") {
        "todo" => HwQuery {
            kind: Some(HwKind::Homework),
            due_from: Some(
                (local_now() - chrono::Duration::days(TODO_OVERDUE_DAYS))
                    .format(DUE_FORMAT)
                    .to_string(),
            ),
            not_done_by: Some(auth.user_id.clone()),
            ..Default::default()
        },
        "upcoming" => HwQuery {
            kind,
            due_from: Some(now_str()),
            ..Default::default()
        },
        "past" => HwQuery {
            kind,
            due_until: Some(now_str()),
            newest_first: true,
            ..Default::default()
        },
        "range" => HwQuery {
            kind,
            due_from: q.from,
            due_until: q.until,
            ..Default::default()
        },
        s => return Err(ApiError::bad_request(format!("scope `{s}` は不正です"))),
    };
    let class_size = state.db.hw_participants(gid.clone()).await?.len();
    let list = state.db.hw_list(gid, query).await?;
    Ok(Json(
        list.into_iter()
            .map(|hw| hw_dto(&state, hw, &auth, &guild, class_size))
            .collect(),
    ))
}

#[derive(Deserialize)]
struct HwInput {
    kind: Option<String>,
    subject: Option<String>,
    title: Option<String>,
    due: Option<String>,
    memo: Option<String>,
}

async fn create_homework(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Json(input): Json<HwInput>,
) -> Result<(StatusCode, Json<HwDto>), ApiError> {
    let guild = auth.guild(&state, &gid)?.clone();
    let kind = parse_kind(input.kind.as_deref())?.unwrap_or(HwKind::Homework);
    let subject = required(input.subject.as_deref().unwrap_or(""), "科目", 30)?;
    let title = required(input.title.as_deref().unwrap_or(""), "内容", 100)?;
    let memo = optional(input.memo, "メモ", 1000)?;
    let settings = state.db.hw_settings(gid.clone()).await?;
    let due_date = parse_due(input.due.as_deref().unwrap_or(""), &settings)?;
    let channel = state.db.announce_channel(gid.clone()).await?;

    let (id, created) = state
        .db
        .hw_add(NewHomework {
            kind,
            guild_id: gid.clone(),
            channel_id: channel.clone().unwrap_or_else(|| "0".to_string()),
            created_by: auth.user_id.clone(),
            subject,
            title,
            description: memo,
            due_date,
            repeat_id: None,
        })
        .await?;
    let hw = state
        .db
        .hw_get(id, gid)
        .await?
        .ok_or_else(|| ApiError::not_found("登録した宿題が見つかりません"))?;

    if created {
        let content = format!(
            "{} <@{}> が Web から{}を登録しました：`#{}` **{}**｜{} — <t:{u}:f>（<t:{u}:R>）",
            hw.kind.emoji(),
            auth.user_id,
            hw.kind.display(),
            hw.id,
            hw.subject,
            hw.title,
            u = due_unix(&hw.due_date),
        );
        let msg = CreateMessage::new()
            .content(with_role_mention(Some(&settings), content))
            .components(done_buttons(
                std::slice::from_ref(&hw),
                false,
                [
                    gcal_button(&hw, Some(&state.config.base_url)),
                    web_button(
                        Some(&state.config.base_url),
                        Some(&hw.guild_id),
                        WebTab::from(hw.kind),
                    ),
                ]
                .into_iter()
                .flatten(),
            ));
        announce(&state, channel.as_deref(), msg, Some(&settings)).await;
    }

    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let class_size = state.db.hw_participants(hw.guild_id.clone()).await?.len();
    Ok((status, Json(hw_dto(&state, hw, &auth, &guild, class_size))))
}

async fn edit_homework(
    State(state): State<AppState>,
    auth: Auth,
    Path((gid, id)): Path<(String, i64)>,
    Json(input): Json<HwInput>,
) -> ApiResult<HwDto> {
    let guild = auth.guild(&state, &gid)?.clone();
    let due_date = match input
        .due
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(due) => {
            let settings = state.db.hw_settings(gid.clone()).await?;
            Some(parse_due(due, &settings)?)
        }
        None => None,
    };
    let edit = HwEdit {
        subject: optional(input.subject, "科目", 30)?,
        title: optional(input.title, "内容", 100)?,
        description: optional(input.memo, "メモ", 1000)?,
        due_date,
    };
    let hw = state
        .db
        .hw_edit(id, gid, edit)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("#{id} が見つかりません")))?;
    let class_size = state.db.hw_participants(hw.guild_id.clone()).await?.len();
    Ok(Json(hw_dto(&state, hw, &auth, &guild, class_size)))
}

async fn delete_homework(
    State(state): State<AppState>,
    auth: Auth,
    Path((gid, id)): Path<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    let guild = auth.guild(&state, &gid)?.clone();
    let hw = state
        .db
        .hw_get(id, gid.clone())
        .await?
        .ok_or_else(|| ApiError::not_found(format!("#{id} が見つかりません")))?;
    if hw.created_by != auth.user_id && !guild.can_manage {
        return Err(ApiError::forbidden(
            "削除できるのは登録した人か、メッセージの管理権限がある人だけです",
        ));
    }
    state.db.hw_delete(id, gid).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DoneInput {
    done: bool,
}

async fn set_done(
    State(state): State<AppState>,
    auth: Auth,
    Path((gid, id)): Path<(String, i64)>,
    Json(input): Json<DoneInput>,
) -> ApiResult<HwDto> {
    let guild = auth.guild(&state, &gid)?.clone();
    let hw = state
        .db
        .hw_get(id, gid.clone())
        .await?
        .ok_or_else(|| ApiError::not_found(format!("#{id} が見つかりません")))?;
    if hw.kind == HwKind::Exam {
        return Err(ApiError::bad_request("テストには完了の記録はありません"));
    }
    state
        .db
        .hw_set_done(id, gid.clone(), auth.user_id.clone(), input.done)
        .await?;
    let hw = state
        .db
        .hw_get(id, gid)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("#{id} が見つかりません")))?;
    let class_size = state.db.hw_participants(hw.guild_id.clone()).await?.len();
    Ok(Json(hw_dto(&state, hw, &auth, &guild, class_size)))
}

#[derive(Serialize)]
struct DoneUserDto {
    id: String,
    name: String,
    avatar_url: Option<String>,
}

/// 宿題を完了した人（サーバーでの表示名とアイコン）
async fn done_by(
    State(state): State<AppState>,
    auth: Auth,
    Path((gid, id)): Path<(String, i64)>,
) -> ApiResult<Vec<DoneUserDto>> {
    auth.guild(&state, &gid)?;
    let hw = state
        .db
        .hw_get(id, gid.clone())
        .await?
        .ok_or_else(|| ApiError::not_found(format!("#{id} が見つかりません")))?;
    let mut users = Vec::with_capacity(hw.done_by.len());
    for user_id in hw.done_by {
        let info = state.discord.member(&gid, &user_id).await;
        users.push(DoneUserDto {
            name: info
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| "（サーバーにいないユーザー）".to_string()),
            avatar_url: info.and_then(|m| m.avatar_url),
            id: user_id,
        });
    }
    Ok(Json(users))
}

// ────────────────────────────────────────────────────────────────────────────
// 検索
// ────────────────────────────────────────────────────────────────────────────

/// 検索結果の上限（種類ごと）
const SEARCH_LIMIT: usize = 50;

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

#[derive(Serialize)]
struct SearchDto {
    homework: Vec<HwDto>,
    tasks: Vec<TaskDto>,
}

/// 宿題・テスト（科目・内容・メモ）とタスク（タイトル・説明）をキーワードで探す。
/// 過去のものも含め、宿題は期限の新しい順・タスクは期限の近い順で返す
async fn search(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<SearchDto> {
    let guild = auth.guild(&state, &gid)?.clone();
    let keyword = q.q.as_deref().map(str::trim).unwrap_or("");
    if keyword.is_empty() {
        return Ok(Json(SearchDto {
            homework: vec![],
            tasks: vec![],
        }));
    }
    if keyword.chars().count() > 100 {
        return Err(ApiError::bad_request(
            "キーワードは100文字以内にしてください",
        ));
    }

    let class_size = state.db.hw_participants(gid.clone()).await?.len();
    let homework = state
        .db
        .hw_list(
            gid.clone(),
            HwQuery {
                keyword: Some(keyword.to_string()),
                newest_first: true,
                ..Default::default()
            },
        )
        .await?;
    let tasks = state
        .db
        .list_tasks(
            gid,
            TaskQuery {
                keyword: Some(keyword.to_string()),
                sort: TaskSort::DueDate,
                ..Default::default()
            },
        )
        .await?;
    Ok(Json(SearchDto {
        homework: homework
            .into_iter()
            .take(SEARCH_LIMIT)
            .map(|hw| hw_dto(&state, hw, &auth, &guild, class_size))
            .collect(),
        tasks: tasks.into_iter().take(SEARCH_LIMIT).map(task_dto).collect(),
    }))
}

// ────────────────────────────────────────────────────────────────────────────
// 設定
// ────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RoleDto {
    id: String,
    name: Option<String>,
}

#[derive(Serialize)]
struct SettingsDto {
    channel_id: Option<String>,
    /// 投稿先に選べるチャンネル（取得できなければ空）
    channels: Vec<ChannelInfo>,
    /// 期限前のチャンネル通知（秒、None でオフ）
    remind_before: Option<i64>,
    remind_choices: Vec<i64>,
    /// 毎日のまとめ投稿の時刻（None でオフ）
    summary_time: Option<String>,
    default_time: String,
    notify_role: Option<RoleDto>,
    /// 自分への DM 通知（秒、None でオフ）
    my_dm_remind_before: Option<i64>,
    timezone: String,
}

async fn settings_dto(state: &AppState, gid: &str, user_id: &str) -> Result<SettingsDto, ApiError> {
    let s = state.db.hw_settings(gid.to_string()).await?;
    let notify_role = match &s.notify_role_id {
        Some(id) => Some(RoleDto {
            id: id.clone(),
            name: state.discord.role_name(gid, id).await,
        }),
        None => None,
    };
    Ok(SettingsDto {
        channels: state.discord.text_channels(gid).await,
        channel_id: s.channel_id,
        remind_before: s.remind_before,
        remind_choices: HwSettings::REMIND_CHOICES.to_vec(),
        summary_time: s.summary_time,
        default_time: s.default_time,
        notify_role,
        my_dm_remind_before: state
            .db
            .hw_get_dm(gid.to_string(), user_id.to_string())
            .await?,
        timezone: timezone().to_string(),
    })
}

async fn get_settings(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
) -> ApiResult<SettingsDto> {
    auth.guild(&state, &gid)?;
    Ok(Json(settings_dto(&state, &gid, &auth.user_id).await?))
}

/// 通知タイミングの入力（null / 0 でオフ、それ以外は選択肢のどれか）
fn parse_remind(value: Option<i64>) -> Result<Option<i64>, ApiError> {
    match value {
        None | Some(0) => Ok(None),
        Some(secs) if HwSettings::REMIND_CHOICES.contains(&secs) => Ok(Some(secs)),
        Some(secs) => Err(ApiError::bad_request(format!(
            "通知のタイミング {secs} 秒は選べません"
        ))),
    }
}

fn parse_hhmm(input: &str, name: &str) -> Result<String, ApiError> {
    parse_time(input)
        .map(|t| t.format("%H:%M").to_string())
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "{name}「{input}」を解釈できませんでした（例: 21:00）"
            ))
        })
}

#[derive(Deserialize)]
struct SettingsInput {
    /// 空文字で未設定に戻す
    channel_id: Option<String>,
    /// 0 でオフ
    remind_before: Option<i64>,
    /// "off" でオフ
    summary_time: Option<String>,
    default_time: Option<String>,
}

/// サーバーの設定を変更する（Discord の /hw settings と同じく誰でも変更できる）
async fn update_settings(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Json(input): Json<SettingsInput>,
) -> ApiResult<SettingsDto> {
    auth.guild(&state, &gid)?;
    let channel_id = match input.channel_id.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(id) => {
            let channels = state.discord.text_channels(&gid).await;
            if !channels.iter().any(|c| c.id == id) {
                return Err(ApiError::bad_request(
                    "そのチャンネルには投稿できません（一覧から選んでください）",
                ));
            }
            Some(Some(id.to_string()))
        }
    };
    let remind_before = match input.remind_before {
        None => None,
        Some(v) => Some(parse_remind(Some(v))?),
    };
    let summary_time = match input.summary_time.as_deref().map(str::trim) {
        None => None,
        Some("off" | "") => Some(None),
        Some(t) => Some(Some(parse_hhmm(t, "まとめ投稿の時刻")?)),
    };
    let default_time = match input.default_time.as_deref() {
        None => None,
        Some(t) => Some(parse_hhmm(t, "時刻")?),
    };
    state
        .db
        .hw_update_settings(
            gid.clone(),
            HwSettingsPatch {
                channel_id,
                remind_before,
                summary_time,
                default_time,
                notify_role_id: None,
            },
        )
        .await?;
    Ok(Json(settings_dto(&state, &gid, &auth.user_id).await?))
}

#[derive(Deserialize)]
struct NotifyInput {
    /// null / 0 でオフ
    remind_before: Option<i64>,
}

/// 自分への DM 通知（/hw notify と同じ）
async fn set_my_notify(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Json(input): Json<NotifyInput>,
) -> ApiResult<SettingsDto> {
    auth.guild(&state, &gid)?;
    let secs = parse_remind(input.remind_before)?;
    state
        .db
        .hw_set_dm(gid.clone(), auth.user_id.clone(), secs)
        .await?;
    Ok(Json(settings_dto(&state, &gid, &auth.user_id).await?))
}

// ────────────────────────────────────────────────────────────────────────────
// 毎週の宿題
// ────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RepeatDto {
    id: i64,
    subject: String,
    title: String,
    memo: Option<String>,
    /// 0 = 月曜 〜 6 = 日曜
    weekday: u32,
    time: String,
    created_by: String,
    can_delete: bool,
    /// 最後に自動登録した宿題の期限
    last_due: Option<String>,
}

fn repeat_dto(r: HwRepeat, auth: &Auth, guild: &WebGuild) -> RepeatDto {
    RepeatDto {
        can_delete: r.created_by == auth.user_id || guild.can_manage,
        id: r.id,
        subject: r.subject,
        title: r.title,
        memo: r.description,
        weekday: r.weekday,
        time: r.time,
        created_by: r.created_by,
        last_due: r.last_due,
    }
}

async fn list_repeats(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
) -> ApiResult<Vec<RepeatDto>> {
    let guild = auth.guild(&state, &gid)?.clone();
    let list = state.db.hw_repeats(Some(gid)).await?;
    Ok(Json(
        list.into_iter()
            .map(|r| repeat_dto(r, &auth, &guild))
            .collect(),
    ))
}

#[derive(Deserialize)]
struct RepeatInput {
    subject: Option<String>,
    title: Option<String>,
    weekday: Option<u32>,
    /// 省略時はサーバーの「時刻を省略したときの期限」
    time: Option<String>,
    memo: Option<String>,
}

/// 毎週の宿題を登録し、今回の分もすぐに登録する（/hw repeat add と同じ）
async fn create_repeat(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Json(input): Json<RepeatInput>,
) -> Result<(StatusCode, Json<RepeatDto>), ApiError> {
    let guild = auth.guild(&state, &gid)?.clone();
    let subject = required(input.subject.as_deref().unwrap_or(""), "科目", 30)?;
    let title = required(input.title.as_deref().unwrap_or(""), "内容", 100)?;
    let memo = optional(input.memo, "メモ", 1000)?;
    let weekday = input
        .weekday
        .filter(|w| *w < 7)
        .ok_or_else(|| ApiError::bad_request("曜日を選んでください"))?;
    let time = match input
        .time
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => parse_hhmm(t, "時刻")?,
        None => state.db.hw_settings(gid.clone()).await?.default_time,
    };
    let channel = state.db.announce_channel(gid.clone()).await?;

    let repeat = HwRepeat {
        id: 0,
        guild_id: gid.clone(),
        channel_id: channel.unwrap_or_else(|| "0".to_string()),
        created_by: auth.user_id.clone(),
        subject,
        title,
        description: memo,
        weekday,
        time: time.clone(),
        last_due: None,
    };
    let id = state.db.hw_repeat_add(repeat).await?;
    if let Some(t) = parse_time(&time) {
        let first = next_weekly(local_now(), weekday, t)
            .format(DUE_FORMAT)
            .to_string();
        state.db.hw_repeat_generate(id, first).await?;
    }
    let created = state
        .db
        .hw_repeat_get(id, gid)
        .await?
        .ok_or_else(|| ApiError::not_found("登録した毎週の宿題が見つかりません"))?;
    Ok((
        StatusCode::CREATED,
        Json(repeat_dto(created, &auth, &guild)),
    ))
}

/// 毎週の宿題を止める（登録済みの宿題は残る。登録した人か管理権限がある人のみ）
async fn remove_repeat(
    State(state): State<AppState>,
    auth: Auth,
    Path((gid, id)): Path<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    let guild = auth.guild(&state, &gid)?.clone();
    let r = state
        .db
        .hw_repeat_get(id, gid.clone())
        .await?
        .ok_or_else(|| ApiError::not_found(format!("毎週の宿題 #{id} が見つかりません")))?;
    if r.created_by != auth.user_id && !guild.can_manage {
        return Err(ApiError::forbidden(
            "止められるのは登録した人か、メッセージの管理権限がある人だけです",
        ));
    }
    state.db.hw_repeat_delete(id, gid).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ────────────────────────────────────────────────────────────────────────────
// タスク
// ────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct TaskDto {
    id: i64,
    title: String,
    description: Option<String>,
    status: &'static str,
    priority: &'static str,
    due: Option<String>,
    due_unix: Option<i64>,
    assignees: Vec<String>,
    created_by: String,
    has_event: bool,
}

fn task_dto(t: Task) -> TaskDto {
    TaskDto {
        id: t.id,
        status: t.status.as_str(),
        priority: t.priority.as_str(),
        due_unix: t.due_date.as_deref().map(due_unix),
        has_event: t.discord_event_id.is_some(),
        title: t.title,
        description: t.description,
        due: t.due_date,
        assignees: t.assignees,
        created_by: t.user_id,
    }
}

#[derive(Deserialize)]
struct TaskListQuery {
    /// open（未完了）/ all / range（from〜until に期限があるもの）
    scope: Option<String>,
    from: Option<String>,
    until: Option<String>,
}

async fn list_tasks(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Query(q): Query<TaskListQuery>,
) -> ApiResult<Vec<TaskDto>> {
    auth.guild(&state, &gid)?;
    let mut tasks = state
        .db
        .list_tasks(
            gid,
            TaskQuery {
                sort: TaskSort::DueDate,
                ..Default::default()
            },
        )
        .await?;
    match q.scope.as_deref().unwrap_or("open") {
        "open" => tasks.retain(|t| t.status != TaskStatus::Done),
        "all" => {}
        "range" => tasks.retain(|t| {
            t.due_date.as_deref().is_some_and(|d| {
                q.from.as_deref().is_none_or(|f| d >= f) && q.until.as_deref().is_none_or(|u| d < u)
            })
        }),
        s => return Err(ApiError::bad_request(format!("scope `{s}` は不正です"))),
    }
    Ok(Json(tasks.into_iter().map(task_dto).collect()))
}

#[derive(Deserialize)]
struct TaskInput {
    title: Option<String>,
    description: Option<String>,
    priority: Option<String>,
    due: Option<String>,
    status: Option<String>,
}

fn parse_priority(p: Option<&str>) -> Result<Option<Priority>, ApiError> {
    match p {
        None | Some("") => Ok(None),
        Some("Low") => Ok(Some(Priority::Low)),
        Some("Medium") => Ok(Some(Priority::Medium)),
        Some("High") => Ok(Some(Priority::High)),
        Some(p) => Err(ApiError::bad_request(format!("priority `{p}` は不正です"))),
    }
}

fn parse_status(s: Option<&str>) -> Result<Option<TaskStatus>, ApiError> {
    match s {
        None | Some("") => Ok(None),
        Some("Pending") => Ok(Some(TaskStatus::Pending)),
        Some("InProgress") => Ok(Some(TaskStatus::InProgress)),
        Some("Done") => Ok(Some(TaskStatus::Done)),
        Some(s) => Err(ApiError::bad_request(format!("status `{s}` は不正です"))),
    }
}

async fn create_task(
    State(state): State<AppState>,
    auth: Auth,
    Path(gid): Path<String>,
    Json(input): Json<TaskInput>,
) -> Result<(StatusCode, Json<TaskDto>), ApiError> {
    auth.guild(&state, &gid)?;
    let title = required(input.title.as_deref().unwrap_or(""), "タイトル", 100)?;
    let description = optional(input.description, "説明", 1000)?;
    let priority = parse_priority(input.priority.as_deref())?.unwrap_or(Priority::Medium);
    let due_date = match input
        .due
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(due) => {
            let settings = state.db.hw_settings(gid.clone()).await?;
            Some(parse_due(due, &settings)?)
        }
        None => None,
    };
    let channel = state.db.announce_channel(gid.clone()).await?;

    let id = state
        .db
        .add_task(
            auth.user_id.clone(),
            gid.clone(),
            channel.clone().unwrap_or_else(|| "0".to_string()),
            title,
            description,
            priority,
            due_date,
            vec![],
        )
        .await?;
    let task = state
        .db
        .get_task(id, gid)
        .await?
        .ok_or_else(|| ApiError::not_found("登録したタスクが見つかりません"))?;

    let due = task
        .due_date
        .as_deref()
        .map(|d| format!(" — 期限 <t:{}:f>", due_unix(d)))
        .unwrap_or_default();
    let msg = CreateMessage::new().content(format!(
        "📋 <@{}> が Web からタスクを登録しました：`#{}` {} {}{}",
        auth.user_id,
        task.id,
        task.priority.emoji(),
        task.title,
        due
    ));
    announce(&state, channel.as_deref(), msg, None).await;

    Ok((StatusCode::CREATED, Json(task_dto(task))))
}

async fn edit_task(
    State(state): State<AppState>,
    auth: Auth,
    Path((gid, id)): Path<(String, i64)>,
    Json(input): Json<TaskInput>,
) -> ApiResult<TaskDto> {
    auth.guild(&state, &gid)?;
    let status = parse_status(input.status.as_deref())?;
    let due_date = match input
        .due
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(due) => {
            let settings = state.db.hw_settings(gid.clone()).await?;
            Some(parse_due(due, &settings)?)
        }
        None => None,
    };
    let edit = TaskEdit {
        title: optional(input.title, "タイトル", 100)?,
        description: optional(input.description, "説明", 1000)?,
        priority: parse_priority(input.priority.as_deref())?,
        due_date,
        reminders: None,
    };
    let event_fields_changed =
        edit.title.is_some() || edit.description.is_some() || edit.due_date.is_some();

    let not_found = || ApiError::not_found(format!("タスク #{id} が見つかりません"));
    let mut task = if event_fields_changed || edit.priority.is_some() {
        state
            .db
            .edit_task(id, gid.clone(), edit)
            .await?
            .ok_or_else(not_found)?
    } else {
        state
            .db
            .get_task(id, gid.clone())
            .await?
            .ok_or_else(not_found)?
    };
    if let Some(status) = status {
        state.db.update_task_status(id, gid.clone(), status).await?;
        task = state.db.get_task(id, gid).await?.ok_or_else(not_found)?;
    }
    if event_fields_changed {
        sync_event(&*state.http, &task).await;
    }
    Ok(Json(task_dto(task)))
}

async fn remove_task(
    State(state): State<AppState>,
    auth: Auth,
    Path((gid, id)): Path<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    auth.guild(&state, &gid)?;
    let task = state
        .db
        .delete_task(id, gid)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("タスク #{id} が見つかりません")))?;
    delete_event(&*state.http, &task).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_text_inputs() {
        assert!(required("  ", "科目", 30).is_err());
        assert_eq!(required(" 数学 ", "科目", 30).unwrap(), "数学");
        assert!(required(&"あ".repeat(31), "科目", 30).is_err());
        assert_eq!(optional(Some("  ".into()), "メモ", 10).unwrap(), None);
    }

    #[test]
    fn parses_due_from_picker_and_words() {
        let s = HwSettings::default_for("G".into());
        assert_eq!(
            parse_due("2999-12-05T13:00", &s).unwrap(),
            "2999-12-05 13:00"
        );
        assert_eq!(parse_due("2999-12-05", &s).unwrap(), "2999-12-05 08:30");
        assert!(parse_due("いつか", &s).is_err());
    }
}
