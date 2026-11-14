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

/// CLI などから使う API トークン（トークン自体は保存せず、SHA-256 のハッシュだけを持つ）
#[derive(Debug, Clone, PartialEq)]
pub struct ApiToken {
    pub id: i64,
    pub user_id: String,
    pub username: String,
    pub avatar: Option<String>,
    /// 発行時のサーバーの一覧（JSON）。使うときに今もメンバーか確かめる
    pub guilds_json: String,
    /// 自分で付けた名前（例: ノート PC）
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// 1人が持てるトークンの数
pub const MAX_API_TOKENS: i64 = 10;

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS web_sessions (
            id          TEXT PRIMARY KEY,
            user_id     TEXT NOT NULL,
            username    TEXT NOT NULL,
            avatar      TEXT,
            guilds_json TEXT NOT NULL,
            expires_at  INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS api_tokens (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id      TEXT NOT NULL,
            username     TEXT NOT NULL,
            avatar       TEXT,
            guilds_json  TEXT NOT NULL,
            name         TEXT NOT NULL,
            token_hash   TEXT NOT NULL UNIQUE,
            created_at   INTEGER NOT NULL,
            last_used_at INTEGER
        );",
    )
}

fn row_to_token(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiToken> {
    Ok(ApiToken {
        id: row.get(0)?,
        user_id: row.get(1)?,
        username: row.get(2)?,
        avatar: row.get(3)?,
        guilds_json: row.get(4)?,
        name: row.get(5)?,
        created_at: row.get(6)?,
        last_used_at: row.get(7)?,
    })
}

const TOKEN_COLUMNS: &str =
    "id, user_id, username, avatar, guilds_json, name, created_at, last_used_at";

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

    /// API トークンを登録する（owner の id 以外の項目を使う）。持てる数（MAX_API_TOKENS）を超えるなら None
    pub async fn api_token_create(
        &self,
        owner: ApiToken,
        token_hash: String,
    ) -> Result<Option<ApiToken>, crate::Error> {
        let session = owner;
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM api_tokens WHERE user_id = ?1",
                params![session.user_id],
                |row| row.get(0),
            )?;
            if count >= MAX_API_TOKENS {
                return Ok(None);
            }
            tx.execute(
                "INSERT INTO api_tokens (user_id, username, avatar, guilds_json, name, token_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    session.user_id,
                    session.username,
                    session.avatar,
                    session.guilds_json,
                    session.name,
                    token_hash,
                    chrono::Utc::now().timestamp()
                ],
            )?;
            let id = tx.last_insert_rowid();
            let token = tx.query_row(
                &format!("SELECT {TOKEN_COLUMNS} FROM api_tokens WHERE id = ?1"),
                params![id],
                row_to_token,
            )?;
            tx.commit()?;
            Ok(Some(token))
        })
        .await
    }

    /// ハッシュからトークンを探し、最後に使った時刻を記録する
    pub async fn api_token_use(
        &self,
        token_hash: String,
    ) -> Result<Option<ApiToken>, crate::Error> {
        self.call(move |conn| {
            let now = chrono::Utc::now().timestamp();
            let token = conn
                .query_row(
                    &format!("SELECT {TOKEN_COLUMNS} FROM api_tokens WHERE token_hash = ?1"),
                    params![token_hash],
                    row_to_token,
                )
                .optional()?;
            if let Some(t) = &token {
                conn.execute(
                    "UPDATE api_tokens SET last_used_at = ?1 WHERE id = ?2",
                    params![now, t.id],
                )?;
            }
            Ok(token)
        })
        .await
    }

    /// 自分のトークン（新しい順）
    pub async fn api_token_list(&self, user_id: String) -> Result<Vec<ApiToken>, crate::Error> {
        self.call(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {TOKEN_COLUMNS} FROM api_tokens WHERE user_id = ?1 ORDER BY id DESC"
            ))?;
            let rows = stmt.query_map(params![user_id], row_to_token)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// 自分のトークンを削除する（他人のものは消せない）
    pub async fn api_token_delete(&self, user_id: String, id: i64) -> Result<bool, crate::Error> {
        self.call(move |conn| {
            let n = conn.execute(
                "DELETE FROM api_tokens WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
            )?;
            Ok(n > 0)
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

    fn owner(user: &str, name: &str) -> ApiToken {
        ApiToken {
            id: 0,
            user_id: user.into(),
            username: "user".into(),
            avatar: None,
            guilds_json: "[]".into(),
            name: name.into(),
            created_at: 0,
            last_used_at: None,
        }
    }

    #[tokio::test]
    async fn api_tokens_are_found_by_hash_and_owned() {
        let db = Database::new(":memory:").unwrap();
        db.init().unwrap();

        let t = db
            .api_token_create(owner("1", "ノート PC"), "hash-a".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.name, "ノート PC");
        assert_eq!(t.last_used_at, None);

        // ハッシュで見つかり、使った時刻が残る
        let used = db.api_token_use("hash-a".into()).await.unwrap().unwrap();
        assert_eq!(used.id, t.id);
        let listed = db.api_token_list("1".into()).await.unwrap();
        assert!(listed[0].last_used_at.is_some());
        assert!(db.api_token_use("hash-x".into()).await.unwrap().is_none());

        // 他人は消せない。本人が消したら使えない
        assert!(!db.api_token_delete("2".into(), t.id).await.unwrap());
        assert!(db.api_token_delete("1".into(), t.id).await.unwrap());
        assert!(db.api_token_use("hash-a".into()).await.unwrap().is_none());

        // 1人 MAX_API_TOKENS 個まで
        for i in 0..MAX_API_TOKENS {
            assert!(
                db.api_token_create(owner("1", "x"), format!("h{i}"))
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            db.api_token_create(owner("1", "x"), "over".into())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.api_token_create(owner("2", "x"), "other".into())
                .await
                .unwrap()
                .is_some()
        );
    }
}
