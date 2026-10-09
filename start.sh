#!/usr/bin/env sh
# リリースビルドしてボットを起動する（設定は .env から読み込まれる）
set -e
cd "$(dirname "$0")"
cargo build --release
exec ./target/release/discord_task_bot
