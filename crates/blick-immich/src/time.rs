//! SystemTime to RFC 3339 (UTC). A whole date crate would be too much for this.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// `2026-10-09T10:43:12.500Z` (zone ignored, Immich delivers UTC) to Unix seconds.
pub fn unix_from_rfc3339(s: &str) -> Option<i64> {
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13).unwrap_or(0), num(14, 16).unwrap_or(0), num(17, 19).unwrap_or(0));
    // days_from_civil, Howard Hinnant
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Days since 1970-01-01 to (year, month, day), after Howard Hinnant.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_what_it_formats() {
        assert_eq!(unix_from_rfc3339("1970-01-01T00:00:00.000Z"), Some(0));
        let t = UNIX_EPOCH + Duration::from_secs(1_791_542_592);
        assert_eq!(unix_from_rfc3339(&rfc3339(t)), Some(1_791_542_592));
        assert_eq!(unix_from_rfc3339("kaputt"), None);
    }

    #[test]
    fn formats_known_dates() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        // 2026-10-09 10:43:12.5 UTC
        let t = UNIX_EPOCH + Duration::from_millis(1_791_542_592_500);
        assert_eq!(rfc3339(t), "2026-10-09T10:43:12.500Z");
        // Schalttag
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00.000Z"
        );
    }
}
