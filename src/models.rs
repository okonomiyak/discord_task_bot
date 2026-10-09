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
