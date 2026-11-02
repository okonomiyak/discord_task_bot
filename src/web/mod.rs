//! Web ダッシュボード（Discord でログインして宿題・タスクを見る・完了にする・編集する）

mod api;
#[cfg(test)]
mod api_tests;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use poise::serenity_prelude::{Cache, GuildId, Http};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::db::{Database, WebSession};

const DISCORD_API: &str = "https://discord.com/api/v10";
const SESSION_COOKIE: &str = "sid";
const STATE_COOKIE: &str = "oauth_state";
/// ログインの有効期間（日）
const SESSION_DAYS: i64 = 7;

/// Discord の権限ビット
const PERM_ADMINISTRATOR: u64 = 1 << 3;
const PERM_MANAGE_MESSAGES: u64 = 1 << 13;

/// Web ダッシュボードの設定（環境変数から読む）
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// 待ち受けるアドレス（例: 0.0.0.0:8080）
    pub addr: SocketAddr,
    /// 外から見た URL（例: https://hw.example.com）。OAuth のリダイレクト先に使う
    pub base_url: String,
    pub client_id: String,
    pub client_secret: String,
}

impl WebConfig {
    /// DISCORD_CLIENT_ID・DISCORD_CLIENT_SECRET・WEB_BASE_URL がそろっていれば Some
    pub fn from_env() -> Option<Result<Self, String>> {
        let client_id = std::env::var("DISCORD_CLIENT_ID").ok()?;
        let client_secret = std::env::var("DISCORD_CLIENT_SECRET").ok()?;
        let base_url = match std::env::var("WEB_BASE_URL") {
            Ok(url) => url.trim_end_matches('/').to_string(),
            Err(_) => {
                return Some(Err(
                    "WEB_BASE_URL が設定されていません（例: https://hw.example.com）".into(),
                ));
            }
        };
        let addr = std::env::var("WEB_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
        Some(
            addr.parse()
                .map_err(|e| format!("WEB_ADDR `{addr}` が無効です: {e}"))
                .map(|addr| Self {
                    addr,
                    base_url,
                    client_id,
                    client_secret,
                }),
        )
    }

    fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.base_url)
    }

    fn secure_cookie(&self) -> bool {
        self.base_url.starts_with("https://")
    }
}

/// ボットが入っているサーバーの調べ方（テストでは固定のリストを使う）
#[derive(Clone)]
enum BotGuilds {
    Cache(Arc<Cache>),
    #[cfg(test)]
    Fixed(Vec<u64>),
}

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub http: Arc<Http>,
    bot_guilds: BotGuilds,
    pub config: Arc<WebConfig>,
    client: reqwest::Client,
}

impl AppState {
    pub fn new(db: Database, http: Arc<Http>, cache: Arc<Cache>, config: WebConfig) -> Self {
        Self {
            db,
            http,
            bot_guilds: BotGuilds::Cache(cache),
            config: Arc::new(config),
            client: reqwest::Client::new(),
        }
    }

    /// ボットがそのサーバーに入っているか
    fn bot_in_guild(&self, guild_id: &str) -> bool {
        guild_id
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .is_some_and(|id| match &self.bot_guilds {
                BotGuilds::Cache(cache) => cache.guilds().contains(&GuildId::new(id)),
                #[cfg(test)]
                BotGuilds::Fixed(ids) => ids.contains(&id),
            })
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/logout", post(logout))
        .nest("/api", api::router())
        .with_state(state)
}

pub async fn serve(state: AppState) {
    let addr = state.config.addr;
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Web サーバーを {addr} で起動できませんでした: {e}");
            return;
        }
    };
    println!(
        "Web ダッシュボード: {} （{addr} で待ち受け）",
        state.config.base_url
    );
    if let Err(e) = axum::serve(listener, router(state)).await {
        eprintln!("Web サーバーエラー: {e}");
    }
}

// ────────────────────────────────────────────────────────────────────────────
// エラー・認証
// ────────────────────────────────────────────────────────────────────────────

/// API のエラー（JSON で `{ "error": "..." }` を返す）
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self(StatusCode::FORBIDDEN, msg.into())
    }
    fn unauthorized() -> Self {
        Self(StatusCode::UNAUTHORIZED, "ログインしてください".into())
    }
}

