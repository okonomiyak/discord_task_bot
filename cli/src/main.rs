//! hw: 宿題ボードを操作する CLI
//!
//! Web ダッシュボードの API を、設定画面で発行した API トークンで呼び出す。
//! 人が打っても、Claude Code などの AI に実行させても使えるように、
//! `--json` を付けると API の結果をそのまま JSON で出す。

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, String>;

const ABOUT: &str = "宿題ボード（Discord の宿題ボット）を操作する CLI";
const LONG_ABOUT: &str = "宿題ボード（Discord の宿題ボット）を操作する CLI

はじめに、Web の設定画面の「API トークン」でトークンを発行して、ログインしてください:
  hw login --url https://hw.example.com

例:
  hw todo                                    自分がまだ終わっていない宿題
  hw add 数学 \"ワーク p.10-15\" 明日          宿題を登録
  hw add 英語 \"単語テスト Unit5\" \"金曜 10:40\" --exam
  hw done 12                                 #12 を完了にする
  hw list --json                             AI やスクリプト向けに JSON で出す

期限の書き方: 明日 / 明後日 17:00 / 明日17時 / 金曜 / 12/5 / 12/5 13:00 / 2025-12-05
（時刻を省略するとサーバーの設定の時刻）

設定ファイルの代わりに環境変数 HW_URL・HW_TOKEN・HW_GUILD も使えます。";

#[derive(Parser)]
#[command(name = "hw", version, about = ABOUT, long_about = LONG_ABOUT)]
struct Cli {
    /// 結果を JSON で出す（AI やスクリプト向け）
    #[arg(long, global = true)]
    json: bool,
    /// 操作するサーバー（ID か名前の一部。省略時は hw use で選んだもの）
    #[arg(long, short = 'g', global = true)]
    guild: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// ログインする（トークンは Web の設定画面の「API トークン」で発行）
    Login {
        /// 宿題ボードの URL（例: https://hw.example.com）
        #[arg(long)]
        url: Option<String>,
        /// API トークン（省略すると入力を求める。シェルの履歴に残したくないときは省略）
        #[arg(long)]
        token: Option<String>,
    },
    /// ログアウトする（設定ファイルを消す。トークン自体は Web の設定画面で削除）
    Logout,
    /// ログイン中のユーザーと、使えるサーバーを表示する
    Whoami,
    /// ふだん操作するサーバーを選ぶ
    Use {
        /// サーバーの ID か名前の一部
        guild: String,
    },
    /// 自分がまだ終わっていない宿題（最近の期限切れを含む）
    Todo,
    /// これからの宿題（--exam でテスト、--past で期限が過ぎたもの）
    List {
        /// テストを表示する
        #[arg(long)]
        exam: bool,
        /// 期限が過ぎたものを表示する
        #[arg(long)]
        past: bool,
    },
    /// 宿題を登録する（--exam でテスト）。Discord のチャンネルにも投稿される
    Add {
        /// 科目（例: 数学）
        subject: String,
        /// 内容（例: ワーク p.10-15）
        title: String,
        /// 期限（例: 明日 / 金曜 17:00 / 12/5）。テストなら日時
        due: String,
        /// メモ（範囲・提出方法・URL など）
        #[arg(long)]
        memo: Option<String>,
        /// テストとして登録する
        #[arg(long)]
        exam: bool,
    },
    /// 自分の分を完了にする（ID は複数指定できる）
    Done {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// 完了を取り消す
    Undo {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// 宿題・テストを編集する（指定した項目だけ変わる）
    Edit {
        id: i64,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        due: Option<String>,
        /// メモ（空文字 "" で消す）
        #[arg(long)]
        memo: Option<String>,
    },
    /// 宿題・テストを削除する（登録した人か管理者だけ。取り消せないので --yes が必要）
    Delete {
        id: i64,
        /// 削除してよいことの確認
        #[arg(long)]
        yes: bool,
    },
    /// 科目・内容・メモから探す（期限が過ぎたものも含む）
    Search { query: String },
}

// ────────────────────────────────────────────────────────────────────────────
// 設定
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Config {
    url: Option<String>,
    token: Option<String>,
    /// ふだん操作するサーバーの ID
    guild: Option<String>,
}

/// 設定ファイルの場所（HW_CONFIG > XDG_CONFIG_HOME > ~/.config > %APPDATA%）
fn config_path() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(p) = env("HW_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| env("APPDATA").map(PathBuf::from))?;
    Some(base.join("hw").join("config.json"))
}

fn load_config() -> Config {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 設定を保存する（トークンが入るので、Unix では本人だけが読めるようにする）
fn save_config(config: &Config) -> Result<PathBuf> {
    let path =
        config_path().ok_or("設定ファイルの場所が分かりません（HW_CONFIG を指定してください）")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let body = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    file.write_all(body.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("https://") || url.starts_with("http://") {
        Ok(url.to_string())
    } else {
        Err(format!(
            "URL「{url}」は https:// か http:// で始めてください"
        ))
    }
}

// ────────────────────────────────────────────────────────────────────────────
// API
// ────────────────────────────────────────────────────────────────────────────

struct Api {
    url: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl Api {
    fn new(url: String, token: String) -> Self {
        Self {
            url,
            token,
            http: reqwest::blocking::Client::builder()
                .user_agent(concat!("hw-cli/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("HTTP クライアントを作れませんでした"),
        }
    }

    fn request(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self
            .http
            .request(method, format!("{}/api{path}", self.url))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req
            .send()
            .map_err(|e| format!("{} に接続できませんでした: {e}", self.url))?;
        let status = res.status();
        let text = res.text().unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(value);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(
                "トークンが無効です。Web の設定画面でトークンを発行して hw login し直してください"
                    .into(),
            );
        }
        Err(value["error"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("サーバーがエラーを返しました（{status}）")))
    }

    fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None)
    }
    fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(reqwest::Method::POST, path, Some(body))
    }
    fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.request(reqwest::Method::PATCH, path, Some(body))
    }
    fn delete(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::DELETE, path, None)
    }
}

/// 設定ファイルと環境変数（HW_URL・HW_TOKEN が優先）から API を用意する
fn api(config: &Config) -> Result<Api> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let url = env("HW_URL")
        .or_else(|| config.url.clone())
        .ok_or("ログインしていません。hw login --url <宿題ボードの URL> を実行してください")?;
    let token = env("HW_TOKEN")
        .or_else(|| config.token.clone())
        .ok_or("ログインしていません。hw login を実行してください")?;
    Ok(Api::new(normalize_url(&url)?, token))
}

