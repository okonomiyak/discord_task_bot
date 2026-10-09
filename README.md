# Discord Task Bot

Rust で書かれた Discord サーバー向けタスク管理ボット。スラッシュコマンドでタスクの追加・管理・リマインダー通知・Discord スケジュールイベント連携ができます。

## 機能

- タスクの追加・編集・削除・ステータス管理
- 優先度（低/中/高）と期限の設定
- 担当者のアサイン（複数人可）と担当者での絞り込み
- キーワード検索（タイトル・説明）
- 一覧のページ送り（10件ごと、◀ ▶ ボタン）と並び替え（優先度/期限/作成日）
- リマインダー通知（最大3つ、30分前〜1週間前）
- 期限切れ通知（未完了のまま期限を過ぎたタスクを1回通知）
- Discord スケジュールイベントと連携（編集時も同期）
- サーバー単位でタスクを共有（メンバー全員が閲覧・操作可能）
- 期限はタイムゾーン設定（デフォルト `Asia/Tokyo`）で解釈し、Discord のタイムスタンプ表示で各自のローカル時刻に変換

## 技術スタック

| ライブラリ | 用途 |
|-----------|------|
| [poise](https://github.com/serenity-rs/poise) 0.6 | スラッシュコマンドフレームワーク |
| [serenity](https://github.com/serenity-rs/serenity) 0.12 | Discord API |
| [rusqlite](https://github.com/rusqlite/rusqlite) 0.31 (bundled) | SQLite データベース |
| [tokio](https://tokio.rs) 1 | 非同期ランタイム |
| [chrono](https://github.com/chronotope/chrono) 0.4 / [chrono-tz](https://github.com/chronotope/chrono-tz) 0.10 | 日時・タイムゾーン処理 |

## セットアップ

### 必要なもの

- Rust (edition 2024)
- Discord Bot トークン（[Discord Developer Portal](https://discord.com/developers/applications) で取得）

### インストール

```bash
git clone <repo>
cd discord_task_bot
cp .env.example .env
```

`.env` を編集:

```env
DISCORD_TOKEN=your_discord_bot_token_here
DATABASE_URL=tasks.db
TIMEZONE=Asia/Tokyo            # 期限を解釈するタイムゾーン（省略時 Asia/Tokyo）
GUILD_ID=123456789012345678   # 省略するとグローバル登録（反映まで最大1時間）
```

### Bot の権限設定

Discord Developer Portal → OAuth2 → URL Generator で以下を有効化:

**Scopes:** `bot`, `applications.commands`

**Bot Permissions:**
- Send Messages
- Embed Links
- Manage Events（Discord イベント連携を使う場合）

### 起動

```bash
cargo run --release
```

または `./start.sh`（リリースビルドして起動）。

#### 常駐させる場合（systemd の例）

`/etc/systemd/system/discord-task-bot.service`:

```ini
[Unit]
Description=Discord Task Bot
After=network-online.target

[Service]
WorkingDirectory=/path/to/discord_task_bot
ExecStart=/path/to/discord_task_bot/target/release/discord_task_bot
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```bash
cargo build --release
sudo systemctl enable --now discord-task-bot
journalctl -u discord-task-bot -f   # ログ確認
```

`.env` と `tasks.db` は `WorkingDirectory` から読み書きされます。

### 開発

```bash
cargo fmt && cargo clippy --all-targets && cargo test
```

GitHub Actions で push / PR ごとに同じチェックが走ります。

## コマンド一覧

### `/task add`
タスクを追加する。

| パラメータ | 必須 | 説明 |
|-----------|------|------|
| `title` | ✅ | タスクのタイトル |
| `description` | | 説明 |
| `priority` | | 優先度（低/中/高、デフォルト: 中） |
| `due_date` | | 期限（例: `2025-12-31 15:00`）。不正な書式はエラーになる |
| `assignee` | | 担当者 |
| `remind1〜3` | | リマインダー（30分前/1時間前/3時間前/1日前/3日前/1週間前） |
| `create_event` | | Discord スケジュールイベントも作成する（true/false） |

### `/task list`
タスク一覧を表示する。10件を超えると ◀ ▶ ボタンでページ送りできる。

| パラメータ | 説明 |
|-----------|------|
| `filter` | ステータス絞り込み（すべて/未完了/待機中/進行中/完了） |
| `assignee` | 担当者で絞り込み |
| `sort` | 並び順（優先度順/期限が近い順/新しい順） |

### `/task search`
タイトル・説明から `keyword` を含むタスクを検索する。`filter` でステータス絞り込みも可能。

### `/task view`
タスクの詳細を表示する。`id` を指定。

### `/task status`
タスクのステータスを変更する。`id` と新しいステータスを指定。

### `/task assign` / `/task unassign`
担当者を追加する（`user`〜`user3` で最大3人同時）／外す。担当者がいるタスクのリマインダー・期限切れ通知は担当者にメンションされる（いなければ作成者）。

### `/task edit`
タスクを編集する。`id` と変更したい項目を指定。`remind1〜3` を1つでも指定するとリマインダーが全置き換えされる。期限を変更するとリマインダー・期限切れ通知が再設定され、Discord イベントが紐づいていれば内容も同期される。

### `/task delete`
タスクを削除する。紐づく Discord スケジュールイベントも同時に削除される。

### `/now`
現在の日時を Discord タイムスタンプ形式で表示する。

### `/help`
コマンド一覧と使い方を表示する。

## データベース

SQLite（`tasks.db`）を使用。初回起動時に自動作成される。

```
tasks
├── id, user_id, guild_id
├── title, description
├── status (Pending / InProgress / Done)
├── priority (Low / Medium / High)
├── due_date (YYYY-MM-DD HH:MM, TIMEZONE の時刻), created_at, channel_id
├── discord_event_id
└── overdue_notified (期限切れ通知済みフラグ)

reminders
├── id, task_id
├── remind_before (秒数)
└── reminded (送信済みフラグ)

assignees
└── task_id, user_id
```

既存の `tasks.db` は起動時に自動でマイグレーションされます（期限の書式も正規化）。

## 期限の書式

```
YYYY-MM-DD HH:MM   （例: 2025-12-31 09:00）
YYYY-MM-DD         （時刻省略時は 00:00 扱い）
YYYY/MM/DD HH:MM   （/ 区切りも可）
```

時刻は `TIMEZONE`（デフォルト `Asia/Tokyo`）の時刻として解釈されます。

## 通知の仕様

- ⏰ **リマインダー**: 期限の指定時間前に、タスクを作成したチャンネルへ通知。登録時点で通知時刻を過ぎているものは送らない
- 🚨 **期限切れ**: 未完了のまま期限を過ぎたタスクを1回だけ通知（期限を変更すると再び有効になる）
- チェックは60秒ごと
