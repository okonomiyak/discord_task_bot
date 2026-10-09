#!/usr/bin/env sh
# Docker でボットをビルド・起動し、ログを表示する
set -e
docker compose up -d --build
docker compose logs -f
