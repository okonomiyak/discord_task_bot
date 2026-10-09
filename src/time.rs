use std::sync::OnceLock;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
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

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(30 * 60), "30分前");
        assert_eq!(format_duration(3 * 3600), "3時間前");
        assert_eq!(format_duration(24 * 3600), "1日前");
        assert_eq!(format_duration(7 * 24 * 3600), "1週間前");
    }
}
