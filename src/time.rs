use std::sync::OnceLock;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

/// DB に保存する期限の正規化フォーマット
pub const DUE_FORMAT: &str = "%Y-%m-%d %H:%M";

static TIMEZONE: OnceLock<Tz> = OnceLock::new();

/// ボットが期限を解釈するタイムゾーンを設定する（起動時に1回だけ呼ぶ）
pub fn init_timezone(tz: Tz) {
    let _ = TIMEZONE.set(tz);
}

/// 期限の解釈に使うタイムゾーン（未設定なら Asia/Tokyo）
pub fn timezone() -> Tz {
    *TIMEZONE.get_or_init(|| chrono_tz::Asia::Tokyo)
}

/// ユーザー入力の期限文字列をパースする
///
/// 受け付ける形式: `YYYY-MM-DD HH:MM[:SS]` / `YYYY-MM-DD`（`/` 区切りも可、時刻省略時は 00:00）
pub fn parse_due(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().replace('/', "-");
    for fmt in ["%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&s, fmt) {
            return Some(dt);
        }
    }
    NaiveDate::parse_from_str(&s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
}

/// 現在時刻（設定タイムゾーンの壁時計時刻）
pub fn local_now() -> NaiveDateTime {
    Utc::now().with_timezone(&timezone()).naive_local()
}

/// `HH:MM` / `H時` / `H時M分` 形式の時刻をパースする
pub fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().replace('：', ":");
    if let Ok(t) = NaiveTime::parse_from_str(&s, "%H:%M") {
        return Some(t);
    }
    let rest = s.strip_suffix('分').unwrap_or(&s);
    let (h, m) = match rest.split_once('時') {
        Some((h, m)) => (h, if m.is_empty() { "0" } else { m }),
        None => return None,
    };
    let m = if m == "半" { "30" } else { m };
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

fn parse_date_word(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    match s {
        "今日" | "きょう" => return Some(today),
        "明日" | "あした" | "あす" => return today.succ_opt(),
        "明後日" | "あさって" => return today.succ_opt()?.succ_opt(),
        _ => {}
    }

    // 曜日（「金」「金曜」「金曜日」）→ 次に来るその曜日（今日と同じ曜日なら来週）
    let day = s
        .strip_suffix("曜日")
        .or_else(|| s.strip_suffix('曜'))
        .unwrap_or(s);
    let weekday = match day {
        "月" => Some(0),
        "火" => Some(1),
        "水" => Some(2),
        "木" => Some(3),
        "金" => Some(4),
        "土" => Some(5),
        "日" => Some(6),
        _ => None,
    };
    if let Some(target) = weekday {
        let current = today.weekday().num_days_from_monday() as i64;
        let diff = (target - current).rem_euclid(7);
        let diff = if diff == 0 { 7 } else { diff };
        return Some(today + Duration::days(diff));
    }

    let s = s.replace('/', "-");
    if let Ok(d) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
        return Some(d);
    }
    // 年を省略した「12/5」→ 今日以降で一番近いその日付
    let (m, d) = s.split_once('-')?;
    let (m, d) = (m.parse().ok()?, d.parse().ok()?);
    let this_year = NaiveDate::from_ymd_opt(today.year(), m, d)?;
    if this_year >= today {
        Some(this_year)
    } else {
        NaiveDate::from_ymd_opt(today.year() + 1, m, d)
    }
}

/// 宿題の期限入力をパースする
///
/// 日付: `今日` `明日` `明後日` / `金` `金曜` `金曜日` / `12/5` / `2025-12-05`
/// 時刻（省略時は default_time）: `17:00` `17時` `17時30分`
pub fn parse_due_input(
    input: &str,
    now: NaiveDateTime,
    default_time: NaiveTime,
) -> Option<NaiveDateTime> {
    let input = input.trim().replace('　', " ");
    let mut parts = input.split_whitespace();
    let date_part = parts.next()?;
    let time_part = parts.next();
    if parts.next().is_some() {
        return None;
    }

    // 「明日17時」のように日付と時刻がくっついている場合も受け付ける
    let (date_part, time_part) = match time_part {
        Some(t) => (date_part.to_string(), Some(t.to_string())),
        None => split_glued_time(date_part),
    };

    let date = parse_date_word(&date_part, now.date())?;
    let time = match time_part {
        Some(t) => parse_time(&t)?,
        None => default_time,
    };
    Some(date.and_time(time))
}

fn split_glued_time(s: &str) -> (String, Option<String>) {
    for word in [
        "明後日",
        "あさって",
        "今日",
        "きょう",
        "明日",
        "あした",
        "あす",
    ] {
        if let Some(rest) = s.strip_prefix(word).filter(|r| !r.is_empty()) {
            return (word.to_string(), Some(rest.to_string()));
        }
    }
    (s.to_string(), None)
}

