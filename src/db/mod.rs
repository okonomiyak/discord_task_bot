use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::sync::{Arc, Mutex};

mod homework;

pub use homework::HwEdit;

use crate::models::{PendingReminder, Priority, Task, TaskQuery, TaskSort, TaskStatus};
use crate::time;

/// タスク取得用の共通カラム（row_to_task と順序を合わせること）
const TASK_COLUMNS: &str = "t.id, t.user_id, t.guild_id, t.title, t.description,
    t.status, t.priority, t.due_date, t.created_at, t.channel_id,
    (SELECT GROUP_CONCAT(r.remind_before || ':' || r.reminded)
       FROM reminders r WHERE r.task_id = t.id),
    t.discord_event_id,
    (SELECT GROUP_CONCAT(a.user_id) FROM assignees a WHERE a.task_id = t.id)";
const TASK_COLUMN_COUNT: usize = 13;

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

/// `/task edit` で変更する項目（None は変更しない）
#[derive(Debug, Default)]
pub struct TaskEdit {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<Priority>,
    /// 正規化済みの期限
    pub due_date: Option<String>,
    /// Some(vec) でリマインダーを全置き換え（空 vec = 全削除）
    pub reminders: Option<Vec<i64>>,
}

impl Database {
    pub fn new(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn init(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id          TEXT NOT NULL,
                guild_id         TEXT NOT NULL,
                title            TEXT NOT NULL,
                description      TEXT,
                status           TEXT NOT NULL DEFAULT 'Pending',
                priority         TEXT NOT NULL DEFAULT 'Medium',
                due_date         TEXT,
                created_at       TEXT NOT NULL,
                channel_id       TEXT,
                remind_before    INTEGER,
                reminded         INTEGER NOT NULL DEFAULT 0,
                discord_event_id TEXT,
                overdue_notified INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS reminders (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id       INTEGER NOT NULL,
                remind_before INTEGER NOT NULL,
                reminded      INTEGER NOT NULL DEFAULT 0,
                UNIQUE(task_id, remind_before)
            );
            CREATE TABLE IF NOT EXISTS assignees (
                task_id INTEGER NOT NULL,
                user_id TEXT NOT NULL,
                PRIMARY KEY (task_id, user_id)
            );",
        )?;

        // 旧カラム追加（既存DBへの互換マイグレーション）
        let _ = conn.execute("ALTER TABLE tasks ADD COLUMN channel_id TEXT", []);
        let _ = conn.execute("ALTER TABLE tasks ADD COLUMN remind_before INTEGER", []);
        let _ = conn.execute(
            "ALTER TABLE tasks ADD COLUMN reminded INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE tasks ADD COLUMN discord_event_id TEXT", []);
        let added_overdue = conn
            .execute(
                "ALTER TABLE tasks ADD COLUMN overdue_notified INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .is_ok();

        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tasks_guild ON tasks (guild_id);
             CREATE INDEX IF NOT EXISTS idx_reminders_task ON reminders (task_id);
             CREATE INDEX IF NOT EXISTS idx_assignees_user ON assignees (user_id);
             -- 旧 remind_before データを reminders テーブルへ移行
             INSERT OR IGNORE INTO reminders (task_id, remind_before, reminded)
             SELECT id, remind_before, reminded
             FROM tasks
             WHERE remind_before IS NOT NULL;",
        )?;

        // 旧データの期限を正規化フォーマットに揃える（並び替えを正しくするため）
        let rows: Vec<(i64, String)> = conn
            .prepare("SELECT id, due_date FROM tasks WHERE due_date IS NOT NULL")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (id, due) in rows {
            if let Some(normalized) = time::normalize_due(&due).filter(|n| *n != due) {
                conn.execute(
                    "UPDATE tasks SET due_date = ?1 WHERE id = ?2",
                    params![normalized, id],
                )?;
            }
        }

        // カラム追加直後は、既に期限切れのタスクへ一斉通知しないよう通知済み扱いにする
        if added_overdue {
            let now = time::now_due_string();
            conn.execute(
                "UPDATE tasks SET overdue_notified = 1 WHERE due_date IS NOT NULL AND due_date <= ?1",
                params![now],
            )?;
        }

        homework::init(&conn)?;

        Ok(())
    }