impl From<crate::Error> for ApiError {
    fn from(e: crate::Error) -> Self {
        eprintln!("Web API エラー: {e:?}");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "サーバーでエラーが発生しました".into(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

/// ログイン時に保存するサーバー情報
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebGuild {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// メッセージの管理権限（他人の宿題も削除できる）
    pub can_manage: bool,
}

/// ログイン中のユーザー（API ハンドラの引数にすると未ログインは 401 になる）
pub struct Auth {
    pub user_id: String,
    pub username: String,
    pub avatar: Option<String>,
    pub guilds: Vec<WebGuild>,
}

impl Auth {
    /// アクセスできるサーバーか確認する（ログイン時に入っていて、今もボットがいる）
    pub fn guild(&self, state: &AppState, guild_id: &str) -> Result<&WebGuild, ApiError> {
        self.guilds
            .iter()
            .find(|g| g.id == guild_id)
            .filter(|g| state.bot_in_guild(&g.id))
            .ok_or_else(|| ApiError::forbidden("このサーバーにはアクセスできません"))
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let sid = cookie(&parts.headers, SESSION_COOKIE).ok_or_else(ApiError::unauthorized)?;
        let session = state
            .db
            .web_session_get(sid)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        Ok(Self {
            user_id: session.user_id,
            username: session.username,
            avatar: session.avatar,
            guilds: serde_json::from_str(&session.guilds_json).unwrap_or_default(),
        })
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

fn set_cookie(config: &WebConfig, name: &str, value: &str, max_age_secs: i64) -> HeaderValue {
    let secure = if config.secure_cookie() {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
    ))
    .expect("cookie はASCIIのみ")
}

fn random_token() -> String {
    let bytes: [u8; 32] = rand::thread_rng().r#gen();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ────────────────────────────────────────────────────────────────────────────
// ページ・ログイン
// ────────────────────────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn login(State(state): State<AppState>) -> Response {
    let oauth_state = random_token();
    let url = reqwest::Url::parse_with_params(
        "https://discord.com/oauth2/authorize",
        &[
            ("client_id", state.config.client_id.as_str()),
            ("redirect_uri", state.config.redirect_uri().as_str()),
            ("response_type", "code"),
            ("scope", "identify guilds"),
            ("state", oauth_state.as_str()),
            ("prompt", "none"),
        ],
    )
    .expect("固定の URL");

    let mut res = Redirect::to(url.as_str()).into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state.config, STATE_COOKIE, &oauth_state, 600),
    );
    res
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

#[derive(Deserialize)]
struct DiscordGuild {
    id: String,
    name: String,
    icon: Option<String>,
    #[serde(default)]
    owner: bool,
    #[serde(default)]
    permissions: Option<String>,
}

fn login_error(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'>\
             <p>ログインできませんでした: {msg}</p><p><a href='/'>トップに戻る</a></p>"
        )),
    )
        .into_response()
}

async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    if q.error.is_some() {
        return Redirect::to("/").into_response();
    }
    let (Some(code), Some(got_state)) = (q.code, q.state) else {
        return login_error("パラメータが足りません");
    };
    // CSRF 対策: /login で発行した state と一致するか
    if cookie(&headers, STATE_COOKIE).as_deref() != Some(got_state.as_str()) {
        return login_error("ログインの有効期限が切れました。もう一度お試しください");
    }

    match finish_login(&state, &code).await {
        Ok(session_id) => {
            let mut res = Redirect::to("/").into_response();
            let h = res.headers_mut();
            h.append(
                header::SET_COOKIE,
                set_cookie(
                    &state.config,
                    SESSION_COOKIE,
                    &session_id,
                    SESSION_DAYS * 86400,
                ),
            );
            h.append(
                header::SET_COOKIE,
                set_cookie(&state.config, STATE_COOKIE, "", 0),
            );
            res
        }
        Err(e) => {
            eprintln!("Web ログインエラー: {e}");
            login_error("Discord との通信に失敗しました")
        }
    }
}

/// 認可コードをトークンに交換し、ユーザーとサーバーを取得してセッションを作る
async fn finish_login(state: &AppState, code: &str) -> Result<String, crate::Error> {
    let token: TokenResponse = state
        .client
        .post(format!("{DISCORD_API}/oauth2/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", state.config.redirect_uri().as_str()),
            ("client_id", state.config.client_id.as_str()),
            ("client_secret", state.config.client_secret.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let get = |path: &str| {
        state
            .client
            .get(format!("{DISCORD_API}{path}"))
            .bearer_auth(&token.access_token)
            .send()
    };
    let user: DiscordUser = get("/users/@me").await?.error_for_status()?.json().await?;
    let guilds: Vec<DiscordGuild> = get("/users/@me/guilds")
        .await?
        .error_for_status()?
        .json()
        .await?;

    let guilds: Vec<WebGuild> = guilds
        .into_iter()
        .filter(|g| state.bot_in_guild(&g.id))
        .map(|g| {
            let perms: u64 = g
                .permissions
                .as_deref()
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
            WebGuild {
                id: g.id,
                name: g.name,
                icon: g.icon,
                can_manage: g.owner || perms & (PERM_ADMINISTRATOR | PERM_MANAGE_MESSAGES) != 0,
            }
        })
        .collect();

    let session_id = random_token();
    state
        .db
        .web_session_create(WebSession {
            id: session_id.clone(),
            user_id: user.id,
            username: user.global_name.unwrap_or(user.username),
            avatar: user.avatar,
            guilds_json: serde_json::to_string(&guilds)?,
            expires_at: chrono::Utc::now().timestamp() + SESSION_DAYS * 86400,
        })
        .await?;
    Ok(session_id)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(sid) = cookie(&headers, SESSION_COOKIE) {
        let _ = state.db.web_session_delete(sid).await;
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state.config, SESSION_COOKIE, "", 0),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cookie_by_name() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; sid=abc; b=2"),
        );
        assert_eq!(cookie(&headers, "sid").as_deref(), Some("abc"));
        assert_eq!(cookie(&headers, "x"), None);
    }

    #[test]
    fn random_tokens_are_long_and_unique() {
        let a = random_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_token());
    }
}