/// 期限文字列を検証し、保存用の正規化フォーマットに変換する
pub fn normalize_due(s: &str) -> Option<String> {
    parse_due(s).map(|dt| dt.format(DUE_FORMAT).to_string())
}

/// 設定タイムゾーンの壁時計時刻を UTC に変換する
pub fn local_to_utc(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
}

/// 現在時刻を設定タイムゾーンでの期限フォーマットで返す（DB 上の期限との文字列比較用）
pub fn now_due_string() -> String {
    Utc::now()
        .with_timezone(&timezone())
        .format(DUE_FORMAT)
        .to_string()
}

/// 期限文字列を UTC 時刻に変換する
pub fn due_to_utc(s: &str) -> Option<DateTime<Utc>> {
    local_to_utc(parse_due(s)?, timezone())
}

/// 期限を Discord タイムスタンプ形式で表示する（閲覧者のローカル時刻で表示される）
pub fn format_due(due: Option<&str>) -> String {
    match due {
        None => "未設定".to_string(),
        Some(s) => match due_to_utc(s) {
            Some(dt) => {
                let unix = dt.timestamp();
                format!("<t:{unix}:f> (<t:{unix}:R>)")
            }
            None => s.to_string(),
        },
    }
}

pub fn format_duration(secs: i64) -> String {
    if secs >= 7 * 24 * 3600 {
        format!("{}週間前", secs / (7 * 24 * 3600))
    } else if secs >= 24 * 3600 {
        format!("{}日前", secs / (24 * 3600))
    } else if secs >= 3600 {
        format!("{}時間前", secs / 3600)
    } else {
        format!("{}分前", secs / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_formats() {
        assert_eq!(
            normalize_due("2025-12-31 15:00").as_deref(),
            Some("2025-12-31 15:00")
        );
        assert_eq!(
            normalize_due("2025-12-31 15:00:30").as_deref(),
            Some("2025-12-31 15:00")
        );
        assert_eq!(
            normalize_due("2025/12/31 9:05").as_deref(),
            Some("2025-12-31 09:05")
        );
        assert_eq!(
            normalize_due(" 2025-12-31 ").as_deref(),
            Some("2025-12-31 00:00")
        );
    }

    #[test]
    fn rejects_invalid_dates() {
        assert_eq!(normalize_due("明日"), None);
        assert_eq!(normalize_due("2025-13-01"), None);
        assert_eq!(normalize_due("2025-02-30 10:00"), None);
    }

    #[test]
    fn converts_local_time_to_utc() {
        let naive = parse_due("2025-12-31 09:00").unwrap();
        let utc = local_to_utc(naive, chrono_tz::Asia::Tokyo).unwrap();
        assert_eq!(utc.format("%Y-%m-%d %H:%M").to_string(), "2025-12-31 00:00");
    }

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn parses_homework_due_input() {
        // 2025-10-09 は木曜日
        let now = dt("2025-10-09 20:00");
        let default = NaiveTime::from_hms_opt(8, 30, 0).unwrap();
        let p =
            |s: &str| parse_due_input(s, now, default).map(|d| d.format(DUE_FORMAT).to_string());

        assert_eq!(p("明日").as_deref(), Some("2025-10-10 08:30"));
        assert_eq!(p("今日 23:59").as_deref(), Some("2025-10-09 23:59"));
        assert_eq!(p("明後日 17時").as_deref(), Some("2025-10-11 17:00"));
        assert_eq!(p("明日17時半").as_deref(), Some("2025-10-10 17:30"));
        assert_eq!(p("金").as_deref(), Some("2025-10-10 08:30"));
        assert_eq!(p("月曜").as_deref(), Some("2025-10-13 08:30"));
        assert_eq!(p("木曜日").as_deref(), Some("2025-10-16 08:30"));
        assert_eq!(p("10/20 13:00").as_deref(), Some("2025-10-20 13:00"));
        assert_eq!(p("1/7").as_deref(), Some("2026-01-07 08:30"));
        assert_eq!(p("2025/12/24").as_deref(), Some("2025-12-24 08:30"));
        assert_eq!(p("2025-12-24 9:05").as_deref(), Some("2025-12-24 09:05"));

        assert_eq!(p("そのうち"), None);
        assert_eq!(p("2/30"), None);
        assert_eq!(p("明日 25:00"), None);
        assert_eq!(p("明日 17:00 くらい"), None);
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(30 * 60), "30分前");
        assert_eq!(format_duration(3 * 3600), "3時間前");
        assert_eq!(format_duration(24 * 3600), "1日前");
        assert_eq!(format_duration(7 * 24 * 3600), "1週間前");
    }
}
