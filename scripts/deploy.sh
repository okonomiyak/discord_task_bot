#!/usr/bin/env bash
# 自宅 PC のボットを指定したコミットに更新し、ビルドして再起動する。
# GitHub Actions（.github/workflows/deploy.yml）から呼ばれるが、手で実行してもよい。
#
#   scripts/deploy.sh <ボットのディレクトリ> [コミット（省略時は origin/master）] [サービス名（省略時は task-bot）]
#
# - ビルドに失敗したら再起動せず、今動いているボットはそのまま
# - 再起動後にボットが動いていなければ、前のコミットとバイナリに戻して再起動する
set -euo pipefail

BOT_DIR=${1:?ボットのディレクトリを指定してください}
TARGET=${2:-}
SERVICE=${3:-task-bot}
HEALTH_SECONDS=${HEALTH_SECONDS:-20}
BIN=target/release/discord_task_bot

log() { echo "==> $*"; }
fail() { echo "::error::$*" >&2; exit 1; }

# root でなければ sudo -n（パスワードなし sudo が必要）で systemctl を呼ぶ
SUDO=()
if [ "$(id -u)" -ne 0 ]; then SUDO=(sudo -n); fi
sctl() { "${SUDO[@]}" systemctl "$@"; }

cd "$BOT_DIR" || fail "$BOT_DIR が見つかりません"
[ -d .git ] || fail "$BOT_DIR は git のチェックアウトではありません"

branch=$(git rev-parse --abbrev-ref HEAD)
[ "$branch" = master ] || fail "$BOT_DIR が master ブランチではありません（今は $branch）"
if ! git diff --quiet HEAD --; then
    git status --short --untracked-files=no
    fail "$BOT_DIR に追跡中のファイルの変更があります。git stash などで片付けてから再実行してください"
fi

# --- 更新 ---------------------------------------------------------------
PREV=$(git rev-parse HEAD)
log "取得: origin/master"
git fetch --quiet origin master
TARGET=${TARGET:-$(git rev-parse FETCH_HEAD)}
git cat-file -e "$TARGET^{commit}" 2>/dev/null || fail "コミット $TARGET が見つかりません"

if [ "$(git rev-parse "$TARGET^{commit}")" = "$PREV" ]; then
    log "すでに $(git log -1 --format='%h %s' "$PREV") です"
else
    git merge --ff-only --quiet "$TARGET" \
        || fail "$TARGET に早送りできません（$BOT_DIR に master にないコミットがあるかもしれません）"
    log "更新: $(git log -1 --format='%h' "$PREV") → $(git log -1 --format='%h %s' HEAD)"
fi

# --- ビルド -------------------------------------------------------------
# systemd や Actions のランナーからだと PATH / HOME に cargo が無いことがあるので探す（start.sh と同じ）
if ! command -v cargo >/dev/null 2>&1; then
    for dir in "${HOME:-}/.cargo/bin" /root/.cargo/bin; do
        if [ -x "$dir/cargo" ]; then
            PATH="$dir:$PATH"
            if [ ! -d "${HOME:-}/.rustup" ]; then HOME="${dir%/.cargo/bin}"; fi
            export PATH HOME
            break
        fi
    done
fi
command -v cargo >/dev/null 2>&1 || fail "cargo が見つかりません"

if [ -x "$BIN" ]; then cp -p "$BIN" "$BIN.prev"; fi

rollback_source() {
    if [ "$(git rev-parse HEAD)" != "$PREV" ]; then
        git reset --quiet --keep "$PREV"
        log "ソースを $(git log -1 --format='%h' "$PREV") に戻しました"
    fi
}

log "ビルド: cargo build --release --locked"
if ! cargo build --release --locked; then
    rollback_source
    if [ -f "$BIN.prev" ]; then cp -p "$BIN.prev" "$BIN"; fi
    fail "ビルドに失敗しました。ボットは再起動していません（前のバージョンのまま動いています）"
fi

# --- 再起動と確認 -------------------------------------------------------
# 起動直後に落ちる（トークンの間違い、パニックなど）と Restart=on-failure で
# activating と failed を行き来するので、しばらく active のままか見張る
healthy() {
    local i state
    for ((i = 0; i < HEALTH_SECONDS; i++)); do
        sleep 1
        state=$(sctl is-active "$SERVICE" || true)
        if [ "$state" != active ]; then
            echo "  $SERVICE: $state（$((i + 1)) 秒後）" >&2
            return 1
        fi
    done
}

log "再起動: $SERVICE"
sctl restart "$SERVICE"
if healthy; then
    log "デプロイ完了: $(git log -1 --format='%h %s')"
    exit 0
fi

echo "::group::journalctl -u $SERVICE"
"${SUDO[@]}" journalctl -u "$SERVICE" -n 50 --no-pager || true
echo "::endgroup::"

log "起動しなかったので前のバージョンに戻します"
rollback_source
if [ -f "$BIN.prev" ]; then
    cp -p "$BIN.prev" "$BIN"
    sctl reset-failed "$SERVICE" || true
    sctl restart "$SERVICE"
    if healthy; then
        fail "新しいバージョンが起動しなかったため、前のバージョン（$(git log -1 --format='%h' "$PREV")）に戻しました"
    fi
fi
fail "新しいバージョンが起動せず、前のバージョンにも戻せませんでした。journalctl -u $SERVICE を確認してください"
