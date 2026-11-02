//! 宿題の達成率の集計

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use serde::Serialize;

use crate::models::{Homework, HwKind};
use crate::time::DUE_FORMAT;

/// 集計する期間
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatsPeriod {
    Week,
    Month,
    All,
}

impl StatsPeriod {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// 期間の (開始, 終了) を期限の文字列で返す（終了は含まない）と、表示用の名前
    pub fn range(&self, now: NaiveDateTime) -> (Option<String>, Option<String>, String) {
        let fmt = |d: NaiveDate| {
            d.and_hms_opt(0, 0, 0)
                .unwrap_or(now)
                .format(DUE_FORMAT)
                .to_string()
        };
        let today = now.date();
        match self {
            Self::Week => {
                let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
                let next = monday + Duration::days(7);
                let sunday = next - Duration::days(1);
                (
                    Some(fmt(monday)),
                    Some(fmt(next)),
                    format!(
                        "今週（{}/{}〜{}/{}）",
                        monday.month(),
                        monday.day(),
                        sunday.month(),
                        sunday.day()
                    ),
                )
            }
            Self::Month => {
                let first = today.with_day(1).unwrap_or(today);
                let next = if first.month() == 12 {
                    NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
                } else {
                    NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
                }
                .unwrap_or(first);
                (
                    Some(fmt(first)),
                    Some(fmt(next)),
                    format!("{}年{}月", first.year(), first.month()),
                )
            }
            Self::All => (None, None, "これまでの全期間".to_string()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Count {
    pub done: usize,
    pub total: usize,
}

impl Count {
    fn add(&mut self, done: bool) {
        self.total += 1;
        if done {
            self.done += 1;
        }
    }

    /// 達成率（0〜100、対象がなければ None）
    pub fn percent(&self) -> Option<u32> {
        (self.total > 0).then(|| (self.done * 100 / self.total) as u32)
    }

    /// `████████░░ 80%（8/10）` 形式の表示
    pub fn bar(&self) -> String {
        match self.percent() {
            None => "対象なし".to_string(),
            Some(p) => {
                let filled = (p as usize + 5) / 10;
                format!(
                    "{}{} {}%（{}/{}）",
                    "█".repeat(filled),
                    "░".repeat(10 - filled),
                    p,
                    self.done,
                    self.total
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubjectCount {
    pub subject: String,
    #[serde(flatten)]
    pub count: Count,
}

/// ある人の達成率
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HwStats {
    /// 期間内のすべての宿題
    pub overall: Count,
    /// 期限が過ぎた宿題（提出できたか）
    pub past: Count,
    /// これからの宿題（どこまで進んだか）
    pub upcoming: Count,
    /// 科目別（宿題の多い順）
    pub by_subject: Vec<SubjectCount>,
}

/// 宿題の一覧から user の達成率を集計する（テストは対象外）
pub fn compute(list: &[Homework], user_id: &str, now: NaiveDateTime) -> HwStats {
    let now = now.format(DUE_FORMAT).to_string();
    let mut stats = HwStats::default();
    for hw in list.iter().filter(|h| h.kind == HwKind::Homework) {
        let done = hw.is_done_by(user_id);
        stats.overall.add(done);
        if hw.due_date < now {
            stats.past.add(done);
        } else {
            stats.upcoming.add(done);
        }
        match stats
            .by_subject
            .iter_mut()
            .find(|s| s.subject == hw.subject)
        {
            Some(s) => s.count.add(done),
            None => {
                let mut count = Count::default();
                count.add(done);
                stats.by_subject.push(SubjectCount {
                    subject: hw.subject.clone(),
                    count,
                });
            }
        }
    }
    stats.by_subject.sort_by(|a, b| {
        b.count
            .total
            .cmp(&a.count.total)
            .then(a.subject.cmp(&b.subject))
    });
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn hw(kind: HwKind, subject: &str, due: &str, done_by: &[&str]) -> Homework {
        Homework {
            id: 0,
            kind,
            guild_id: "G".into(),
            channel_id: "1".into(),
            created_by: "1".into(),
            subject: subject.into(),
            title: "t".into(),
            description: None,
            due_date: due.into(),
            created_at: 0,
            done_by: done_by.iter().map(|s| s.to_string()).collect(),
            repeat_id: None,
        }
    }

    #[test]
    fn computes_personal_rates() {
        let now = dt("2025-10-09 20:00");
        let list = [
            hw(HwKind::Homework, "数学", "2025-10-08 08:30", &["me"]),
            hw(HwKind::Homework, "数学", "2025-10-09 08:30", &["other"]),
            hw(
                HwKind::Homework,
                "英語",
                "2025-10-10 08:30",
                &["me", "other"],
            ),
            hw(HwKind::Homework, "数学", "2025-10-11 08:30", &[]),
            hw(HwKind::Exam, "英語", "2025-10-10 10:00", &[]),
        ];
        let s = compute(&list, "me", now);
        assert_eq!(s.overall, Count { done: 2, total: 4 });
        assert_eq!(s.past, Count { done: 1, total: 2 });
        assert_eq!(s.upcoming, Count { done: 1, total: 2 });
        assert_eq!(s.by_subject[0].subject, "数学");
        assert_eq!(s.by_subject[0].count, Count { done: 1, total: 3 });
        assert_eq!(s.by_subject[1].count, Count { done: 1, total: 1 });
        assert_eq!(s.overall.percent(), Some(50));
    }

    #[test]
    fn formats_bars() {
        assert_eq!(Count { done: 8, total: 10 }.bar(), "████████░░ 80%（8/10）");
        assert_eq!(Count { done: 0, total: 3 }.bar(), "░░░░░░░░░░ 0%（0/3）");
        assert_eq!(Count { done: 3, total: 3 }.bar(), "██████████ 100%（3/3）");
        assert_eq!(Count::default().bar(), "対象なし");
        assert_eq!(Count::default().percent(), None);
    }

    #[test]
    fn period_ranges() {
        // 2025-10-09 は木曜日
        let now = dt("2025-10-09 20:00");
        let (from, until, label) = StatsPeriod::Week.range(now);
        assert_eq!(from.as_deref(), Some("2025-10-06 00:00"));
        assert_eq!(until.as_deref(), Some("2025-10-13 00:00"));
        assert_eq!(label, "今週（10/6〜10/12）");

        let (from, until, label) = StatsPeriod::Month.range(now);
        assert_eq!(from.as_deref(), Some("2025-10-01 00:00"));
        assert_eq!(until.as_deref(), Some("2025-11-01 00:00"));
        assert_eq!(label, "2025年10月");

        let (_, until, _) = StatsPeriod::Month.range(dt("2025-12-15 10:00"));
        assert_eq!(until.as_deref(), Some("2026-01-01 00:00"));

        assert_eq!(StatsPeriod::All.range(now).0, None);
    }
}
