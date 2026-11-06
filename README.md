# Discord Task Bot

Rust で書かれた Discord サーバー向けのタスク・宿題管理ボット。スラッシュコマンドでタスクや宿題の登録・管理、リマインダー通知、毎日のまとめ投稿、Discord スケジュールイベント連携ができます。

## 機能

### 宿題（`/hw`）

友達・クラスメイトのサーバー向け。**課題はみんなで共有、完了は1人ずつ**記録します。

- 誰か1人が登録すれば全員に共有。`/hw todo` で「自分がまだ終わっていない宿題」だけ見られる
- 期限は `明日` `金曜` `12/5 13:00` のように気軽に書ける
- `/hw done` は候補から選ぶだけ（ID を覚えなくていい）。登録時・通知・まとめ投稿の **✅ ボタン**でも完了にできる
- テスト・小テストの予定も登録でき、まとめ投稿や通知に載る
- 毎週出る宿題は一度設定すれば、翌週の分が自動で登録される
- 通知はすべて自由に設定できる
  - サーバー: 期限前のチャンネル通知（初期値: 1日前）、毎日のまとめ投稿（初期値: 毎日 21:00）
  - 期限前の通知（チャンネル・DM）は 22:00〜7:00 には送らない。22 時台・23 時台に当たる通知はその晩の 22:00 に早める（23:59 締め切りの「1日前」は前日の 22:00 に届く）。0:00〜7:00 に当たる通知は朝 7:00 に（間に合わなければ前の晩の 22:00）
  - 個人: 自分が未完了の宿題だけ DM でお知らせ（初期値: オフ）

### Web ダッシュボード

Discord でログインして、ブラウザ（スマホ・PC）から宿題・テスト・タスクを確認・完了・登録・編集できます。

- 「やること」「宿題」「テスト」「タスク」「カレンダー」のタブ
- PC（幅 1024px 以上）では左にサイドバー。やることは「宿題｜達成率・テスト」の2段組、一覧はカードを2〜3列に並べ、カレンダーはマスに内容まで表示して選んだ日の予定を右に表示
- 「やること」の上に自分の達成率（今週・今月・全期間、科目別）
- 「完了 ○人」を押すと、完了した人の名前とアイコンを表示。「クラス ○%」で何割が終わったか分かる
  （分母は「このサーバーで宿題の完了を記録したことがある人」。達成率カードにはクラス平均も表示）
- 検索: 宿題・テスト（科目・内容・メモ）とタスク（タイトル・説明）を、期限が過ぎたものも含めて探せる
- 飾りの絵文字や角丸のない、すっきりした見た目。科目は内容の上に色付きの見出しで表示し、カードの左端も科目の色に（国語=赤・数学=青・英語=紫・理科=緑・社会=オレンジ など。その他の科目も名前ごとに一定の色）
- メモに書いた URL はタップで開ける
- 「テキストでコピー」: やること・宿題・テストの一覧を、LINE などに貼れる文章でコピー（`/hw share` と同じ書き方）
- 「複製」: 編集画面から、科目・内容・メモが入った状態で新しく追加できる（期限だけ入れ直す）
- 「カレンダーに追加」: Google カレンダーの予定作成画面を、内容・期限・メモが入った状態で開く。保存したときだけ自分のカレンダーに入る
  （Google の API は使わないので設定は不要。宿題は期限から 30 分、テストは 1 時間の予定）。Discord の登録メッセージと `/hw view` にも同じボタンが付く
- 使い方: 右上の「？」から、画面の見方・期限の書き方・通知・よくある質問・Discord のコマンド一覧を確認できる
- 設定画面: サーバーの設定（通知チャンネル・期限前の通知・まとめ投稿・既定の時刻）、自分への DM 通知、毎週の宿題の追加・停止
- ✅ ボタンで自分の分を完了に。追加・編集・削除もその場で
- Web から登録したものは Discord のチャンネルにも投稿される（完了ボタン付き）
- 見られるのは「自分が入っていて、ボットもいるサーバー」だけ。削除の権限は Discord と同じ
- OGP 対応: URL を Discord・LINE・X などに貼ると、画像付きのカードで表示される（画像は `src/web/og.png`、作り直すときは `assets/render-og.js`）。
  og:url・og:image は `WEB_BASE_URL` から作るので、外から見た URL を正しく設定しておく。スマホでホーム画面に追加したときのアイコンもある