    async fn call<F, R>(&self, f: F) -> Result<R, crate::Error>
    where
        F: FnOnce(&mut Connection) -> Result<R, crate::Error> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap();
            f(&mut guard)
        })
        .await?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_task(
        &self,
        user_id: String,
        guild_id: String,
        channel_id: String,
        title: String,
        description: Option<String>,
        priority: Priority,
        due_date: Option<String>,
        reminders: Vec<i64>,
    ) -> Result<i64, crate::Error> {
        let created_at = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let overdue = due_date.as_deref().is_some_and(is_past);
            tx.execute(
                "INSERT INTO tasks
                    (user_id, guild_id, channel_id, title, description, status,
                     priority, due_date, created_at, overdue_notified)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'Pending', ?6, ?7, ?8, ?9)",
                params![
                    user_id,
                    guild_id,
                    channel_id,
                    title,
                    description,
                    priority.as_str(),
                    due_date,
                    created_at,
                    overdue
                ],
            )?;
            let task_id = tx.last_insert_rowid();
            insert_reminders(&tx, task_id, due_date.as_deref(), &reminders)?;
            tx.commit()?;
            Ok(task_id)
        })
        .await
    }

    pub async fn list_tasks(
        &self,
        guild_id: String,
        query: TaskQuery,
    ) -> Result<Vec<Task>, crate::Error> {
        self.call(move |conn| {
            let mut sql = format!("SELECT {TASK_COLUMNS} FROM tasks t WHERE t.guild_id = ?1");
            let mut args = vec![guild_id];

            if let Some(status) = query.status {
                args.push(status.as_str().to_string());
                sql += &format!(" AND t.status = ?{}", args.len());
            }
            if let Some(user_id) = query.assignee {
                args.push(user_id);
                sql += &format!(
                    " AND EXISTS (SELECT 1 FROM assignees a WHERE a.task_id = t.id AND a.user_id = ?{})",
                    args.len()
                );
            }
            if let Some(keyword) = query.keyword {
                args.push(format!("%{}%", escape_like(&keyword)));
                let n = args.len();
                sql += &format!(
                    " AND (t.title LIKE ?{n} ESCAPE '\\' OR IFNULL(t.description, '') LIKE ?{n} ESCAPE '\\')"
                );
            }

            let priority_rank = "CASE t.priority WHEN 'High' THEN 1 WHEN 'Medium' THEN 2 ELSE 3 END";
            sql += &match query.sort {
                TaskSort::Priority => format!(
                    " ORDER BY {priority_rank}, t.due_date IS NULL, t.due_date, t.id DESC"
                ),
                TaskSort::DueDate => format!(
                    " ORDER BY t.due_date IS NULL, t.due_date, {priority_rank}, t.id DESC"
                ),
                TaskSort::Created => " ORDER BY t.id DESC".to_string(),
            };

            let tasks = conn
                .prepare(&sql)?
                .query_map(params_from_iter(args), row_to_task)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(tasks)
        })
        .await
    }

    pub async fn get_task(&self, id: i64, guild_id: String) -> Result<Option<Task>, crate::Error> {
        self.call(move |conn| {
            let sql =
                format!("SELECT {TASK_COLUMNS} FROM tasks t WHERE t.id = ?1 AND t.guild_id = ?2");
            Ok(conn
                .query_row(&sql, params![id, guild_id], row_to_task)
                .optional()?)
        })
        .await
    }

    pub async fn update_task_status(
        &self,
        id: i64,
        guild_id: String,
        status: TaskStatus,
    ) -> Result<usize, crate::Error> {
        self.call(move |conn| {
            Ok(conn.execute(
                "UPDATE tasks SET status = ?1 WHERE id = ?2 AND guild_id = ?3",
                params![status.as_str(), id, guild_id],
            )?)
        })
        .await
    }

    /// タスクを削除する。削除できたタスクを返す（存在しなければ None）
    pub async fn delete_task(
        &self,
        id: i64,
        guild_id: String,
    ) -> Result<Option<Task>, crate::Error> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let sql =
                format!("SELECT {TASK_COLUMNS} FROM tasks t WHERE t.id = ?1 AND t.guild_id = ?2");
            let Some(task) = tx
                .query_row(&sql, params![id, guild_id], row_to_task)
                .optional()?
            else {
                return Ok(None);
            };
            tx.execute("DELETE FROM reminders WHERE task_id = ?1", params![id])?;
            tx.execute("DELETE FROM assignees WHERE task_id = ?1", params![id])?;
            tx.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(Some(task))
        })
        .await
    }

    /// タスクを編集する。更新後のタスクを返す（存在しなければ None）
    pub async fn edit_task(
        &self,
        id: i64,
        guild_id: String,
        edit: TaskEdit,
    ) -> Result<Option<Task>, crate::Error> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let sql =
                format!("SELECT {TASK_COLUMNS} FROM tasks t WHERE t.id = ?1 AND t.guild_id = ?2");
            let Some(cur) = tx
                .query_row(&sql, params![id, guild_id], row_to_task)
                .optional()?
            else {
                return Ok(None);
            };

            let due_changed = edit
                .due_date
                .as_ref()
                .is_some_and(|d| Some(d) != cur.due_date.as_ref());
            let new_due = edit.due_date.or(cur.due_date);

            tx.execute(
                "UPDATE tasks SET title = ?1, description = ?2, priority = ?3, due_date = ?4
                 WHERE id = ?5",
                params![
                    edit.title.unwrap_or(cur.title),
                    edit.description.or(cur.description),
                    edit.priority.unwrap_or(cur.priority).as_str(),
                    new_due,
                    id
                ],
            )?;

            if due_changed {
                tx.execute(
                    "UPDATE tasks SET overdue_notified = ?1 WHERE id = ?2",
                    params![new_due.as_deref().is_some_and(is_past), id],
                )?;
            }

            // リマインダーは指定があれば全置き換え、期限が変わったら送信状態を再計算
            let reminders = match edit.reminders {
                Some(r) => Some(r),
                None if due_changed => Some(cur.reminders.iter().map(|(s, _)| *s).collect()),
                None => None,
            };
            if let Some(reminders) = reminders {
                tx.execute("DELETE FROM reminders WHERE task_id = ?1", params![id])?;
                insert_reminders(&tx, id, new_due.as_deref(), &reminders)?;
            }

            let updated = tx.query_row(&sql, params![id, guild_id], row_to_task)?;
            tx.commit()?;
            Ok(Some(updated))
        })
        .await
    }

    /// 担当者を追加する。追加された件数を返す（タスクが存在しなければ None）
    pub async fn assign(
        &self,
        id: i64,
        guild_id: String,
        user_ids: Vec<String>,
    ) -> Result<Option<usize>, crate::Error> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            if !task_exists(&tx, id, &guild_id)? {
                return Ok(None);
            }
            let mut added = 0;
            for user_id in user_ids {
                added += tx.execute(
                    "INSERT OR IGNORE INTO assignees (task_id, user_id) VALUES (?1, ?2)",
                    params![id, user_id],
                )?;
            }
            tx.commit()?;
            Ok(Some(added))
        })
        .await
    }

    /// 担当者を外す。外した件数を返す（タスクが存在しなければ None）
    pub async fn unassign(
        &self,
        id: i64,
        guild_id: String,
        user_id: String,
    ) -> Result<Option<usize>, crate::Error> {
        self.call(move |conn| {
            if !task_exists(conn, id, &guild_id)? {
                return Ok(None);
            }
            Ok(Some(conn.execute(
                "DELETE FROM assignees WHERE task_id = ?1 AND user_id = ?2",
                params![id, user_id],
            )?))
        })
        .await
    }

    pub async fn get_pending_reminders(&self) -> Result<Vec<PendingReminder>, crate::Error> {
        self.call(|conn| {
            let sql = format!(
                "SELECT {TASK_COLUMNS}, r.id, r.remind_before
                 FROM reminders r
                 JOIN tasks t ON t.id = r.task_id
                 WHERE r.reminded = 0
                   AND t.status != 'Done'
                   AND t.due_date IS NOT NULL
                   AND t.channel_id IS NOT NULL"
            );
            let reminders = conn
                .prepare(&sql)?
                .query_map([], |row| {
                    Ok(PendingReminder {
                        task: row_to_task(row)?,
                        reminder_id: row.get(TASK_COLUMN_COUNT)?,
                        remind_before: row.get(TASK_COLUMN_COUNT + 1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(reminders)
        })
        .await
    }

    pub async fn mark_reminder_sent(&self, reminder_id: i64) -> Result<(), crate::Error> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE reminders SET reminded = 1 WHERE id = ?1",
                params![reminder_id],
            )?;
            Ok(())
        })
        .await
    }

    /// 期限切れ通知がまだ送られていない未完了タスク（期限の判定は呼び出し側で行う）
    pub async fn get_overdue_candidates(&self) -> Result<Vec<Task>, crate::Error> {
        self.call(|conn| {
            let sql = format!(
                "SELECT {TASK_COLUMNS} FROM tasks t
                 WHERE t.overdue_notified = 0
                   AND t.status != 'Done'
                   AND t.due_date IS NOT NULL
                   AND t.channel_id IS NOT NULL"
            );
            let tasks = conn
                .prepare(&sql)?
                .query_map([], row_to_task)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(tasks)
        })
        .await
    }

    pub async fn mark_overdue_notified(&self, task_id: i64) -> Result<(), crate::Error> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE tasks SET overdue_notified = 1 WHERE id = ?1",
                params![task_id],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn set_event_id(&self, task_id: i64, event_id: String) -> Result<(), crate::Error> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE tasks SET discord_event_id = ?1 WHERE id = ?2",
                params![event_id, task_id],
            )?;
            Ok(())
        })
        .await
    }
}

