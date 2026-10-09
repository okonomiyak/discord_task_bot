# ビルドステージ（実行ステージと同じ Debian バージョンに揃えて glibc の不一致を防ぐ）
FROM rust:1-slim-trixie AS builder
WORKDIR /app

# 依存クレートだけ先にビルドしてキャッシュを効かせる
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

# 実行ステージ（軽量）
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/discord_task_bot /usr/local/bin/discord_task_bot

# SQLite の保存先（docker-compose でボリュームをマウントする）
ENV DATABASE_URL=/data/tasks.db
VOLUME /data

CMD ["discord_task_bot"]
