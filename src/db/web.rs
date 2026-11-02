use rusqlite::{Connection, OptionalExtension, params};

use super::Database;

/// Web ダッシュボードのログインセッション
#[derive(Debug, Clone, PartialEq)]
pub struct WebSession {
    pub id: String,
    pub user_id: String,
    pub username: String,
    pub avatar: Option<String>,
    /// ログイン時に取得した、ボットもいるサーバーの一覧（JSON）
    pub guilds_json: String,
    /// 有効期限（UNIX 秒）
    pub expires_at: i64,
}

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS web_sessions (
            id          TEXT PRIMARY KEY,
            user_id     TEXT NOT NULL,
            username    TEXT NOT NULL,
            avatar      TEXT,
            guilds_json TEXT NOT NULL,
            expires_at  INTEGER NOT NULL
        );",
    )
}

impl Database {
    pub async fn web_session_create(&self, session: WebSession) -> Result<(), crate::Error> {
        self.call(move |conn| {
            // ついでに期限切れのセッションを掃除する
            conn.execute(
                "DELETE FROM web_sessions WHERE expires_at < ?1",
                params![chrono::Utc::now().timestamp()],
            )?;
            conn.execute(
                "INSERT INTO web_sessions (id, user_id, username, avatar, guilds_json, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session.id,
                    session.user_id,
                    session.username,
                    session.avatar,
                    session.guilds_json,
                    session.expires_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// 有効なセッションを取得する（期限切れなら None）
    pub async fn web_session_get(&self, id: String) -> Result<Option<WebSession>, crate::Error> {
        self.call(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT id, user_id, username, avatar, guilds_json, expires_at
                     FROM web_sessions WHERE id = ?1 AND expires_at >= ?2",
                    params![id, chrono::Utc::now().timestamp()],
                    |row| {
                        Ok(WebSession {
                            id: row.get(0)?,
                            user_id: row.get(1)?,
                            username: row.get(2)?,
                            avatar: row.get(3)?,
                            guilds_json: row.get(4)?,
                            expires_at: row.get(5)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    pub async fn web_session_delete(&self, id: String) -> Result<(), crate::Error> {
        self.call(move |conn| {
            conn.execute("DELETE FROM web_sessions WHERE id = ?1", params![id])?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, expires_at: i64) -> WebSession {
        WebSession {
            id: id.into(),
            user_id: "1".into(),
            username: "user".into(),
            avatar: None,
            guilds_json: "[]".into(),
            expires_at,
        }
    }

    #[tokio::test]
    async fn sessions_expire_and_can_be_deleted() {
        let db = Database::new(":memory:").unwrap();
        db.init().unwrap();
        let now = chrono::Utc::now().timestamp();

        db.web_session_create(session("old", now - 10))
            .await
            .unwrap();
        db.web_session_create(session("live", now + 3600))
            .await
            .unwrap();

        assert!(db.web_session_get("old".into()).await.unwrap().is_none());
        assert_eq!(
            db.web_session_get("live".into()).await.unwrap(),
            Some(session("live", now + 3600))
        );

        db.web_session_delete("live".into()).await.unwrap();
        assert!(db.web_session_get("live".into()).await.unwrap().is_none());
    }
}