/// 操作するサーバーを決める（--guild > HW_GUILD > hw use で選んだもの > 1つだけならそれ）
fn pick_guild(guilds: &[Value], wanted: Option<&str>) -> Result<String> {
    let id = |g: &Value| g["id"].as_str().unwrap_or("").to_string();
    let name = |g: &Value| g["name"].as_str().unwrap_or("").to_string();
    let names = || {
        guilds
            .iter()
            .map(|g| format!("  {}  {}", id(g), name(g)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match wanted.map(str::trim).filter(|w| !w.is_empty()) {
        Some(w) => {
            if let Some(g) = guilds.iter().find(|g| id(g) == w) {
                return Ok(id(g));
            }
            let lower = w.to_lowercase();
            let hits: Vec<&Value> = guilds
                .iter()
                .filter(|g| name(g).to_lowercase().contains(&lower))
                .collect();
            match hits.as_slice() {
                [g] => Ok(id(g)),
                [] => Err(format!(
                    "サーバー「{w}」が見つかりません。使えるサーバー:\n{}",
                    names()
                )),
                _ => Err(format!(
                    "「{w}」に当てはまるサーバーが複数あります。ID で指定してください:\n{}",
                    names()
                )),
            }
        }
        None => match guilds {
            [g] => Ok(id(g)),
            [] => Err("使えるサーバーがありません（ボットがいるサーバーに参加してから、トークンを発行し直してください）".into()),
            _ => Err(format!(
                "サーバーが複数あります。hw use <名前> で選ぶか、--guild で指定してください:\n{}",
                names()
            )),
        },
    }
}

// ────────────────────────────────────────────────────────────────────────────
// 表示
// ────────────────────────────────────────────────────────────────────────────

const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];

/// 曜日（0 = 日曜）
fn weekday(y: i32, m: u32, d: u32) -> usize {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32).rem_euclid(7)) as usize
}

