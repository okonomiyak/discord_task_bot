#!/usr/bin/env sh
# ボットをビルドして起動する（設定は .env から読み込まれる）
#
# systemd などから起動するとログイン時の PATH や HOME が使われず、cargo（~/.cargo/bin）が
# 見つからない・動かないことがある。そのときは cargo を探し、ビルドできなければ
# ビルド済みのボットをそのまま起動する。
cd "$(dirname "$0")" || exit 1

BIN=./target/release/discord_task_bot

if ! command -v cargo >/dev/null 2>&1; then
    for dir in "${HOME:-}/.cargo/bin" /root/.cargo/bin; do
        if [ -x "$dir/cargo" ]; then
            PATH="$dir:$PATH"
            # rustup は HOME の下からツールチェーンを探すので、HOME が無い・違うときは合わせる
            if [ ! -d "${HOME:-}/.rustup" ]; then
                HOME="${dir%/.cargo/bin}"
            fi
            export PATH HOME
            break
        fi
    done
fi

if command -v cargo >/dev/null 2>&1 && cargo build --release; then
    :
elif [ -x "$BIN" ]; then
    echo "ビルドできなかったため、ビルド済みのボットをそのまま起動します" >&2
else
    echo "ビルドできず、ビルド済みのボット ($BIN) もありません。" >&2
    echo "ログインしたターミナルで cargo build --release を実行してください。" >&2
    exit 1
fi

exec "$BIN"
