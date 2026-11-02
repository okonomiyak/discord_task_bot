//! Web API を router 経由で叩くテスト（Discord への通信は発生しない）

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use poise::serenity_prelude::Http;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{AppState, BotGuilds, WebConfig, WebGuild, router};
use crate::db::{Database, WebSession};

const GUILD: &str = "123";

fn test_state() -> AppState {
    let db = Database::new(":memory:").unwrap();
    db.init().unwrap();
    AppState {
        db,
        http: Arc::new(Http::new("")),
        bot_guilds: BotGuilds::Fixed(vec![123, 456]),
        config: Arc::new(WebConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            base_url: "http://localhost".into(),
            client_id: "id".into(),
            client_secret: "secret".into(),
        }),
        client: reqwest::Client::new(),
    }
}

/// セッションを直接作って Cookie ヘッダーの値を返す
async fn login_as(state: &AppState, user_id: &str, can_manage: bool) -> String {
    let sid = format!("sid-{user_id}");
    let guilds = vec![
        WebGuild {
            id: GUILD.into(),
            name: "クラス".into(),
            icon: None,
            can_manage,
        },
        // ボットがいないサーバー（アクセスできない）
        WebGuild {
            id: "999".into(),
            name: "別".into(),
            icon: None,
            can_manage: true,
        },
    ];
    state
        .db
        .web_session_create(WebSession {
            id: sid.clone(),
            user_id: user_id.into(),
            username: format!("user{user_id}"),
            avatar: None,
            guilds_json: serde_json::to_string(&guilds).unwrap(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
        })
        .await
        .unwrap();
    format!("sid={sid}")
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn serves_the_page_and_requires_login_for_api() {
    let state = test_state();
    let req = Request::get("/").body(Body::empty()).unwrap();
    let res = router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let (status, body) = call(&state, "GET", "/api/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "ログインしてください");

    let (status, _) = call(&state, "GET", "/api/me", Some("sid=nope"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_redirects_to_discord_with_state_cookie() {
    let state = test_state();
    let res = router(state)
        .oneshot(Request::get("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(res.status().is_redirection());
    let location = res.headers()[header::LOCATION].to_str().unwrap();
    assert!(location.starts_with("https://discord.com/oauth2/authorize?"));
    assert!(location.contains("redirect_uri=http%3A%2F%2Flocalhost%2Fauth%2Fcallback"));
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("oauth_state=") && cookie.contains("HttpOnly"));
}

#[tokio::test]
async fn callback_rejects_mismatched_state() {
    let state = test_state();
    let req = Request::get("/auth/callback?code=x&state=abc")
        .header(header::COOKIE, "oauth_state=other")
        .body(Body::empty())
        .unwrap();
    let res = router(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn me_lists_only_guilds_with_the_bot() {
    let state = test_state();
    let cookie = login_as(&state, "1", false).await;
    let (status, me) = call(&state, "GET", "/api/me", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["name"], "user1");
    let guilds = me["guilds"].as_array().unwrap();
    assert_eq!(guilds.len(), 1);
    assert_eq!(guilds[0]["id"], GUILD);

    let (status, _) = call(
        &state,
        "GET",
        "/api/guilds/999/homework",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // ボットはいるがログインしたユーザーが入っていないサーバー
    let (status, _) = call(
        &state,
        "GET",
        "/api/guilds/456/homework",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn homework_flow_add_done_edit_delete() {
    let state = test_state();
    let alice = login_as(&state, "1", false).await;
    let bob = login_as(&state, "2", false).await;
    let base = format!("/api/guilds/{GUILD}/homework");

    // 期限の書式が不正
    let (status, err) = call(
        &state,
        "POST",
        &base,
        Some(&alice),
        Some(json!({ "subject": "数学", "title": "p.32", "due": "そのうち" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("そのうち"));

    // 科目なし
    let (status, _) = call(
        &state,
        "POST",
        &base,
        Some(&alice),
        Some(json!({ "subject": " ", "title": "p.32", "due": "明日" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 登録（日時ピッカー形式）
    let (status, hw) = call(
        &state,
        "POST",
        &base,
        Some(&alice),
        Some(json!({ "subject": "数学", "title": "p.32", "due": "2999-01-05T08:30", "memo": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(hw["due"], "2999-01-05 08:30");
    assert_eq!(hw["memo"], Value::Null);
    assert_eq!(hw["can_delete"], true);
    let id = hw["id"].as_i64().unwrap();

    // 二重送信は登録されない
    let (status, again) = call(
        &state,
        "POST",
        &base,
        Some(&alice),
        Some(json!({ "subject": "数学", "title": "p.32", "due": "2999-01-05 08:30" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["id"], id);

    // テストも登録
    let (status, exam) = call(
        &state,
        "POST",
        &base,
        Some(&bob),
        Some(json!({ "kind": "exam", "subject": "英語", "title": "単語", "due": "2999-01-06" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(exam["kind"], "exam");
    let exam_id = exam["id"].as_i64().unwrap();

    // やること（未完了の宿題のみ）
    let (_, todo) = call(
        &state,
        "GET",
        &format!("{base}?scope=todo"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(todo.as_array().unwrap().len(), 1);

    // 完了にすると、自分のやることからだけ消える
    let (status, done) = call(
        &state,
        "POST",
        &format!("{base}/{id}/done"),
        Some(&alice),
        Some(json!({ "done": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(done["done_by_me"], true);
    assert_eq!(done["done_count"], 1);
    let (_, todo) = call(
        &state,
        "GET",
        &format!("{base}?scope=todo"),
        Some(&alice),
        None,
    )
    .await;
    assert!(todo.as_array().unwrap().is_empty());
    let (_, todo_b) = call(
        &state,
        "GET",
        &format!("{base}?scope=todo"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(todo_b.as_array().unwrap().len(), 1);
    assert_eq!(todo_b[0]["done_by_me"], false);

    // テストは完了にできない
    let (status, _) = call(
        &state,
        "POST",
        &format!("{base}/{exam_id}/done"),
        Some(&alice),
        Some(json!({ "done": true })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 種類での絞り込み・期間指定
    let (_, exams) = call(
        &state,
        "GET",
        &format!("{base}?scope=upcoming&kind=exam"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(exams.as_array().unwrap().len(), 1);
    let (_, range) = call(
        &state,
        "GET",
        &format!("{base}?scope=range&from=2999-01-06&until=2999-01-07"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(range.as_array().unwrap().len(), 1);
    assert_eq!(range[0]["id"], exam_id);

    // 編集（期限は日付だけなら既定の時刻）
    let (status, edited) = call(
        &state,
        "PATCH",
        &format!("{base}/{id}"),
        Some(&bob),
        Some(json!({ "title": "p.32〜35", "due": "2999-01-07" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edited["title"], "p.32〜35");
    assert_eq!(edited["due"], "2999-01-07 08:30");
    assert_eq!(edited["subject"], "数学");
    assert_eq!(edited["can_delete"], false);

    // 他人の宿題は削除できない（管理権限があれば可）
    let (status, _) = call(&state, "DELETE", &format!("{base}/{id}"), Some(&bob), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let admin = login_as(&state, "3", true).await;
    let (status, _) = call(
        &state,
        "DELETE",
        &format!("{base}/{id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &state,
        "DELETE",
        &format!("{base}/{id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, subjects) = call(
        &state,
        "GET",
        &format!("/api/guilds/{GUILD}/subjects"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(subjects, json!(["英語"]));
}

#[tokio::test]
async fn stats_reports_personal_rate() {
    let state = test_state();
    let alice = login_as(&state, "1", false).await;
    let base = format!("/api/guilds/{GUILD}/homework");
    for (subject, due) in [
        ("数学", "2999-01-05"),
        ("数学", "2999-01-06"),
        ("英語", "2999-01-07"),
    ] {
        call(
            &state,
            "POST",
            &base,
            Some(&alice),
            Some(json!({ "subject": subject, "title": due, "due": due })),
        )
        .await;
    }
    call(
        &state,
        "POST",
        &base,
        Some(&alice),
        Some(json!({ "kind": "exam", "subject": "英語", "title": "t", "due": "2999-01-08" })),
    )
    .await;
    call(
        &state,
        "POST",
        &format!("{base}/1/done"),
        Some(&alice),
        Some(json!({ "done": true })),
    )
    .await;

    let (status, s) = call(
        &state,
        "GET",
        &format!("/api/guilds/{GUILD}/stats?period=all"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["label"], "これまでの全期間");
    assert_eq!(s["overall"], json!({ "done": 1, "total": 3 }));
    assert_eq!(s["upcoming"], json!({ "done": 1, "total": 3 }));
    assert_eq!(
        s["by_subject"][0],
        json!({ "subject": "数学", "done": 1, "total": 2 })
    );

    // 他の人から見ると 0%
    let bob = login_as(&state, "2", false).await;
    let (_, s) = call(
        &state,
        "GET",
        &format!("/api/guilds/{GUILD}/stats?period=all"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(s["overall"], json!({ "done": 0, "total": 3 }));

    let (status, _) = call(
        &state,
        "GET",
        &format!("/api/guilds/{GUILD}/stats?period=year"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&state, "GET", "/api/guilds/999/stats", Some(&alice), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn task_flow_add_status_edit_delete() {
    let state = test_state();
    let cookie = login_as(&state, "1", false).await;
    let base = format!("/api/guilds/{GUILD}/tasks");

    let (status, task) = call(
        &state,
        "POST",
        &base,
        Some(&cookie),
        Some(json!({ "title": "申請書", "priority": "High", "due": "2999-02-01 17:00" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(task["status"], "Pending");
    assert_eq!(task["priority"], "High");
    let id = task["id"].as_i64().unwrap();

    // 期限なしのタスク
    let (status, no_due) = call(
        &state,
        "POST",
        &base,
        Some(&cookie),
        Some(json!({ "title": "掃除" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(no_due["due"], Value::Null);

    let (status, _) = call(
        &state,
        "POST",
        &base,
        Some(&cookie),
        Some(json!({ "title": "x", "priority": "Huge" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, done) = call(
        &state,
        "PATCH",
        &format!("{base}/{id}"),
        Some(&cookie),
        Some(json!({ "status": "Done" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(done["status"], "Done");

    let (_, open) = call(
        &state,
        "GET",
        &format!("{base}?scope=open"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(open.as_array().unwrap().len(), 1);
    let (_, all) = call(
        &state,
        "GET",
        &format!("{base}?scope=all"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let (_, range) = call(
        &state,
        "GET",
        &format!("{base}?scope=range&from=2999-02-01&until=2999-02-02"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(range.as_array().unwrap().len(), 1);

    let (status, edited) = call(
        &state,
        "PATCH",
        &format!("{base}/{id}"),
        Some(&cookie),
        Some(json!({ "title": "申請書を出す", "priority": "Low", "status": "InProgress" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edited["title"], "申請書を出す");
    assert_eq!(edited["priority"], "Low");
    assert_eq!(edited["status"], "InProgress");

    let (status, _) = call(
        &state,
        "DELETE",
        &format!("{base}/{id}"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &state,
        "PATCH",
        &format!("{base}/{id}"),
        Some(&cookie),
        Some(json!({ "status": "Done" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn logout_deletes_session() {
    let state = test_state();
    let cookie = login_as(&state, "1", false).await;
    let req = Request::post("/logout")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let (status, _) = call(&state, "GET", "/api/me", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// 画面確認用のデモサーバー（`cargo test serve_demo -- --ignored` で起動、Cookie `sid=sid-1`）
#[tokio::test]
#[ignore]
async fn serve_demo() {
    let state = test_state();
    let cookie = login_as(&state, "1", true).await;
    login_as(&state, "2", false).await;
    let hw = format!("/api/guilds/{GUILD}/homework");
    let tasks = format!("/api/guilds/{GUILD}/tasks");
    let local = crate::time::local_now();
    let day = |n: i64, t: &str| {
        format!(
            "{} {t}",
            (local + chrono::Duration::days(n)).format("%Y-%m-%d")
        )
    };
    for (kind, subject, title, due, memo) in [
        (
            "homework",
            "数学",
            "問題集 p.32〜35",
            day(0, "23:59"),
            Some("解答は自分で丸つけ"),
        ),
        (
            "homework",
            "英語",
            "単語 Unit 5 を覚える",
            day(1, "08:30"),
            None,
        ),
        (
            "homework",
            "国語",
            "読書感想文",
            day(5, "08:30"),
            Some("原稿用紙3枚"),
        ),
        ("homework", "理科", "実験レポート", day(-1, "17:00"), None),
        (
            "exam",
            "数学",
            "小テスト 二次関数",
            day(3, "10:40"),
            Some("範囲: p.20〜40"),
        ),
    ] {
        call(&state, "POST", &hw, Some(&cookie),
            Some(json!({ "kind": kind, "subject": subject, "title": title, "due": due, "memo": memo }))).await;
    }
    let bob = "sid=sid-2";
    call(
        &state,
        "POST",
        &format!("{hw}/2/done"),
        Some(bob),
        Some(json!({ "done": true })),
    )
    .await;
    call(
        &state,
        "POST",
        &format!("{hw}/1/done"),
        Some(bob),
        Some(json!({ "done": true })),
    )
    .await;
    call(
        &state,
        "POST",
        &tasks,
        Some(&cookie),
        Some(json!({ "title": "文化祭の申請書", "priority": "High", "due": day(2, "17:00") })),
    )
    .await;
    call(
        &state,
        "POST",
        &tasks,
        Some(&cookie),
        Some(json!({ "title": "部室の掃除", "priority": "Low" })),
    )
    .await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:18080")
        .await
        .unwrap();
    println!("demo: http://127.0.0.1:18080 （Cookie {cookie}）");
    axum::serve(listener, router(state)).await.unwrap();
}