- Discord から: `/web` でリンクを表示（`public:true` でチャンネルに貼れる）。登録・期限前の通知・まとめ投稿・一覧・
  `/hw stats`・`/help` のメッセージにも「Web で開く」ボタンが付き、そのサーバーの該当する画面（やること・宿題・テスト・タスク・設定）が開く。
  ログインしていなくても、ログイン後にその画面が開く

セットアップは「[Web ダッシュボードを有効にする](#web-ダッシュボードを有効にする)」を参照。

### タスク（`/task`）

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
| [axum](https://github.com/tokio-rs/axum) 0.8 / [reqwest](https://github.com/seanmonstar/reqwest) 0.12 | Web ダッシュボード・Discord OAuth2 |
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
- Manage Roles（課題通知ロール `/hw role setup` を使う場合）

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
Wants=network-online.target

[Service]
WorkingDirectory=/path/to/discord_task_bot
# ビルド済みのボットを直接起動する（systemd からは cargo が見えないことが多いので、ビルドはしない）
ExecStart=/path/to/discord_task_bot/target/release/discord_task_bot
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
cargo build --release                       # ログインしたターミナルでビルド
sudo systemctl daemon-reload
sudo systemctl enable --now discord-task-bot
journalctl -u discord-task-bot -f           # ログ確認
```

更新するとき:

```bash
git pull && cargo build --release && sudo systemctl restart discord-task-bot
```

`ExecStart` に `start.sh` を指定することもできます。その場合は `~/.cargo/bin` の cargo を探してビルドし、
ビルドできなければビルド済みのボットをそのまま起動します。

起動に何度も失敗して「start request repeated too quickly」で止まったときは、原因を直してから
`sudo systemctl reset-failed discord-task-bot && sudo systemctl start discord-task-bot` で再開できます。

`.env` と `tasks.db` は `WorkingDirectory` から読み書きされます。

#### GitHub Actions で自動デプロイする

master に push（PR をマージ）して CI が通ると、自宅 PC の
[セルフホストランナー](https://docs.github.com/ja/actions/hosting-your-own-runners)が
`scripts/deploy.sh` を実行し、ボットを更新して再起動します（`.github/workflows/deploy.yml`）。

- `git pull`（早送りのみ）→ `cargo build --release --locked` → `systemctl restart` → 20 秒間動いているか確認
- ビルドに失敗したら再起動しません（前のバージョンのまま動き続けます）
- 再起動後に落ちたら、前のコミットとバイナリに戻して再起動し、ジョブを失敗にします（ログに journalctl を出します）
- Actions 画面の **Deploy → Run workflow** から手動でも実行できます

設定（初回だけ）:

1. **ランナーを登録する**: GitHub のリポジトリの **Settings → Actions → Runners → New self-hosted runner** で
   Linux / x64 を選び、表示されるコマンドを自宅 PC で実行します。`config.sh` には `--labels task-bot` を付けます。
   ```bash
   mkdir ~/actions-runner && cd ~/actions-runner
   # 画面に出る curl / tar を実行してから:
   ./config.sh --url https://github.com/<owner>/discord_task_bot --token <画面のトークン> --labels task-bot
   sudo ./svc.sh install "$(whoami)"   # サービスとして常駐させる
   sudo ./svc.sh start
   ```
   root で動かす場合は `config.sh` の前に `export RUNNER_ALLOW_RUNASROOT=1` が必要です。
   root 以外で動かす場合は、そのユーザーがボットのディレクトリに書き込めて、
   `sudo -n systemctl restart <サービス名>`（と `is-active` / `reset-failed`、`journalctl`）を
   パスワードなしで実行できるようにしてください。
2. **変数を設定する**: **Settings → Secrets and variables → Actions → Variables** に追加します。
   | 名前 | 例 | 説明 |
   |---|---|---|
   | `BOT_DIR` | `/root/discord_task_bot` | ボットが動いているディレクトリ（必須。未設定のあいだはデプロイしません） |
   | `BOT_SERVICE` | `task-bot` | systemd のサービス名（省略時は `task-bot`） |
3. `BOT_DIR` は **master ブランチ**のチェックアウトにして、追跡中のファイルを直接書き換えないでください
   （変更があるとデプロイは止まります。`.env` と `tasks.db` は追跡対象外なので大丈夫です）。

> **公開リポジトリの場合の注意**: セルフホストランナーは、PR のワークフローからも使われる可能性があります。
> **Settings → Actions → General → Approval for running fork pull request workflows from contributors** を
> **Require approval for all external contributors** にして、知らない人の PR のワークフローが
> 承認なしで動かないようにしてください（このリポジトリのワークフローは PR ではランナーを使いません）。
> 心配ならリポジトリを非公開にするのが確実です。

### Web ダッシュボードを有効にする

1. [Discord Developer Portal](https://discord.com/developers/applications) でボットのアプリを開き、**OAuth2** を開く
   - **Client ID** と **Client Secret**（Reset Secret で発行）を控える
   - **Redirects** に `https://<あなたのドメイン>/auth/callback` を追加して保存
2. `.env` に追加:
   ```env
   DISCORD_CLIENT_ID=123456789012345678
   DISCORD_CLIENT_SECRET=xxxxxxxx
   WEB_BASE_URL=https://hw.example.com   # 外から見た URL（末尾の / は不要）
   WEB_ADDR=0.0.0.0:8080                 # 省略可
   ```
3. ボットを再起動すると `Web ダッシュボード: https://hw.example.com` と表示される

**外部に公開するときは HTTPS にしてください。** ログイン情報（Cookie）を守るためです。自宅 PC なら次のどちらかが簡単です:

- **Cloudflare Tunnel**（ポート開放不要・HTTPS 自動）:
  ```bash
  cloudflared tunnel --url http://localhost:8080   # お試し用（URL は毎回変わる）
  ```
  独自ドメインで常用するなら `cloudflared tunnel create` → `cloudflared tunnel route dns` で固定 URL にする
- **Caddy**（ポート 80/443 を PC に転送している場合、証明書を自動取得）:
  ```
  hw.example.com {
      reverse_proxy localhost:8080
  }
  ```

`WEB_BASE_URL` と Developer Portal の Redirects は**完全に一致**させてください（http/https、ドメイン、末尾 `/auth/callback`）。

### 二重起動の防止

同じデータベースを使うボットを2つ起動すると、1回のコマンドが2回実行されてしまいます（宿題が2件登録されるなど）。
そのため、2つ目は「既に起動しています」と表示して終了します（`tasks.db.lock` でロック）。
古いプロセスが残っている場合は止めてから起動してください:

```bash
ps aux | grep discord_task_bot
kill <PID>
```

### 開発

```bash
cargo fmt && cargo clippy --all-targets && cargo test
```

GitHub Actions で push / PR ごとに同じチェックが走ります。

## コマンド一覧

### 宿題

| コマンド | 説明 |
|---------|------|
| `/hw add subject title due [memo]` | 宿題を登録する。`subject` は過去に使った科目が候補に出る |
| `/hw todo [subject]` | 自分が未完了の宿題（直近1週間の期限切れも含む）。自分にだけ表示 |
| `/hw list [subject] [past]` | これからの宿題一覧と完了人数。`past:true` で期限が過ぎたもの |
| `/hw done id` / `/hw undo id` | 自分の分を完了にする／取り消す。`id` は入力すると候補が出る |
| `/hw view id` | 詳細と完了した人（テストも可） |
| `/hw copy id due [title] [memo]` | 前の宿題・テストを複製して登録。科目・内容・メモはそのままで、新しい期限だけ指定する（内容・メモも変えられる） |
| `/hw share [kind] [mine] [subject]` | これからの宿題・テストを「・数学 ワーク p.10（11/6(金) 08:30まで）」のような文章で表示（自分にだけ）。長押しでコピーして LINE などに貼れる。`mine:true` で宿題は自分の未完了だけ |
| `/hw edit id ...` | 科目・内容・期限・メモを変更（テストも可）。期限を変えると通知も再設定 |
| `/hw delete id` | 削除（テストも可）。登録した人か「メッセージの管理」権限がある人のみ |
| `/hw exam add subject title date [memo]` | テスト・小テストを登録する（完了の記録はなし） |
| `/hw exam list [subject] [past]` | テストの予定 |
| `/hw repeat add subject title weekday [time] [memo]` | 毎週の宿題を登録。今回の分をすぐ登録し、期限が過ぎるたびに翌週の分を自動登録 |
| `/hw repeat list` / `/hw repeat delete id` | 毎週の宿題の一覧・停止（登録済みの宿題は残る） |
| `/hw settings [channel] [remind] [summary] [default_time] [link_preview] [mention]` | サーバーの設定。何も指定しないと現在の設定を表示。`mention` は通知ロールを鳴らすとき（初期値: まとめ投稿だけ）。`link_preview` は通知などの本文に入った URL をカードで展開するか（初期値: 出さない。期限前の通知・DM・タスクのリマインダー・Web から登録したときの投稿・`/hw share` に効く） |
| `/hw stats [period] [public]` | 自分の宿題の達成率（全体・期限が過ぎた分・これからの分・科目別）。期間は今週/今月/全期間。`public:true` でチャンネルに表示 |
| `/hw notify [timing]` | 自分への DM 通知のタイミング（オフ / 1時間前〜1週間前） |
| `/hw role setup [role]` | 課題通知ロールを作成（または既存ロールを指定）し、「🔔 通知を受け取る／🔕 受け取らない」ボタンを投稿。要「ロールの管理」権限 |
| `/hw role remove` | 課題通知ロールのメンションをやめる（ロール自体は残る） |

**課題通知ロール**を設定すると、毎日のまとめ投稿（載せるものがある日だけ）でそのロールをメンションします。`/hw settings mention:登録・期限前の通知・まとめ投稿`（Web の設定画面でも可）にすると、宿題・テストの登録（Discord・Web とも）と期限前のチャンネル通知でも鳴らします。メッセージで鳴らすのはこのロールだけで、宿題のタイトルなどに書かれた他のメンションでは通知されません。ボタンで付け外しするには、ボットのロールが通知ロールより上にある必要があります（`/hw role setup` で作ったロールは自動的にそうなります）。

**`/hw settings` の項目**

| 項目 | 初期値 | 説明 |
|------|--------|------|
| `channel` | 未設定 | 通知・まとめ投稿の送り先。未設定なら、通知は宿題を登録したチャンネル、まとめ投稿は最後に宿題・タスクを登録したチャンネル |
| `remind` | 1日前 | 期限前にチャンネルへ通知（オフ可）。通知時刻より後に登録された宿題には送らない |
| `summary` | 21:00 | 毎日この時刻に「今日／明日／1週間以内」の宿題、2週間以内のテスト、未完了のタスク（期限切れ・1週間以内）をまとめて投稿（✅ ボタン付き）。`7:30` のように変更、`オフ` で停止 |
| `default_time` | 08:30 | 期限を `明日` のように日付だけで書いたときの時刻（`/task` も共通） |

✅ ボタンは押した人の分だけ完了になります（押した人にだけ結果が表示され、「取り消す」ボタンも出ます）。ボットを再起動した後や、DM のボタンでも使えます。

**期限の書き方**

```
明日 / 明後日 / 今日 23:59 / 明日17時 / 明日17時半
金 / 金曜 / 金曜日          … 次のその曜日（今日と同じ曜日なら来週）
12/5 / 12/5 13:00           … 年は省略可（過ぎていれば来年）
2025-12-05 / 2025/12/05 9:00
```

### タスク

#### `/task add`
タスクを追加する。

| パラメータ | 必須 | 説明 |
|-----------|------|------|
| `title` | ✅ | タスクのタイトル |
| `description` | | 説明 |
| `priority` | | 優先度（低/中/高、デフォルト: 中） |
| `due_date` | | 期限（例: `明日` `金曜 17:00` `2025-12-31 15:00`）。不正な書式はエラーになる |
| `assignee` | | 担当者 |
| `remind1〜3` | | リマインダー（30分前/1時間前/3時間前/1日前/3日前/1週間前） |
| `create_event` | | Discord スケジュールイベントも作成する（true/false） |

#### `/task list`
タスク一覧を表示する。10件を超えると ◀ ▶ ボタンでページ送りできる。

| パラメータ | 説明 |
|-----------|------|
| `filter` | ステータス絞り込み（すべて/未完了/待機中/進行中/完了） |
| `assignee` | 担当者で絞り込み |
| `sort` | 並び順（優先度順/期限が近い順/新しい順） |

#### `/task search`
タイトル・説明から `keyword` を含むタスクを検索する。`filter` でステータス絞り込みも可能。

#### `/task view`
タスクの詳細を表示する。`id` を指定。

#### `/task status`
タスクのステータスを変更する。`id` と新しいステータスを指定。

#### `/task assign` / `/task unassign`
担当者を追加する（`user`〜`user3` で最大3人同時）／外す。担当者がいるタスクのリマインダー・期限切れ通知は担当者にメンションされる（いなければ作成者）。

#### `/task edit`
タスクを編集する。`id` と変更したい項目を指定。`remind1〜3` を1つでも指定するとリマインダーが全置き換えされる。期限を変更するとリマインダー・期限切れ通知が再設定され、Discord イベントが紐づいていれば内容も同期される。

#### `/task delete`
タスクを削除する。紐づく Discord スケジュールイベントも同時に削除される。

### その他

#### `/now`
現在の日時を Discord タイムスタンプ形式で表示する。

#### `/web`
Web ダッシュボードのリンクを表示する（Web ダッシュボードが有効なときだけ）。
`public:true` にするとチャンネルのみんなに見える。

#### `/help`
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

homework
├── id, guild_id, channel_id, created_by
├── subject, title, description
├── due_date, created_at (UNIX 秒)
└── kind (homework / exam), repeat_id

hw_progress      … 人ごとの完了 (homework_id, user_id, done_at)
hw_repeats       … 毎週の宿題 (weekday, time, last_due …)
hw_settings      … サーバーごとの設定
hw_dm_settings   … 個人の DM 通知設定
hw_sent          … 送信済み通知 (homework_id, target, remind_before)

reminders
├── id, task_id
├── remind_before (秒数)
└── reminded (送信済みフラグ)

assignees
└── task_id, user_id
```

既存の `tasks.db` は起動時に自動でマイグレーションされます（期限の書式も正規化）。

## 期限の書式（/task）

`/hw` と同じ書き方が使えます（`明日` `金曜 17:00` `12/5` `2025-12-31 09:00` など）。
時刻を省略すると `/hw settings` の `default_time`（初期値 08:30）になります。

時刻は `TIMEZONE`（デフォルト `Asia/Tokyo`）の時刻として解釈されます。

## 通知の仕様（/task）

- ⏰ **リマインダー**: 期限の指定時間前に、タスクを作成したチャンネルへ通知。登録時点で通知時刻を過ぎているものは送らない
- 🚨 **期限切れ**: 未完了のまま期限を過ぎたタスクを1回だけ通知（期限を変更すると再び有効になる）
- チェックは60秒ごと