/// `2025-10-10 08:30` → `10/10(金) 08:30`
fn short_due(due: &str) -> String {
    let parse = || -> Option<String> {
        let (date, time) = due.split_once(' ')?;
        let mut parts = date.split('-');
        let y: i32 = parts.next()?.parse().ok()?;
        let m: u32 = parts.next()?.parse().ok()?;
        let d: u32 = parts.next()?.parse().ok()?;
        Some(format!("{m}/{d}({}) {time}", WEEKDAYS[weekday(y, m, d)]))
    };
    parse().unwrap_or_else(|| due.to_string())
}

/// 期限までの残り（`あと3日` / `5時間前に期限切れ`）
fn relative(due_unix: i64, now: i64) -> String {
    let diff = due_unix - now;
    let abs = diff.abs();
    let s = if abs < 3600 {
        format!("{}分", abs / 60)
    } else if abs < 48 * 3600 {
        format!("{}時間", abs / 3600)
    } else {
        format!("{}日", abs / 86400)
    };
    if diff >= 0 {
        format!("あと{s}")
    } else {
        format!("{s}前に期限切れ")
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 宿題・テスト 1 件の表示
fn hw_line(hw: &Value, now: i64) -> String {
    let is_exam = hw["kind"] == "exam";
    let mut line = format!(
        "#{:<4} {} {}｜{}  {} {}（{}）",
        hw["id"].as_i64().unwrap_or(0),
        if is_exam { "テスト" } else { "宿題  " },
        hw["subject"].as_str().unwrap_or(""),
        hw["title"].as_str().unwrap_or(""),
        if is_exam { "日時" } else { "期限" },
        short_due(hw["due"].as_str().unwrap_or("")),
        relative(hw["due_unix"].as_i64().unwrap_or(0), now),
    );
    if !is_exam {
        line += &format!("  完了 {}人", hw["done_count"].as_i64().unwrap_or(0));
        if hw["done_by_me"] == true {
            line += "  [自分は完了]";
        }
    }
    if hw["repeat"] == true {
        line += "  [毎週]";
    }
    if let Some(memo) = hw["memo"].as_str().filter(|m| !m.is_empty()) {
        line += &format!("\n       メモ: {}", memo.replace('\n', " / "));
    }
    line
}

fn print_list(list: &Value, empty: &str) {
    let items = list.as_array().map(Vec::as_slice).unwrap_or_default();
    if items.is_empty() {
        println!("{empty}");
        return;
    }
    let now = now_unix();
    for hw in items {
        println!("{}", hw_line(hw, now));
    }
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

// ────────────────────────────────────────────────────────────────────────────
// コマンド
// ────────────────────────────────────────────────────────────────────────────

fn read_token() -> Result<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("API トークン（Web の設定画面で発行）: ");
        std::io::stderr().flush().ok();
    }
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let token = line.trim().to_string();
    if token.is_empty() {
        return Err("トークンが入力されませんでした".into());
    }
    Ok(token)
}

fn run(cli: Cli) -> Result<()> {
    let mut config = load_config();
    let json = cli.json;

    // ログイン・ログアウトはサーバーを選ぶ前に済ませる
    match &cli.cmd {
        Cmd::Login { url, token } => {
            let url = url
                .clone()
                .or_else(|| config.url.clone())
                .ok_or("--url で宿題ボードの URL を指定してください")?;
            let url = normalize_url(&url)?;
            let token = match token {
                Some(t) => t.trim().to_string(),
                None => read_token()?,
            };
            let me = Api::new(url.clone(), token.clone()).get("/me")?;
            let guilds = me["guilds"].as_array().cloned().unwrap_or_default();
            config.url = Some(url);
            config.token = Some(token);
            // サーバーが 1 つならそれを選んでおく。前に選んだものが今も使えるなら残す
            let still_ok = config
                .guild
                .as_deref()
                .is_some_and(|g| guilds.iter().any(|x| x["id"] == g));
            if !still_ok {
                config.guild = (guilds.len() == 1)
                    .then(|| guilds[0]["id"].as_str().map(String::from))
                    .flatten();
            }
            let path = save_config(&config)?;
            if json {
                print_json(&me);
            } else {
                println!(
                    "{} としてログインしました（設定: {}）",
                    me["name"].as_str().unwrap_or("?"),
                    path.display()
                );
                if config.guild.is_none() && guilds.len() > 1 {
                    println!("サーバーが複数あります。hw use <名前> で選んでください。");
                }
            }
            return Ok(());
        }
        Cmd::Logout => {
            if let Some(path) = config_path().filter(|p| p.exists()) {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            println!(
                "ログアウトしました（トークン自体を無効にするには Web の設定画面で削除してください）"
            );
            return Ok(());
        }
        _ => {}
    }

    let api = api(&config)?;
    let me = api.get("/me")?;
    let guilds = me["guilds"].as_array().cloned().unwrap_or_default();
    let wanted = cli
        .guild
        .clone()
        .or_else(|| std::env::var("HW_GUILD").ok())
        .or_else(|| config.guild.clone());

    match cli.cmd {
        Cmd::Login { .. } | Cmd::Logout => unreachable!(),
        Cmd::Whoami => {
            if json {
                print_json(&me);
            } else {
                println!(
                    "{}（ID {}）",
                    me["name"].as_str().unwrap_or("?"),
                    me["id"].as_str().unwrap_or("?")
                );
                let current = pick_guild(&guilds, wanted.as_deref()).ok();
                for g in &guilds {
                    let id = g["id"].as_str().unwrap_or("");
                    let mark = if current.as_deref() == Some(id) {
                        "*"
                    } else {
                        " "
                    };
                    println!("{mark} {id}  {}", g["name"].as_str().unwrap_or(""));
                }
            }
        }
        Cmd::Use { guild } => {
            let id = pick_guild(&guilds, Some(&guild))?;
            config.guild = Some(id.clone());
            save_config(&config)?;
            let name = guilds
                .iter()
                .find(|g| g["id"] == id.as_str())
                .and_then(|g| g["name"].as_str())
                .unwrap_or("");
            println!("これからは「{name}」（{id}）を操作します");
        }
        cmd => {
            let gid = pick_guild(&guilds, wanted.as_deref())?;
            let g = |p: &str| format!("/guilds/{gid}{p}");
            match cmd {
                Cmd::Todo => {
                    let list = api.get(&g("/homework?scope=todo"))?;
                    if json {
                        print_json(&list)
                    } else {
                        print_list(&list, "未完了の宿題はありません")
                    }
                }
                Cmd::List { exam, past } => {
                    let kind = if exam { "exam" } else { "homework" };
                    let scope = if past { "past" } else { "upcoming" };
                    let list = api.get(&g(&format!("/homework?scope={scope}&kind={kind}")))?;
                    if json {
                        print_json(&list)
                    } else {
                        print_list(&list, "ありません")
                    }
                }
                Cmd::Add {
                    subject,
                    title,
                    due,
                    memo,
                    exam,
                } => {
                    let hw = api.post(
                        &g("/homework"),
                        json!({
                            "kind": if exam { "exam" } else { "homework" },
                            "subject": subject,
                            "title": title,
                            "due": due,
                            "memo": memo,
                        }),
                    )?;
                    if json {
                        print_json(&hw);
                    } else {
                        println!("登録しました:\n{}", hw_line(&hw, now_unix()));
                    }
                }
                Cmd::Done { ids } => set_done(&api, &g, &ids, true, json)?,
                Cmd::Undo { ids } => set_done(&api, &g, &ids, false, json)?,
                Cmd::Edit {
                    id,
                    subject,
                    title,
                    due,
                    memo,
                } => {
                    let mut body = serde_json::Map::new();
                    for (k, v) in [
                        ("subject", subject),
                        ("title", title),
                        ("due", due),
                        ("memo", memo),
                    ] {
                        if let Some(v) = v {
                            body.insert(k.into(), Value::String(v));
                        }
                    }
                    if body.is_empty() {
                        return Err(
                            "変える項目を --subject / --title / --due / --memo で指定してください"
                                .into(),
                        );
                    }
                    let hw = api.patch(&g(&format!("/homework/{id}")), Value::Object(body))?;
                    if json {
                        print_json(&hw)
                    } else {
                        println!("更新しました:\n{}", hw_line(&hw, now_unix()))
                    }
                }
                Cmd::Delete { id, yes } => {
                    if !yes {
                        return Err(format!(
                            "削除は取り消せません。削除するときは hw delete {id} --yes としてください"
                        ));
                    }
                    api.delete(&g(&format!("/homework/{id}")))?;
                    if json {
                        print_json(&json!({ "deleted": id }))
                    } else {
                        println!("#{id} を削除しました")
                    }
                }
                Cmd::Search { query } => {
                    let q: String = query
                        .bytes()
                        .map(|b| match b {
                            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                                (b as char).to_string()
                            }
                            _ => format!("%{b:02X}"),
                        })
                        .collect();
                    let res = api.get(&g(&format!("/search?q={q}")))?;
                    if json {
                        print_json(&res)
                    } else {
                        print_list(&res["homework"], "見つかりませんでした")
                    }
                }
                Cmd::Login { .. } | Cmd::Logout | Cmd::Whoami | Cmd::Use { .. } => unreachable!(),
            }
        }
    }
    Ok(())
}

fn set_done(
    api: &Api,
    g: &dyn Fn(&str) -> String,
    ids: &[i64],
    done: bool,
    json: bool,
) -> Result<()> {
    let mut results = Vec::new();
    for id in ids {
        let hw = api.post(&g(&format!("/homework/{id}/done")), json!({ "done": done }))?;
        if !json {
            println!(
                "{}: #{id} {} {}",
                if done {
                    "完了にしました"
                } else {
                    "未完了に戻しました"
                },
                hw["subject"].as_str().unwrap_or(""),
                hw["title"].as_str().unwrap_or("")
            );
        }
        results.push(hw);
    }
    if json {
        print_json(&Value::Array(results));
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("エラー: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_due_dates() {
        // 2025-10-10 は金曜日、2024-02-29 は木曜日
        assert_eq!(short_due("2025-10-10 08:30"), "10/10(金) 08:30");
        assert_eq!(short_due("2024-02-29 23:59"), "2/29(木) 23:59");
        assert_eq!(short_due("2026-01-01 00:00"), "1/1(木) 00:00");
        assert_eq!(short_due("変な値"), "変な値");
        assert_eq!(relative(1000 + 3 * 86400, 1000), "あと3日");
        assert_eq!(relative(1000 + 5 * 3600, 1000), "あと5時間");
        assert_eq!(relative(1000 - 30 * 60, 1000), "30分前に期限切れ");
    }

    #[test]
    fn picks_a_guild() {
        let guilds = vec![
            json!({ "id": "1", "name": "3年2組" }),
            json!({ "id": "2", "name": "部活" }),
        ];
        assert_eq!(pick_guild(&guilds, Some("2")).unwrap(), "2");
        assert_eq!(pick_guild(&guilds, Some("2組")).unwrap(), "1");
        assert!(pick_guild(&guilds, Some("なし")).is_err());
        assert!(pick_guild(&guilds, None).unwrap_err().contains("hw use"));
        assert_eq!(pick_guild(&guilds[..1], None).unwrap(), "1");
        assert!(pick_guild(&[], None).is_err());
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_url(" https://hw.example.com/ ").unwrap(),
            "https://hw.example.com"
        );
        assert!(normalize_url("hw.example.com").is_err());
    }

    #[test]
    fn prints_a_homework_line() {
        let hw = json!({
            "id": 12, "kind": "homework", "subject": "数学", "title": "ワーク p.10",
            "memo": "答え合わせ\nまで", "due": "2025-10-10 08:30", "due_unix": 1000 + 86400 * 2,
            "done_count": 3, "done_by_me": true, "repeat": false
        });
        let line = hw_line(&hw, 1000);
        assert!(line.starts_with("#12   宿題   数学｜ワーク p.10  期限 10/10(金) 08:30（あと2日）  完了 3人  [自分は完了]"));
        assert!(line.ends_with("メモ: 答え合わせ / まで"));
    }
}
