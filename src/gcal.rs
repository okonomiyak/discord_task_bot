//! Google カレンダーの「予定を作成」画面を、宿題・テストの内容を入れた状態で開くリンク。
//! 押した人が Google の画面で保存したときだけ、その人のカレンダーに追加される
//! （Google の API や OAuth は使わない）

use chrono::{DateTime, Duration, Utc};

use crate::commands::{WebTab, dashboard_url};
use crate::models::{Homework, HwKind};
use crate::time::due_to_utc;

/// Discord のリンクボタンの URL の上限
pub const DISCORD_URL_LIMIT: usize = 512;

/// URL のクエリに入れる文字をエンコードする（英数字と -_.~ 以外は %XX）
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn build(text: &str, start: DateTime<Utc>, end: DateTime<Utc>, details: &str) -> String {
    let mut url = format!(
        "https://calendar.google.com/calendar/render?action=TEMPLATE&text={}&dates={}/{}",
        encode(text),
        stamp(start),
        stamp(end)
    );
    if !details.is_empty() {
        url += "&details=";
        url += &encode(details);
    }
    url
}

/// 予定の名前（宿題は「【宿題】数学 ワーク（〆切）」、テストは「【テスト】英語 単語テスト」）
fn event_title(hw: &Homework, title: &str) -> String {
    match hw.kind {
        HwKind::Homework => format!("【宿題】{} {}（〆切）", hw.subject, title),
        HwKind::Exam => format!("【テスト】{} {}", hw.subject, title),
    }
}

/// 期限（テストは日時）から始まる予定のリンク。宿題は 30 分、テストは 1 時間。
/// web_base を渡すと、説明欄に宿題ボードのリンクも入れる。
/// max_len を超えるときは、説明 → 内容の順に削って収める
pub fn calendar_url(
    hw: &Homework,
    web_base: Option<&str>,
    max_len: Option<usize>,
) -> Option<String> {
    let start = due_to_utc(&hw.due_date)?;
    let end = start
        + match hw.kind {
            HwKind::Homework => Duration::minutes(30),
            HwKind::Exam => Duration::hours(1),
        };
    let board = web_base.map(|b| {
        format!(
            "宿題ボード: {}",
            dashboard_url(b, Some(&hw.guild_id), WebTab::from(hw.kind))
        )
    });
    let details = [hw.description.clone(), board]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n");

    let url = build(&event_title(hw, &hw.title), start, end, &details);
    let Some(max) = max_len else {
        return Some(url);
    };
    if url.len() <= max {
        return Some(url);
    }
    // 説明なし、それでも長ければ内容を短くする
    let chars: Vec<char> = hw.title.chars().collect();
    for n in (0..=chars.len()).rev() {
        let mut title: String = chars[..n].iter().collect();
        if n < chars.len() {
            title.push('…');
        }
        let url = build(&event_title(hw, &title), start, end, "");
        if url.len() <= max {
            return Some(url);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(kind: HwKind, title: &str, memo: Option<&str>) -> Homework {
        Homework {
            id: 1,
            kind,
            guild_id: "G".into(),
            channel_id: "1".into(),
            created_by: "1".into(),
            subject: "数学".into(),
            title: title.into(),
            description: memo.map(String::from),
            due_date: "2025-10-10 08:30".into(),
            created_at: 0,
            done_by: vec![],
            repeat_id: None,
        }
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(encode("p.10-15_~"), "p.10-15_~");
    }

    #[test]
    fn builds_homework_and_exam_links() {
        // TIMEZONE 未設定なら Asia/Tokyo（08:30 JST = 23:30 UTC の前日）
        let url = calendar_url(
            &hw(HwKind::Homework, "ワーク", Some("p.10")),
            Some("https://hw.example.com"),
            None,
        )
        .unwrap();
        assert!(
            url.starts_with("https://calendar.google.com/calendar/render?action=TEMPLATE&text=")
        );
        assert!(url.contains("&dates=20251009T233000Z/20251010T000000Z"));
        assert!(url.contains(&encode("【宿題】数学 ワーク（〆切）")));
        assert!(url.contains(&format!(
            "&details={}",
            encode("p.10\n\n宿題ボード: https://hw.example.com/?guild=G&tab=homework")
        )));

        let url = calendar_url(&hw(HwKind::Exam, "小テスト", None), None, None).unwrap();
        assert!(url.contains("&dates=20251009T233000Z/20251010T003000Z"));
        assert!(url.contains(&encode("【テスト】数学 小テスト")));
        assert!(!url.contains("details"));
    }

    #[test]
    fn long_links_fit_discord_limit() {
        let item = hw(
            HwKind::Homework,
            &"あ".repeat(100),
            Some(&"メモ".repeat(400)),
        );
        let url = calendar_url(
            &item,
            Some("https://hw.example.com"),
            Some(DISCORD_URL_LIMIT),
        )
        .unwrap();
        assert!(url.len() <= DISCORD_URL_LIMIT);
        assert!(!url.contains("details"));
        assert!(url.contains(&encode("…（〆切）")));

        // 短ければ説明も残る
        let short = hw(HwKind::Homework, "ワーク", Some("p.10"));
        let url = calendar_url(&short, None, Some(DISCORD_URL_LIMIT)).unwrap();
        assert!(url.contains("details"));
    }

    #[test]
    fn invalid_due_has_no_link() {
        let mut item = hw(HwKind::Homework, "ワーク", None);
        item.due_date = "そのうち".into();
        assert!(calendar_url(&item, None, None).is_none());
    }
}
