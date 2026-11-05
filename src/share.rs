//! LINE などに貼れる、宿題・テストの一覧のテキスト
//! （Web の「テキストでコピー」も同じ書き方にしている: src/web/index.html の shareText）

use chrono::{Datelike, NaiveDateTime, Timelike};

use crate::models::{Homework, WEEKDAYS_JA};
use crate::time::DUE_FORMAT;

/// Discord のメッセージの上限（2000文字）より少し短く
const MAX_CHARS: usize = 1900;

/// `2025-10-10 08:30` → `10/10(金) 08:30`
pub fn short_due(due: &str) -> String {
    match NaiveDateTime::parse_from_str(due, DUE_FORMAT) {
        Ok(d) => format!(
            "{}/{}({}) {:02}:{:02}",
            d.month(),
            d.day(),
            WEEKDAYS_JA[d.weekday().num_days_from_monday() as usize],
            d.hour(),
            d.minute()
        ),
        Err(_) => due.to_string(),
    }
}

/// `・数学 ワーク p.10（10/10(金) 08:30まで）`。テストは `（10/10(金) 10:40）`
fn line(hw: &Homework, until: bool) -> String {
    format!(
        "・{} {}（{}{}）",
        hw.subject,
        hw.title,
        short_due(&hw.due_date),
        if until { "まで" } else { "" }
    )
}

/// 【宿題】【テスト】の見出しを付けた一覧。何もなければ空文字。
/// 長すぎるときは末尾を「…ほか N 件」にする
pub fn share_text(homework: &[Homework], exams: &[Homework]) -> String {
    let mut out: Vec<String> = Vec::new();
    let total = homework.len() + exams.len();
    let mut shown = 0;
    for (heading, list, until) in [("【宿題】", homework, true), ("【テスト】", exams, false)]
    {
        if list.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(heading.to_string());
        for hw in list {
            let l = line(hw, until);
            let used: usize = out.iter().map(|s| s.chars().count() + 1).sum();
            if used + l.chars().count() > MAX_CHARS {
                out.push(format!("…ほか {} 件", total - shown));
                return out.join("\n");
            }
            out.push(l);
            shown += 1;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::HwKind;

    fn hw(kind: HwKind, subject: &str, title: &str, due: &str) -> Homework {
        Homework {
            id: 1,
            kind,
            guild_id: "G".into(),
            channel_id: "1".into(),
            created_by: "1".into(),
            subject: subject.into(),
            title: title.into(),
            description: Some("メモは載せない".into()),
            due_date: due.into(),
            created_at: 0,
            done_by: vec![],
            repeat_id: None,
        }
    }

    #[test]
    fn formats_short_due() {
        // 2025-10-10 は金曜日
        assert_eq!(short_due("2025-10-10 08:30"), "10/10(金) 08:30");
        assert_eq!(short_due("2025-10-12 17:05"), "10/12(日) 17:05");
        assert_eq!(short_due("変な値"), "変な値");
    }

    #[test]
    fn builds_share_text() {
        let homework = [
            hw(
                HwKind::Homework,
                "数学",
                "ワーク p.10-15",
                "2025-10-10 08:30",
            ),
            hw(HwKind::Homework, "英語", "音読", "2025-10-13 08:30"),
        ];
        let exams = [hw(
            HwKind::Exam,
            "英語",
            "単語テスト Unit 3",
            "2025-10-14 10:40",
        )];
        assert_eq!(
            share_text(&homework, &exams),
            "【宿題】\n\
             ・数学 ワーク p.10-15（10/10(金) 08:30まで）\n\
             ・英語 音読（10/13(月) 08:30まで）\n\
             \n\
             【テスト】\n\
             ・英語 単語テスト Unit 3（10/14(火) 10:40）"
        );
        assert_eq!(
            share_text(&[], &exams),
            "【テスト】\n・英語 単語テスト Unit 3（10/14(火) 10:40）"
        );
        assert_eq!(share_text(&[], &[]), "");
    }

    #[test]
    fn long_share_text_is_cut() {
        let title = "あ".repeat(90);
        let homework: Vec<Homework> = (0..40)
            .map(|_| hw(HwKind::Homework, "数学", &title, "2025-10-10 08:30"))
            .collect();
        let text = share_text(&homework, &[]);
        assert!(text.chars().count() <= 2000);
        let shown = text.lines().filter(|l| l.starts_with('・')).count();
        assert!(text.ends_with(&format!("…ほか {} 件", 40 - shown)));
    }
}