fn task_exists(conn: &Connection, id: i64, guild_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND guild_id = ?2)",
        params![id, guild_id],
        |row| row.get(0),
    )
}

/// 期限が現在時刻を過ぎているか
fn is_past(due: &str) -> bool {
    time::due_to_utc(due).is_some_and(|dt| dt <= chrono::Utc::now())
}

/// リマインダーを登録する。通知時刻を既に過ぎているものは送信済み扱いにする
/// （「1日前」なのに期限直前に届く、といった誤解を招く通知を防ぐため）
fn insert_reminders(
    conn: &Connection,
    task_id: i64,
    due_date: Option<&str>,
    reminders: &[i64],
) -> rusqlite::Result<()> {
    let due = due_date.and_then(time::due_to_utc);
    let now = chrono::Utc::now();
    for secs in reminders {
        let already_passed = due.is_some_and(|d| d - chrono::Duration::seconds(*secs) <= now);
        conn.execute(
            "INSERT OR IGNORE INTO reminders (task_id, remind_before, reminded)
             VALUES (?1, ?2, ?3)",
            params![task_id, secs, already_passed],
        )?;
    }
    Ok(())
}

/// LIKE 句のワイルドカードをエスケープする
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn parse_reminders(s: Option<String>) -> Vec<(i64, bool)> {
    let Some(s) = s.filter(|s| !s.is_empty()) else {
        return vec![];
    };
    let mut entries: Vec<(i64, bool)> = s
        .split(',')
        .filter_map(|part| {
            let (secs, reminded) = part.split_once(':')?;
            Some((secs.parse().ok()?, reminded.parse::<i64>().ok()? != 0))
        })
        .collect();
    entries.sort_by_key(|(s, _)| *s);
    entries
}

