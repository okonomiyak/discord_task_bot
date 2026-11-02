use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use poise::serenity_prelude::{ChannelId, CreateAllowedMentions, CreateMessage};
use serde::{Deserialize, Serialize};

use super::{ApiError, AppState, Auth, WebGuild};
use crate::{
    commands::{delete_event, done_buttons, sync_event},
    db::{HwEdit, TaskEdit},
    models::{
        Homework, HwKind, HwQuery, HwSettings, NewHomework, Priority, Task, TaskQuery, TaskSort,
        TaskStatus,
    },
    time::{DUE_FORMAT, due_to_utc, local_now, parse_due_input, parse_time},
};

type ApiResult<T> = Result<Json<T>, ApiError>;

/// `/hw todo` と同じく、期限切れでも7日以内なら「やること」に表示する
const TODO_OVERDUE_DAYS: i64 = 7;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/guilds/{gid}/subjects", get(subjects))
        .route(
            "/guilds/{gid}/homework",
            get(list_homework).post(create_homework),
        )
        .route(
            "/guilds/{gid}/homework/{id}",
            patch(edit_homework).delete(delete_homework),
        )
        .route("/guilds/{gid}/homework/{id}/done", post(set_done))
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
async fn announce(state: &AppState, channel: Option<&str>, msg: CreateMessage) {
    let Some(id) = channel
        .and_then(|c| c.parse::<u64>().ok())
        .filter(|id| *id > 0)
    else {
        return;
    };
    let msg = msg.allowed_mentions(CreateAllowedMentions::new());
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
    done_by_me: bool,
    created_by: String,
    repeat: bool,
    can_delete: bool,
}

fn hw_dto(hw: Homework, auth: &Auth, guild: &WebGuild) -> HwDto {
    HwDto {
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
    let list = state.db.hw_list(gid, query).await?;
    Ok(Json(
        list.into_iter()
            .map(|hw| hw_dto(hw, &auth, &guild))
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
            .content(content)
            .components(done_buttons(std::slice::from_ref(&hw), false));
        announce(&state, channel.as_deref(), msg).await;
    }

    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(hw_dto(hw, &auth, &guild))))
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
    Ok(Json(hw_dto(hw, &auth, &guild)))
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
    Ok(Json(hw_dto(hw, &auth, &guild)))
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
    announce(&state, channel.as_deref(), msg).await;

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
