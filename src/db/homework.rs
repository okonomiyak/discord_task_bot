use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use super::Database;
use crate::models::{
    Homework, HwKind, HwQuery, HwRepeat, HwSettings, HwSettingsPatch, NewHomework,
};

/// 宿題取得用の共通カラム（row_to_homework と順序を合わせること）
const HW_COLUMNS: &str = "h.id, h.guild_id, h.channel_id, h.created_by, h.subject, h.title,
    h.description, h.due_date, h.created_at,
    (SELECT GROUP_CONCAT(p.user_id) FROM hw_progress p WHERE p.homework_id = h.id),
    h.kind, h.repeat_id";

const REPEAT_COLUMNS: &str =
    "id, guild_id, channel_id, created_by, subject, title, description, weekday, time, last_due";

/// 宿題の変更項目（None は変更しない）
#[derive(Debug, Default)]
pub struct HwEdit {
    pub subject: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// 正規化済みの期限
    pub due_date: Option<String>,
}

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS homework (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id    TEXT NOT NULL,
            channel_id  TEXT NOT NULL,
            created_by  TEXT NOT NULL,
            subject     TEXT NOT NULL,
            title       TEXT NOT NULL,
            description TEXT,
            due_date    TEXT NOT NULL,
            created_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_homework_guild_due ON homework (guild_id, due_date);

        -- 人ごとの完了状態
        CREATE TABLE IF NOT EXISTS hw_progress (
            homework_id INTEGER NOT NULL,
            user_id     TEXT NOT NULL,
            done_at     INTEGER NOT NULL,
            PRIMARY KEY (homework_id, user_id)
        );

        -- サーバーごとの設定（remind_before = 0 で事前通知オフ、summary_time NULL でまとめ投稿オフ）
        CREATE TABLE IF NOT EXISTS hw_settings (
            guild_id          TEXT PRIMARY KEY,
            channel_id        TEXT,
            remind_before     INTEGER NOT NULL,
            summary_time      TEXT,
            default_time      TEXT NOT NULL,
            last_summary_date TEXT
        );

        -- 個人の DM 通知設定
        CREATE TABLE IF NOT EXISTS hw_dm_settings (
            guild_id      TEXT NOT NULL,
            user_id       TEXT NOT NULL,
            remind_before INTEGER NOT NULL,
            PRIMARY KEY (guild_id, user_id)
        );

        -- 送信済みの通知（target は 'channel' またはユーザー ID）
        CREATE TABLE IF NOT EXISTS hw_sent (
            homework_id   INTEGER NOT NULL,
            target        TEXT NOT NULL,
            remind_before INTEGER NOT NULL,
            PRIMARY KEY (homework_id, target, remind_before)
        );

        -- 毎週の宿題（last_due は最後に自動登録した宿題の期限）
        CREATE TABLE IF NOT EXISTS hw_repeats (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id    TEXT NOT NULL,
            channel_id  TEXT NOT NULL,
            created_by  TEXT NOT NULL,
            subject     TEXT NOT NULL,
            title       TEXT NOT NULL,
            description TEXT,
            weekday     INTEGER NOT NULL,
            time        TEXT NOT NULL,
            last_due    TEXT
        );",
    )?;

    // 既存DBへの互換マイグレーション
    let _ = conn.execute(
        "ALTER TABLE homework ADD COLUMN kind TEXT NOT NULL DEFAULT 'homework'",
        [],
    );
    let _ = conn.execute("ALTER TABLE homework ADD COLUMN repeat_id INTEGER", []);
    Ok(())
}