fn parse_assignees(s: Option<String>) -> Vec<String> {
    let mut ids: Vec<String> = s
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    ids.sort();
    ids
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        user_id: row.get(1)?,
        guild_id: row.get(2)?,
        title: row.get(3)?,
        description: row.get(4)?,
        status: TaskStatus::from_str(&row.get::<_, String>(5)?),
        priority: Priority::from_str(&row.get::<_, String>(6)?),
        due_date: row.get(7)?,
        created_at: row.get(8)?,
        channel_id: row.get(9)?,
        reminders: parse_reminders(row.get(10)?),
        discord_event_id: row.get(11)?,
        assignees: parse_assignees(row.get(12)?),
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

    async fn add(
        db: &Database,
        guild: &str,
        title: &str,
        priority: Priority,
        due: Option<&str>,
    ) -> i64 {
        db.add_task(
            "100".into(),
            guild.into(),
            "200".into(),
            title.into(),
            None,
            priority,
            due.map(str::to_string),
            vec![],
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn migrates_legacy_database() {
        let db = Database::new(":memory:").unwrap();
        db.conn
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE tasks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL,
                    guild_id TEXT NOT NULL, title TEXT NOT NULL, description TEXT,
                    status TEXT NOT NULL DEFAULT 'Pending', priority TEXT NOT NULL DEFAULT 'Medium',
                    due_date TEXT, created_at TEXT NOT NULL, channel_id TEXT,
                    remind_before INTEGER, reminded INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO tasks (user_id, guild_id, title, due_date, created_at, channel_id, remind_before)
                VALUES ('1', 'G', 'old', '2000-01-01', 'x', '2', 3600),
                       ('1', 'G', 'future', '2999-01-01 09:00:00', 'x', '2', NULL);",
            )
            .unwrap();
        db.init().unwrap();

        let old = db.get_task(1, "G".into()).await.unwrap().unwrap();
        assert_eq!(old.due_date.as_deref(), Some("2000-01-01 00:00"));
        assert_eq!(old.reminders, vec![(3600, false)]);
        let future = db.get_task(2, "G".into()).await.unwrap().unwrap();
        assert_eq!(future.due_date.as_deref(), Some("2999-01-01 09:00"));

        // 既に期限切れの旧タスクには通知しない
        let ids: Vec<_> = db
            .get_overdue_candidates()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![2]);

        // 2回目の init でも壊れない
        db.init().unwrap();
    }

    #[tokio::test]
    async fn delete_does_not_touch_other_guilds() {
        let db = test_db();
        let id = db
            .add_task(
                "1".into(),
                "A".into(),
                "2".into(),
                "t".into(),
                None,
                Priority::Medium,
                Some("2999-01-01 00:00".into()),
                vec![3600],
            )
            .await
            .unwrap();

        assert!(db.delete_task(id, "B".into()).await.unwrap().is_none());
        let task = db.get_task(id, "A".into()).await.unwrap().unwrap();
        assert_eq!(task.reminders, vec![(3600, false)]);

        assert!(db.delete_task(id, "A".into()).await.unwrap().is_some());
        assert!(db.get_task(id, "A".into()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn assign_and_filter_by_assignee() {
        let db = test_db();
        let a = add(&db, "G", "a", Priority::Low, None).await;
        let _b = add(&db, "G", "b", Priority::Low, None).await;

        assert_eq!(
            db.assign(a, "G".into(), vec!["7".into(), "8".into()])
                .await
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            db.assign(a, "G".into(), vec!["7".into()]).await.unwrap(),
            Some(0)
        );
        assert_eq!(
            db.assign(a, "X".into(), vec!["7".into()]).await.unwrap(),
            None
        );

        let query = TaskQuery {
            assignee: Some("8".into()),
            ..Default::default()
        };
        let tasks = db.list_tasks("G".into(), query).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].assignees, vec!["7", "8"]);
        assert_eq!(tasks[0].notify_targets(), vec!["7", "8"]);

        assert_eq!(
            db.unassign(a, "G".into(), "7".into()).await.unwrap(),
            Some(1)
        );
        let task = db.get_task(a, "G".into()).await.unwrap().unwrap();
        assert_eq!(task.assignees, vec!["8"]);
    }

    #[tokio::test]
    async fn search_escapes_wildcards() {
        let db = test_db();
        add(&db, "G", "100% done", Priority::Medium, None).await;
        add(&db, "G", "1000 items", Priority::Medium, None).await;

        let query = TaskQuery {
            keyword: Some("0%".into()),
            ..Default::default()
        };
        let tasks = db.list_tasks("G".into(), query).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "100% done");
    }

    #[tokio::test]
    async fn sorts_by_due_date() {
        let db = test_db();
        add(&db, "G", "none", Priority::High, None).await;
        add(&db, "G", "late", Priority::High, Some("2999-12-31 00:00")).await;
        add(&db, "G", "early", Priority::Low, Some("2999-01-01 00:00")).await;

        let query = TaskQuery {
            sort: TaskSort::DueDate,
            ..Default::default()
        };
        let titles: Vec<_> = db
            .list_tasks("G".into(), query)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, ["early", "late", "none"]);
    }

    #[tokio::test]
    async fn past_reminders_and_overdue_are_marked_on_insert() {
        let db = test_db();
        let past = add(&db, "G", "past", Priority::Medium, Some("2000-01-01 00:00")).await;
        assert!(
            db.get_overdue_candidates()
                .await
                .unwrap()
                .iter()
                .all(|t| t.id != past)
        );

        let id = db
            .add_task(
                "1".into(),
                "G".into(),
                "2".into(),
                "soon".into(),
                None,
                Priority::Medium,
                Some("2999-01-01 00:00".into()),
                vec![1800],
            )
            .await
            .unwrap();
        assert_eq!(db.get_pending_reminders().await.unwrap().len(), 1);

        // 期限を過去に変更すると、未送信リマインダーは送信済み扱いになる
        let edit = TaskEdit {
            due_date: Some("2000-01-01 00:00".into()),
            ..Default::default()
        };
        let task = db.edit_task(id, "G".into(), edit).await.unwrap().unwrap();
        assert_eq!(task.reminders, vec![(1800, true)]);
        assert!(db.get_pending_reminders().await.unwrap().is_empty());
    }
}
