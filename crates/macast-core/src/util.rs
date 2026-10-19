//! Small helpers shared across the crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Format a duration in seconds as `H:MM:SS` (UPnP `TransportState` time format).
///
/// `0` -> `0:00:00`, `75.5` -> `0:01:15`, `3723.0` -> `1:02:03`.
pub fn format_time(secs: f64) -> String {
    let total = if secs.is_finite() && secs > 0.0 {
        secs.round() as i64
    } else {
        0
    };
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{h}:{m:02}:{s:02}")
}

/// Parse a UPnP time string (`H:MM:SS`, `MM:SS` or plain seconds) into seconds.
pub fn parse_time(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return 0.0;
    }
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        1 => parts[0].parse::<f64>().unwrap_or(0.0).max(0.0),
        2 => {
            let m = parts[0].parse::<f64>().unwrap_or(0.0);
            let sec = parts[1].parse::<f64>().unwrap_or(0.0);
            (m * 60.0 + sec).max(0.0)
        }
        3 => {
            let h = parts[0].parse::<f64>().unwrap_or(0.0);
            let m = parts[1].parse::<f64>().unwrap_or(0.0);
            let sec = parts[2].parse::<f64>().unwrap_or(0.0);
            (h * 3600.0 + m * 60.0 + sec).max(0.0)
        }
        _ => 0.0,
    }
}

/// HTTP-date (RFC 1123), e.g. `Sun, 06 Nov 1994 08:49:37 GMT`.
/// Used for the SSDP `DATE` header. Avoids pulling in a date crate.
pub fn http_date_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    http_date(secs)
}

/// Format a Unix timestamp as an RFC 1123 HTTP-date (always GMT).
pub fn http_date(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let dow = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday; 0 = Sunday

    const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WDAYS[dow as usize],
        d,
        MONTHS[(month - 1) as usize],
        year,
        h,
        m,
        s
    )
}

/// XML-escape text content.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(format_time(0.0), "0:00:00");
        assert_eq!(format_time(75.5), "0:01:16");
        assert_eq!(format_time(3723.0), "1:02:03");
        assert_eq!(format_time(-5.0), "0:00:00");
        assert_eq!(format_time(f64::NAN), "0:00:00");
    }

    #[test]
    fn parses_upnp_times() {
        assert_eq!(parse_time("0:01:15"), 75.0);
        assert_eq!(parse_time("1:02:03"), 3723.0);
        assert_eq!(parse_time("01:15"), 75.0);
        assert_eq!(parse_time("90"), 90.0);
        assert_eq!(parse_time(" 0:00:30 "), 30.0);
        assert_eq!(parse_time(""), 0.0);
        assert_eq!(parse_time("not-a-time"), 0.0);
        assert_eq!(parse_time("1:2:3:4"), 0.0);
    }

    #[test]
    fn formats_http_dates() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(946_684_800), "Sat, 01 Jan 2000 00:00:00 GMT");
        assert_eq!(http_date(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
    }

    #[test]
    fn escapes_xml() {
        assert_eq!(
            xml_escape("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
        assert_eq!(xml_escape("中文 & 符号"), "中文 &amp; 符号");
    }
}