fn insert(conn: &Connection, hw: &NewHomework) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO homework
            (kind, guild_id, channel_id, created_by, subject, title, description,
             due_date, created_at, repeat_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            hw.kind.as_str(),
            hw.guild_id,
            hw.channel_id,
            hw.created_by,
            hw.subject,
            hw.title,
            hw.description,
            hw.due_date,
            chrono::Utc::now().timestamp(),
            hw.repeat_id
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

impl Database {
    pub async fn hw_add(&self, hw: NewHomework) -> Result<i64, crate::Error> {
        self.call(move |conn| Ok(insert(conn, &hw)?)).await
    }

    pub async fn hw_get(
        &self,
        id: i64,
        guild_id: String,
    ) -> Result<Option<Homework>, crate::Error> {
        self.call(move |conn| Ok(get(conn, id, &guild_id)?)).await
    }

    pub async fn hw_list(
        &self,
        guild_id: String,
        query: HwQuery,
    ) -> Result<Vec<Homework>, crate::Error> {
        self.call(move |conn| {
            let mut sql = format!("SELECT {HW_COLUMNS} FROM homework h WHERE h.guild_id = ?1");
            let mut args = vec![guild_id];

            if let Some(kind) = query.kind {
                args.push(kind.as_str().to_string());
                sql += &format!(" AND h.kind = ?{}", args.len());
            }
            if let Some(subject) = query.subject {
                args.push(subject);
                sql += &format!(" AND h.subject = ?{}", args.len());
            }
            if let Some(from) = query.due_from {
                args.push(from);
                sql += &format!(" AND h.due_date >= ?{}", args.len());
            }
            if let Some(until) = query.due_until {
                args.push(until);
                sql += &format!(" AND h.due_date < ?{}", args.len());
            }
            let progress = "SELECT 1 FROM hw_progress p WHERE p.homework_id = h.id AND p.user_id";
            if let Some(user_id) = query.not_done_by {
                args.push(user_id);
                sql += &format!(" AND NOT EXISTS ({progress} = ?{})", args.len());
            }
            if let Some(user_id) = query.done_by {
                args.push(user_id);
                sql += &format!(" AND EXISTS ({progress} = ?{})", args.len());
            }
            sql += if query.newest_first {
                " ORDER BY h.due_date DESC, h.id DESC"
            } else {
                " ORDER BY h.due_date, h.id"
            };

            let list = conn
                .prepare(&sql)?
                .query_map(params_from_iter(args), row_to_homework)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(list)
        })
        .await
    }

    /// 自分の完了状態を切り替える。変化があれば true（宿題が存在しなければ None）
    pub async fn hw_set_done(
        &self,
        id: i64,
        guild_id: String,
        user_id: String,
        done: bool,
    ) -> Result<Option<bool>, crate::Error> {
        let now = chrono::Utc::now().timestamp();
        self.call(move |conn| {
            if get(conn, id, &guild_id)?.is_none() {
                return Ok(None);
            }
            let changed = if done {
                conn.execute(
                    "INSERT OR IGNORE INTO hw_progress (homework_id, user_id, done_at) VALUES (?1, ?2, ?3)",
                    params![id, user_id, now],
                )?
            } else {
                conn.execute(
                    "DELETE FROM hw_progress WHERE homework_id = ?1 AND user_id = ?2",
                    params![id, user_id],
                )?
            };
            Ok(Some(changed > 0))
        })
        .await
    }

    /// 宿題を編集する。期限が変わったら送信済み通知をリセットする
    pub async fn hw_edit(
        &self,
        id: i64,
        guild_id: String,
        edit: HwEdit,
    ) -> Result<Option<Homework>, crate::Error> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let Some(cur) = get(&tx, id, &guild_id)? else {
                return Ok(None);
            };
            let due_changed = edit.due_date.as_ref().is_some_and(|d| *d != cur.due_date);
            tx.execute(
                "UPDATE homework SET subject = ?1, title = ?2, description = ?3, due_date = ?4
                 WHERE id = ?5",
                params![
                    edit.subject.unwrap_or(cur.subject),
                    edit.title.unwrap_or(cur.title),
                    edit.description.or(cur.description),
                    edit.due_date.unwrap_or(cur.due_date),
                    id
                ],
            )?;
            if due_changed {
                tx.execute("DELETE FROM hw_sent WHERE homework_id = ?1", params![id])?;
            }
            let updated = get(&tx, id, &guild_id)?;
            tx.commit()?;
            Ok(updated)
        })
        .await
    }

    pub async fn hw_delete(
        &self,
        id: i64,
        guild_id: String,
    ) -> Result<Option<Homework>, crate::Error> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let Some(hw) = get(&tx, id, &guild_id)? else {
                return Ok(None);
            };
            tx.execute(
                "DELETE FROM hw_progress WHERE homework_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM hw_sent WHERE homework_id = ?1", params![id])?;
            tx.execute("DELETE FROM homework WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(Some(hw))
        })
        .await
    }

    /// サーバーで使われている科目（入力補完用、よく使う順）
    pub async fn hw_subjects(&self, guild_id: String) -> Result<Vec<String>, crate::Error> {
        self.call(move |conn| {
            let subjects = conn
                .prepare(
                    "SELECT subject FROM homework WHERE guild_id = ?1
                     GROUP BY subject ORDER BY COUNT(*) DESC, MAX(id) DESC LIMIT 100",
                )?
                .query_map(params![guild_id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(subjects)
        })
        .await
    }

    pub async fn hw_settings(&self, guild_id: String) -> Result<HwSettings, crate::Error> {
        self.call(move |conn| Ok(settings(conn, &guild_id)?)).await
    }

    pub async fn hw_update_settings(
        &self,
        guild_id: String,
        patch: HwSettingsPatch,
    ) -> Result<HwSettings, crate::Error> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let mut s = settings(&tx, &guild_id)?;
            if let Some(channel_id) = patch.channel_id {
                s.channel_id = Some(channel_id);
            }
            if let Some(remind_before) = patch.remind_before {
                s.remind_before = remind_before;
            }
            if let Some(summary_time) = patch.summary_time {
                s.summary_time = summary_time;
            }
            if let Some(default_time) = patch.default_time {
                s.default_time = default_time;
            }
            save_settings(&tx, &s)?;
            tx.commit()?;
            Ok(s)
        })
        .await
    }

    /// まとめ投稿が有効なサーバーの設定
    pub async fn hw_summary_settings(&self) -> Result<Vec<HwSettings>, crate::Error> {
        self.call(|conn| {
            let list = conn
                .prepare(&format!(
                    "SELECT {SETTINGS_COLUMNS} FROM hw_settings WHERE summary_time IS NOT NULL"
                ))?
                .query_map([], row_to_settings)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(list)
        })
        .await
    }

    pub async fn hw_set_last_summary_date(
        &self,
        guild_id: String,
        date: String,
    ) -> Result<(), crate::Error> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE hw_settings SET last_summary_date = ?1 WHERE guild_id = ?2",
                params![date, guild_id],
            )?;
            Ok(())
        })
        .await
    }

    /// 個人の DM 通知設定（None でオフ）
    pub async fn hw_set_dm(
        &self,
        guild_id: String,
        user_id: String,
        remind_before: Option<i64>,
    ) -> Result<(), crate::Error> {
        self.call(move |conn| {
            match remind_before {
                Some(secs) => conn.execute(
                    "INSERT INTO hw_dm_settings (guild_id, user_id, remind_before) VALUES (?1, ?2, ?3)
                     ON CONFLICT (guild_id, user_id) DO UPDATE SET remind_before = excluded.remind_before",
                    params![guild_id, user_id, secs],
                )?,
                None => conn.execute(
                    "DELETE FROM hw_dm_settings WHERE guild_id = ?1 AND user_id = ?2",
                    params![guild_id, user_id],
                )?,
            };
            Ok(())
        })
        .await
    }

    pub async fn hw_get_dm(
        &self,
        guild_id: String,
        user_id: String,
    ) -> Result<Option<i64>, crate::Error> {
        self.call(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT remind_before FROM hw_dm_settings WHERE guild_id = ?1 AND user_id = ?2",
                    params![guild_id, user_id],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// DM 通知を設定しているユーザー (user_id, remind_before)
    pub async fn hw_dm_subscribers(
        &self,
        guild_id: String,
    ) -> Result<Vec<(String, i64)>, crate::Error> {
        self.call(move |conn| {
            let list = conn
                .prepare("SELECT user_id, remind_before FROM hw_dm_settings WHERE guild_id = ?1")?
                .query_map(params![guild_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(list)
        })
        .await
    }

    /// 期限が (from, until] の宿題（全サーバー、通知チェック用）
    pub async fn hw_due_between(
        &self,
        from: String,
        until: String,
    ) -> Result<Vec<Homework>, crate::Error> {
        self.call(move |conn| {
            let list = conn
                .prepare(&format!(
                    "SELECT {HW_COLUMNS} FROM homework h
                     WHERE h.due_date > ?1 AND h.due_date <= ?2
                     ORDER BY h.due_date"
                ))?
                .query_map(params![from, until], row_to_homework)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(list)
        })
        .await
    }

    pub async fn hw_repeat_add(&self, repeat: HwRepeat) -> Result<i64, crate::Error> {
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO hw_repeats
                    (guild_id, channel_id, created_by, subject, title, description, weekday, time)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    repeat.guild_id,
                    repeat.channel_id,
                    repeat.created_by,
                    repeat.subject,
                    repeat.title,
                    repeat.description,
                    repeat.weekday,
                    repeat.time
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    /// 毎週の宿題の設定一覧（guild_id が None なら全サーバー）
    pub async fn hw_repeats(
        &self,
        guild_id: Option<String>,
    ) -> Result<Vec<HwRepeat>, crate::Error> {
        self.call(move |conn| {
            let list = match guild_id {
                Some(g) => conn
                    .prepare(&format!(
                        "SELECT {REPEAT_COLUMNS} FROM hw_repeats WHERE guild_id = ?1
                         ORDER BY weekday, time, id"
                    ))?
                    .query_map(params![g], row_to_repeat)?
                    .collect::<Result<Vec<_>, _>>()?,
                None => conn
                    .prepare(&format!("SELECT {REPEAT_COLUMNS} FROM hw_repeats"))?
                    .query_map([], row_to_repeat)?
                    .collect::<Result<Vec<_>, _>>()?,
            };
            Ok(list)
        })
        .await
    }

    pub async fn hw_repeat_get(
        &self,
        id: i64,
        guild_id: String,
    ) -> Result<Option<HwRepeat>, crate::Error> {
        self.call(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {REPEAT_COLUMNS} FROM hw_repeats WHERE id = ?1 AND guild_id = ?2"
                    ),
                    params![id, guild_id],
                    row_to_repeat,
                )
                .optional()?)
        })
        .await
    }

    /// 毎週の宿題の設定を削除する（登録済みの宿題は残す）
    pub async fn hw_repeat_delete(&self, id: i64, guild_id: String) -> Result<bool, crate::Error> {
        self.call(move |conn| {
            Ok(conn.execute(
                "DELETE FROM hw_repeats WHERE id = ?1 AND guild_id = ?2",
                params![id, guild_id],
            )? > 0)
        })
        .await
    }

    /// 毎週の宿題から、指定の期限の宿題を登録する。
    /// 既に同じかそれ以降の期限を登録済みなら何もしない（None）
    pub async fn hw_repeat_generate(
        &self,
        repeat_id: i64,
        due_date: String,
    ) -> Result<Option<i64>, crate::Error> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let Some(r) = tx
                .query_row(
                    &format!("SELECT {REPEAT_COLUMNS} FROM hw_repeats WHERE id = ?1"),
                    params![repeat_id],
                    row_to_repeat,
                )
                .optional()?
            else {
                return Ok(None);
            };
            if r.last_due.as_ref().is_some_and(|last| *last >= due_date) {
                return Ok(None);
            }
            let id = insert(
                &tx,
                &NewHomework {
                    kind: HwKind::Homework,
                    guild_id: r.guild_id,
                    channel_id: r.channel_id,
                    created_by: r.created_by,
                    subject: r.subject,
                    title: r.title,
                    description: r.description,
                    due_date: due_date.clone(),
                    repeat_id: Some(repeat_id),
                },
            )?;
            tx.execute(
                "UPDATE hw_repeats SET last_due = ?1 WHERE id = ?2",
                params![due_date, repeat_id],
            )?;
            tx.commit()?;
            Ok(Some(id))
        })
        .await
    }

    /// 通知を送信済みとして記録する。まだ記録がなければ true（＝今回送るべき）
    pub async fn hw_claim_notification(
        &self,
        homework_id: i64,
        target: String,
        remind_before: i64,
    ) -> Result<bool, crate::Error> {
        self.call(move |conn| {
            let inserted = conn.execute(
                "INSERT OR IGNORE INTO hw_sent (homework_id, target, remind_before) VALUES (?1, ?2, ?3)",
                params![homework_id, target, remind_before],
            )?;
            Ok(inserted > 0)
        })
        .await
    }
}

