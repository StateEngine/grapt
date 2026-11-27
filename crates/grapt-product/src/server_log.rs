//! Structured UTC timestamp and severity formatting for `grapt-server` stderr (`server.log`).
//!
//! Wall-clock time comes from `grapt_store::SystemClock` so no ambient clock
//! call leaks outside the named clock adapter.

use std::fmt::Display;
use std::io::{self, Write};

use grapt_store::SystemClock;
use grapt_types::ClockAuthority;

/// Inclusive UTC millisecond bounds representable as a 4-digit RFC 3339 year (`0000..=9999`).
const MIN_RFC3339_MS: i64 = -62_167_219_200_000; // 0000-01-01T00:00:00.000Z
const MAX_RFC3339_MS: i64 = 253_402_300_799_999; // 9999-12-31T23:59:59.999Z

pub(crate) fn info(message: impl Display) {
    emit("INFO", message);
}

pub(crate) fn warn(message: impl Display) {
    emit("WARN", message);
}

pub(crate) fn error(message: impl Display) {
    emit("ERROR", message);
}

fn emit(level: &str, message: impl Display) {
    let unix_ms = SystemClock.now().as_millis();
    let line = format_line(unix_ms, level, message);
    let _result = writeln!(io::stderr(), "{line}");
}

/// Format a single structured `server.log` line with RFC 3339 UTC millisecond timestamp and level.
///
/// Line delimiters (`\r`, `\n`) in `message` are escaped so every diagnostic
/// occupies exactly one structured line. Timestamps outside years `0000..=9999`
/// use a fixed non-digit token (`timestamp-out-of-range`) so log scanners never
/// mistake them for a 13-digit epoch millisecond timestamp.
pub(crate) fn format_line(unix_ms: i64, level: &str, message: impl Display) -> String {
    let timestamp = format_timestamp(unix_ms);
    let single_line = escape_line_delimiters(&message.to_string());
    format!("{timestamp} {level} {single_line}")
}

fn escape_line_delimiters(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn format_timestamp(unix_ms: i64) -> String {
    if !(MIN_RFC3339_MS..=MAX_RFC3339_MS).contains(&unix_ms) {
        return "timestamp-out-of-range".to_owned();
    }
    let millis = unix_ms.rem_euclid(1_000);
    let total_secs = unix_ms.div_euclid(1_000);
    let secs_of_day = total_secs.rem_euclid(86_400);
    let days = total_secs.div_euclid(86_400);
    let (year, month, day) = utc_date_from_epoch_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_in_year(year: i64) -> i64 {
    if is_leap_year(year) {
        366
    } else {
        365
    }
}

/// Convert signed days since `1970-01-01` into `(year, month, day)` in UTC.
fn utc_date_from_epoch_days(mut remaining: i64) -> (i64, u32, u32) {
    let mut year = 1970_i64;
    if remaining >= 0 {
        loop {
            let length = days_in_year(year);
            if remaining < length {
                break;
            }
            remaining -= length;
            year += 1;
        }
    } else {
        while remaining < 0 {
            year -= 1;
            remaining += days_in_year(year);
        }
    }
    let month_lengths: [i64; 12] = [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1_u32;
    for length in month_lengths {
        if remaining < length {
            break;
        }
        remaining -= length;
        month += 1;
    }
    let day = (remaining + 1) as u32;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{format_line, MAX_RFC3339_MS};

    #[test]
    fn formats_utc_timestamp_and_level() {
        assert_eq!(
            format_line(0, "INFO", "boot"),
            "1970-01-01T00:00:00.000Z INFO boot"
        );
        // Leap day 2024-02-29T12:34:56.789Z
        assert_eq!(
            format_line(1_709_210_096_789, "INFO", "leap"),
            "2024-02-29T12:34:56.789Z INFO leap"
        );
        // 2026-11-26T20:45:12.345Z
        assert_eq!(
            format_line(1_795_725_912_345, "WARN", "retrying"),
            "2026-11-26T20:45:12.345Z WARN retrying"
        );
        assert_eq!(
            format_line(MAX_RFC3339_MS, "INFO", "max"),
            "9999-12-31T23:59:59.999Z INFO max"
        );
    }

    #[test]
    fn escapes_multiline_messages_into_single_structured_line() {
        let line = format_line(0, "ERROR", "first line\nsecond line\r\nthird\rfourth");
        assert!(!line.contains('\n'));
        assert!(!line.contains('\r'));
        assert_eq!(
            line,
            "1970-01-01T00:00:00.000Z ERROR first line\\nsecond line\\r\\nthird\\rfourth"
        );
    }

    #[test]
    fn formats_out_of_range_years_explicitly() {
        let past_9999 = MAX_RFC3339_MS + 1;
        assert_eq!(
            format_line(past_9999, "WARN", "future"),
            "timestamp-out-of-range WARN future"
        );
    }
}
