#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
}

impl TaskStatus {
    pub fn from_str(s: &str) -> Self {
        match s {
            "InProgress" => Self::InProgress,
            "Done" => Self::Done,
            _ => Self::Pending,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::InProgress => "InProgress",
            Self::Done => "Done",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Pending => "⏳",
            Self::InProgress => "🔄",
            Self::Done => "✅",
        }
    }

    pub fn display(&self) -> &'static str {
        match self {
            Self::Pending => "待機中",
            Self::InProgress => "進行中",
            Self::Done => "完了",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Priority {
    Low,
    Medium,
    High,
}

impl Priority {
    pub fn from_str(s: &str) -> Self {
        match s {
            "Low" => Self::Low,
            "High" => Self::High,
            _ => Self::Medium,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Low => "🟢",
            Self::Medium => "🟡",
            Self::High => "🔴",
        }
    }

    pub fn display(&self) -> &'static str {
        match self {
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
        }
    }
}

/// タスク一覧の並び順
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TaskSort {
    #[default]
    Priority,
    DueDate,
    Created,
}

/// タスク一覧・検索の条件
#[derive(Debug, Clone, Default)]
pub struct TaskQuery {
    pub status: Option<TaskStatus>,
    pub assignee: Option<String>,
    pub keyword: Option<String>,
    pub sort: TaskSort,
}

/// (remind_before_secs, reminded)
pub type ReminderEntry = (i64, bool);

#[derive(Debug, Clone)]
pub struct Task {
    pub id: i64,
    pub user_id: String,
    pub guild_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: Priority,
    pub due_date: Option<String>,
    pub created_at: String,
    pub channel_id: Option<String>,
    /// (remind_before_secs, reminded) のリスト（秒数昇順）
    pub reminders: Vec<ReminderEntry>,
    /// Discord スケジュールイベント ID
    pub discord_event_id: Option<String>,
    /// 担当者のユーザー ID
    pub assignees: Vec<String>,
}

impl Task {
    /// 通知でメンションする相手（担当者がいれば担当者、いなければ作成者）
    pub fn notify_targets(&self) -> Vec<String> {
        if self.assignees.is_empty() {
            vec![self.user_id.clone()]
        } else {
            self.assignees.clone()
        }
    }
}

/// バックグラウンドチェッカーが処理する未送信リマインダー
#[derive(Debug, Clone)]
pub struct PendingReminder {
    pub reminder_id: i64,
    pub task: Task,
    pub remind_before: i64,
}

/// ユーザー ID のリストをメンション文字列にする
pub fn mentions(user_ids: &[String]) -> String {
    user_ids
        .iter()
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 宿題の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwKind {
    /// 宿題（人ごとに完了を記録する）
    Homework,
    /// テスト・小テスト（完了の概念はない）
    Exam,
}

impl HwKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "exam" => Self::Exam,
            _ => Self::Homework,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Homework => "homework",
            Self::Exam => "exam",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Homework => "📘",
            Self::Exam => "📝",
        }
    }

    pub fn display(&self) -> &'static str {
        match self {
            Self::Homework => "宿題",
            Self::Exam => "テスト",
        }
    }
}

/// 宿題・テスト（課題はサーバーで共有し、完了状態は人ごとに持つ）
#[derive(Debug, Clone)]
pub struct Homework {
    pub id: i64,
    pub kind: HwKind,
    pub guild_id: String,
    pub channel_id: String,
    pub created_by: String,
    pub subject: String,
    pub title: String,
    pub description: Option<String>,
    /// 正規化済みの期限（設定タイムゾーンの時刻）
    pub due_date: String,
    /// 登録日時（UNIX 秒）
    pub created_at: i64,
    /// 完了したユーザー ID
    pub done_by: Vec<String>,
    /// 毎週の宿題から自動登録された場合、その設定 ID
    pub repeat_id: Option<i64>,
}

impl Homework {
    pub fn is_done_by(&self, user_id: &str) -> bool {
        self.done_by.iter().any(|u| u == user_id)
    }

    /// 一覧などで使う「科目｜タイトル」形式の表示名（テストは 📝 付き）
    pub fn label(&self) -> String {
        match self.kind {
            HwKind::Homework => format!("{}｜{}", self.subject, self.title),
            HwKind::Exam => format!("📝 {}｜{}", self.subject, self.title),
        }
    }
}

/// 新しく登録する宿題・テスト
#[derive(Debug, Clone)]
pub struct NewHomework {
    pub kind: HwKind,
    pub guild_id: String,
    pub channel_id: String,
    pub created_by: String,
    pub subject: String,
    pub title: String,
    pub description: Option<String>,
    /// 正規化済みの期限
    pub due_date: String,
    pub repeat_id: Option<i64>,
}

/// 毎週の宿題の設定
#[derive(Debug, Clone, PartialEq)]
pub struct HwRepeat {
    pub id: i64,
    pub guild_id: String,
    pub channel_id: String,
    pub created_by: String,
    pub subject: String,
    pub title: String,
    pub description: Option<String>,
    /// 曜日（0 = 月曜 〜 6 = 日曜）
    pub weekday: u32,
    /// 期限の時刻 `HH:MM`
    pub time: String,
    /// 最後に自動登録した宿題の期限
    pub last_due: Option<String>,
}

pub const WEEKDAYS_JA: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

/// 宿題一覧の条件
#[derive(Debug, Clone, Default)]
pub struct HwQuery {
    /// 種類（None ならすべて）
    pub kind: Option<HwKind>,
    pub subject: Option<String>,
    /// この期限以降（正規化フォーマット、含む）
    pub due_from: Option<String>,
    /// この期限より前（正規化フォーマット、含まない）
    pub due_until: Option<String>,
    /// このユーザーが未完了のものだけ
    pub not_done_by: Option<String>,
    /// このユーザーが完了済みのものだけ
    pub done_by: Option<String>,
    /// 期限の新しい順にする
    pub newest_first: bool,
}

/// サーバーごとの宿題設定
#[derive(Debug, Clone, PartialEq)]
pub struct HwSettings {
    pub guild_id: String,
    /// 通知・まとめ投稿のチャンネル（未設定なら宿題を登録したチャンネル）
    pub channel_id: Option<String>,
    /// チャンネルへの事前通知（期限の何秒前か、None でオフ）
    pub remind_before: Option<i64>,
    /// 毎日のまとめ投稿の時刻 `HH:MM`（None でオフ）
    pub summary_time: Option<String>,
    /// 日付だけ指定されたときの期限の時刻 `HH:MM`
    pub default_time: String,
    /// 最後にまとめ投稿した日 `YYYY-MM-DD`
    pub last_summary_date: Option<String>,
}

impl HwSettings {
    pub const DEFAULT_REMIND_BEFORE: i64 = 24 * 3600;
    pub const DEFAULT_TIME: &'static str = "08:30";

    pub fn default_for(guild_id: String) -> Self {
        Self {
            guild_id,
            channel_id: None,
            remind_before: Some(Self::DEFAULT_REMIND_BEFORE),
            summary_time: None,
            default_time: Self::DEFAULT_TIME.to_string(),
            last_summary_date: None,
        }
    }
}

/// `/hw settings` で変更する項目（None は変更しない）
#[derive(Debug, Default)]
pub struct HwSettingsPatch {
    pub channel_id: Option<String>,
    pub remind_before: Option<Option<i64>>,
    pub summary_time: Option<Option<String>>,
    pub default_time: Option<String>,
}