const SETTINGS_COLUMNS: &str =
    "guild_id, channel_id, remind_before, summary_time, default_time, last_summary_date";

fn settings(conn: &Connection, guild_id: &str) -> rusqlite::Result<HwSettings> {
    Ok(conn
        .query_row(
            &format!("SELECT {SETTINGS_COLUMNS} FROM hw_settings WHERE guild_id = ?1"),
            params![guild_id],
            row_to_settings,
        )
        .optional()?
        .unwrap_or_else(|| HwSettings::default_for(guild_id.to_string())))
}

fn save_settings(conn: &Connection, s: &HwSettings) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO hw_settings (guild_id, channel_id, remind_before, summary_time, default_time, last_summary_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (guild_id) DO UPDATE SET
            channel_id = excluded.channel_id,
            remind_before = excluded.remind_before,
            summary_time = excluded.summary_time,
            default_time = excluded.default_time,
            last_summary_date = excluded.last_summary_date",
        params![
            s.guild_id,
            s.channel_id,
            s.remind_before.unwrap_or(0),
            s.summary_time,
            s.default_time,
            s.last_summary_date
        ],
    )?;
    Ok(())
}

fn row_to_settings(row: &rusqlite::Row<'_>) -> rusqlite::Result<HwSettings> {
    let remind_before: i64 = row.get(2)?;
    Ok(HwSettings {
        guild_id: row.get(0)?,
        channel_id: row.get(1)?,
        remind_before: (remind_before > 0).then_some(remind_before),
        summary_time: row.get(3)?,
        default_time: row.get(4)?,
        last_summary_date: row.get(5)?,
    })
}

