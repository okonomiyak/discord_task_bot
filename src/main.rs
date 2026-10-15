mod commands;
mod db;
mod hw_notify;
mod models;
mod reminder;
mod time;

use std::sync::Arc;

use poise::serenity_prelude as serenity;

#[derive(Debug)]
pub struct Data {
    pub db: db::Database,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

/// 同じ DB を使うボットが既に動いていないか確認し、ロックを取る。
/// 返したファイルを保持している間ロックが続き、プロセス終了で自動的に解放される
fn acquire_instance_lock(db_path: &str) -> Result<std::fs::File, String> {
    let lock_path = format!("{db_path}.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("ロックファイル {lock_path} を開けませんでした: {e}"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "同じデータベース ({db_path}) を使うボットが既に起動しています。\n\
             二重に動かすとコマンドが2回実行されるため終了します。\n\
             もう一方を止めてから起動してください（例: ps aux | grep discord_task_bot）。"
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!(
            "ロックファイル {lock_path} をロックできませんでした: {e}"
        )),
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN が設定されていません");
    let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| "tasks.db".to_string());

    let tz: chrono_tz::Tz = std::env::var("TIMEZONE")
        .unwrap_or_else(|_| "Asia/Tokyo".to_string())
        .parse()
        .expect("TIMEZONE が無効です（例: Asia/Tokyo）");
    time::init_timezone(tz);
    println!("タイムゾーン: {}", tz);

    // 二重起動すると1回のコマンドが2回実行される（宿題が2件登録されるなど）ので防ぐ
    let _instance_lock = match acquire_instance_lock(&db_path) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let database = db::Database::new(&db_path).expect("データベースのオープンに失敗しました");
    database
        .init()
        .expect("データベーススキーマの初期化に失敗しました");
    let db_for_reminder = database.clone();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                commands::task(),
                commands::hw(),
                commands::now(),
                commands::help(),
            ],
            on_error: |err| {
                Box::pin(async move {
                    match err {
                        poise::FrameworkError::Command { error, ctx, .. } => {
                            eprintln!("コマンドエラー: {:?}", error);
                            let _ = ctx
                                .send(
                                    poise::CreateReply::default()
                                        .ephemeral(true)
                                        .content(format!("エラーが発生しました: {}", error)),
                                )
                                .await;
                        }
                        err => {
                            if let Err(e) = poise::builtins::on_error(err).await {
                                eprintln!("エラーハンドリング中のエラー: {:?}", e);
                            }
                        }
                    }
                })
            },
            // どのコマンドがいつ・何回届いたかを追えるようにログに残す
            // （同じ ID が2回出れば二重処理、別の ID なら Discord から2回届いている）
            pre_command: |ctx| {
                Box::pin(async move {
                    println!(
                        "コマンド受信: /{} by {} (interaction {})",
                        ctx.command().qualified_name,
                        ctx.author().id,
                        ctx.id()
                    );
                })
            },
            // 完了ボタンはメッセージが古くても・ボット再起動後でも押せるよう、イベントで受け取る
            event_handler: |ctx, event, _framework, data| {
                Box::pin(async move {
                    if let serenity::FullEvent::InteractionCreate {
                        interaction: serenity::Interaction::Component(component),
                    } = event
                        && let Err(e) = commands::handle_component(ctx, data, component).await
                    {
                        eprintln!("ボタン処理エラー: {:?}", e);
                    }
                    Ok(())
                })
            },
            ..Default::default()
        })
        .setup(|ctx, ready, framework| {
            Box::pin(async move {
                println!("ボットが起動しました: {}", ready.user.name);
                let commands = &framework.options().commands;
                if let Ok(guild_id_str) = std::env::var("GUILD_ID") {
                    let guild_id = guild_id_str
                        .parse::<u64>()
                        .expect("GUILD_ID が無効な数値です");
                    poise::builtins::register_globally::<Data, Error>(ctx, &[]).await?;
                    poise::builtins::register_in_guild(
                        ctx,
                        commands,
                        serenity::GuildId::new(guild_id),
                    )
                    .await?;
                    println!("コマンドをギルド {} に登録しました（即時反映）", guild_id);
                } else {
                    poise::builtins::register_globally(ctx, commands).await?;
                    println!("コマンドをグローバルに登録しました（反映まで最大1時間）");
                }
                Ok(Data { db: database })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::non_privileged();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .expect("Discord クライアントの作成に失敗しました");

    // リマインダー・期限切れ通知のバックグラウンドタスク（60秒ごとにチェック）
    tokio::spawn(reminder::run(db_for_reminder, Arc::clone(&client.http)));

    client.start().await.expect("ボットの起動に失敗しました");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_is_rejected() {
        let dir = std::env::temp_dir().join(format!("task_bot_lock_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("tasks.db").to_string_lossy().to_string();

        let first = acquire_instance_lock(&db_path).expect("1つ目は起動できる");
        let err = acquire_instance_lock(&db_path).expect_err("2つ目は拒否される");
        assert!(err.contains("既に起動しています"));

        // 1つ目が終了すれば起動できる
        drop(first);
        assert!(acquire_instance_lock(&db_path).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }
}