fn row_to_repeat(row: &rusqlite::Row<'_>) -> rusqlite::Result<HwRepeat> {
    Ok(HwRepeat {
        id: row.get(0)?,
        guild_id: row.get(1)?,
        channel_id: row.get(2)?,
        created_by: row.get(3)?,
        subject: row.get(4)?,
        title: row.get(5)?,
        description: row.get(6)?,
        weekday: row.get(7)?,
        time: row.get(8)?,
        last_due: row.get(9)?,
    })
}

fn get(conn: &Connection, id: i64, guild_id: &str) -> rusqlite::Result<Option<Homework>> {
    conn.query_row(
        &format!("SELECT {HW_COLUMNS} FROM homework h WHERE h.id = ?1 AND h.guild_id = ?2"),
        params![id, guild_id],
        row_to_homework,
    )
    .optional()
}

fn row_to_homework(row: &rusqlite::Row<'_>) -> rusqlite::Result<Homework> {
    let done_by: Option<String> = row.get(9)?;
    Ok(Homework {
        id: row.get(0)?,
        guild_id: row.get(1)?,
        channel_id: row.get(2)?,
        created_by: row.get(3)?,
        subject: row.get(4)?,
        title: row.get(5)?,
        description: row.get(6)?,
        due_date: row.get(7)?,
        created_at: row.get(8)?,
        done_by: done_by
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        kind: HwKind::from_str(&row.get::<_, String>(10)?),
        repeat_id: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let db = Database::new(":memory:").unwrap();
        db.init().unwrap();
        db
    }

    fn new_hw(kind: HwKind, guild: &str, subject: &str, due: &str) -> NewHomework {
        NewHomework {
            kind,
            guild_id: guild.into(),
            channel_id: "10".into(),
            created_by: "1".into(),
            subject: subject.into(),
            title: format!("{subject}の宿題"),
            description: None,
            due_date: due.into(),
            repeat_id: None,
        }
    }

    async fn add(db: &Database, guild: &str, subject: &str, due: &str) -> i64 {
        db.hw_add(new_hw(HwKind::Homework, guild, subject, due))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn filters_by_kind() {
        let db = test_db();
        add(&db, "G", "数学", "2999-01-01 08:30").await;
        db.hw_add(new_hw(HwKind::Exam, "G", "英語", "2999-01-02 08:30"))
            .await
            .unwrap();

        let all = db.hw_list("G".into(), HwQuery::default()).await.unwrap();
        assert_eq!(all.len(), 2);
        let exams = db
            .hw_list(
                "G".into(),
                HwQuery {
                    kind: Some(HwKind::Exam),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(exams.len(), 1);
        assert_eq!(exams[0].kind, HwKind::Exam);
        assert_eq!(exams[0].label(), "📝 英語｜英語の宿題");
    }

    #[tokio::test]
    async fn repeat_generates_each_due_once() {
        let db = test_db();
        let rid = db
            .hw_repeat_add(HwRepeat {
                id: 0,
                guild_id: "G".into(),
                channel_id: "10".into(),
                created_by: "1".into(),
                subject: "英語".into(),
                title: "単語テスト".into(),
                description: None,
                weekday: 0,
                time: "08:30".into(),
                last_due: None,
            })
            .await
            .unwrap();

        let first = db
            .hw_repeat_generate(rid, "2999-01-06 08:30".into())
            .await
            .unwrap();
        assert!(first.is_some());
        // 同じ期限・過去の期限では登録しない
        assert!(
            db.hw_repeat_generate(rid, "2999-01-06 08:30".into())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.hw_repeat_generate(rid, "2998-12-30 08:30".into())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.hw_repeat_generate(rid, "2999-01-13 08:30".into())
                .await
                .unwrap()
                .is_some()
        );

        let list = db.hw_list("G".into(), HwQuery::default()).await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .all(|h| h.repeat_id == Some(rid) && h.title == "単語テスト")
        );

        assert!(db.hw_repeat_get(rid, "X".into()).await.unwrap().is_none());
        assert!(!db.hw_repeat_delete(rid, "X".into()).await.unwrap());
        assert!(db.hw_repeat_delete(rid, "G".into()).await.unwrap());
        assert!(
            db.hw_repeat_generate(rid, "2999-01-20 08:30".into())
                .await
                .unwrap()
                .is_none()
        );
        // 設定を消しても登録済みの宿題は残る
        assert_eq!(
            db.hw_list("G".into(), HwQuery::default())
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn progress_is_per_user() {
        let db = test_db();
        let id = add(&db, "G", "数学", "2999-01-01 08:30").await;

        assert_eq!(
            db.hw_set_done(id, "G".into(), "A".into(), true)
                .await
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            db.hw_set_done(id, "G".into(), "A".into(), true)
                .await
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            db.hw_set_done(id, "X".into(), "A".into(), true)
                .await
                .unwrap(),
            None
        );

        let hw = db.hw_get(id, "G".into()).await.unwrap().unwrap();
        assert!(hw.is_done_by("A"));
        assert!(!hw.is_done_by("B"));

        let todo_a = db
            .hw_list(
                "G".into(),
                HwQuery {
                    not_done_by: Some("A".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let todo_b = db
            .hw_list(
                "G".into(),
                HwQuery {
                    not_done_by: Some("B".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(todo_a.is_empty());
        assert_eq!(todo_b.len(), 1);

        assert_eq!(
            db.hw_set_done(id, "G".into(), "A".into(), false)
                .await
                .unwrap(),
            Some(true)
        );
        let hw = db.hw_get(id, "G".into()).await.unwrap().unwrap();
        assert!(hw.done_by.is_empty());
    }

    #[tokio::test]
    async fn list_filters_by_subject_and_range() {
        let db = test_db();
        add(&db, "G", "数学", "2999-01-03 08:30").await;
        add(&db, "G", "英語", "2999-01-01 08:30").await;
        add(&db, "G", "数学", "2999-01-02 08:30").await;
        add(&db, "H", "数学", "2999-01-02 08:30").await;

        let math = db
            .hw_list(
                "G".into(),
                HwQuery {
                    subject: Some("数学".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let dues: Vec<_> = math.iter().map(|h| h.due_date.as_str()).collect();
        assert_eq!(dues, ["2999-01-02 08:30", "2999-01-03 08:30"]);

        let range = db
            .hw_list(
                "G".into(),
                HwQuery {
                    due_from: Some("2999-01-02 00:00".into()),
                    due_until: Some("2999-01-03 00:00".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(range.len(), 1);

        assert_eq!(db.hw_subjects("G".into()).await.unwrap(), ["数学", "英語"]);
    }

    #[tokio::test]
    async fn settings_roundtrip() {
        let db = test_db();
        let s = db.hw_settings("G".into()).await.unwrap();
        assert_eq!(s, HwSettings::default_for("G".into()));

        let s = db
            .hw_update_settings(
                "G".into(),
                HwSettingsPatch {
                    channel_id: Some("99".into()),
                    remind_before: Some(None),
                    summary_time: Some(Some("21:00".into())),
                    default_time: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(s.remind_before, None);
        assert_eq!(db.hw_settings("G".into()).await.unwrap(), s);
        assert_eq!(db.hw_summary_settings().await.unwrap(), vec![s]);
    }

    #[tokio::test]
    async fn notifications_are_claimed_once_and_reset_on_due_change() {
        let db = test_db();
        let id = add(&db, "G", "数学", "2999-01-01 08:30").await;

        assert!(
            db.hw_claim_notification(id, "channel".into(), 3600)
                .await
                .unwrap()
        );
        assert!(
            !db.hw_claim_notification(id, "channel".into(), 3600)
                .await
                .unwrap()
        );

        let edit = HwEdit {
            due_date: Some("2999-01-02 08:30".into()),
            ..Default::default()
        };
        db.hw_edit(id, "G".into(), edit).await.unwrap().unwrap();
        assert!(
            db.hw_claim_notification(id, "channel".into(), 3600)
                .await
                .unwrap()
        );

        // 期限以外の変更ではリセットしない
        let edit = HwEdit {
            title: Some("新".into()),
            ..Default::default()
        };
        db.hw_edit(id, "G".into(), edit).await.unwrap().unwrap();
        assert!(
            !db.hw_claim_notification(id, "channel".into(), 3600)
                .await
                .unwrap()
        );

        assert!(db.hw_delete(id, "G".into()).await.unwrap().is_some());
        assert!(db.hw_get(id, "G".into()).await.unwrap().is_none());
    }
}
