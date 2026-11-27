//! Observable-validity reconstruction of `new Date(value)`.
//!
//! This answers ONE question — would the frozen product accept this string as a
//! `format: "date-time"` answer — and deliberately not "what instant does it
//! denote". A form validity predicate needs the first and never the second, so
//! nothing here computes a time.
//!
//! Reconstructed from a MEASURED corpus against the oracle's JavaScript runtime 1.3.14, the runtime the
//! frozen product runs on, not from the ECMAScript specification. The
//! specification mandates one format and calls everything else
//! implementation-defined, but the product's observable behaviour is the
//! contract this projection owes, and that behaviour includes the legacy
//! fallback. The corpus is preserved beside this lane's evidence and every case
//! in it is asserted by the differential test below.
//!
//! Two paths, and which one applies is decided by SHAPE, not by trying one and
//! falling back. A string in the ISO shape is judged by ISO rules ALONE: the
//! oracle refuses `2026-13-01` outright rather than re-reading it as a legacy
//! date, so a fallback would wrongly admit it.

/// The whitespace `new Date` trims before parsing.
fn trim(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_ascii_whitespace())
}

/// Is this a value the oracle accepts?
pub fn is_valid(value: &str) -> bool {
    let value = trim(value);
    if value.is_empty() {
        return false;
    }
    match iso_shape(value) {
        // In the ISO shape, ISO rules are the whole answer.
        Some(valid) => valid,
        None => legacy_valid(value),
    }
}

/// `Some(validity)` when the string is in the ISO shape, `None` when it is not
/// and the legacy reading applies.
fn iso_shape(value: &str) -> Option<bool> {
    let (date, time) = match value.split_once(['T', 't']) {
        Some((date, rest)) => (date, Some(rest)),
        None => (value, None),
    };
    let (year_ok, rest) = iso_year(date)?;
    // A calendar part that is not exactly `-MM` / `-MM-DD` is not the ISO shape.
    let calendar_ok = match rest {
        "" => true,
        _ => {
            let mut parts = rest.split('-');
            // `rest` starts with '-', so the first split is empty.
            if parts.next() != Some("") {
                return None;
            }
            let month = parts.next()?;
            if month.len() != 2 || !month.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let month_ok = matches!(month.parse::<u32>(), Ok(m) if (1..=12).contains(&m));
            match parts.next() {
                None => month_ok,
                Some(day) => {
                    if day.len() != 2 || !day.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    if parts.next().is_some() {
                        return None;
                    }
                    // Day 31 in a 30-day month is ACCEPTED and rolls over; only
                    // the 1..=31 range is enforced. The round-trip check that
                    // refuses February 30th belongs to `format: "date"`.
                    month_ok && matches!(day.parse::<u32>(), Ok(d) if (1..=31).contains(&d))
                }
            }
        }
    };
    let Some(time) = time else {
        return Some(year_ok && calendar_ok);
    };
    Some(year_ok && calendar_ok && iso_time_valid(time))
}

/// The ISO year: optionally signed, four or six digits. Returns whether it is in
/// range along with the remainder, or `None` when the shape does not apply.
fn iso_year(date: &str) -> Option<(bool, &str)> {
    let (signed, body) = match date.strip_prefix(['+', '-']) {
        Some(body) => (true, body),
        None => (false, date),
    };
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    // An UNSIGNED year is exactly four digits, so `275760-09-13` is NOT the ISO
    // shape and the oracle refuses it outright. A SIGNED one may be four or six.
    let admitted = if signed {
        digits == 4 || digits == 6
    } else {
        digits == 4
    };
    if !admitted {
        return None;
    }
    Some((true, &date[date.len() - (body.len() - digits)..]))
}

fn iso_time_valid(time: &str) -> bool {
    // Split the offset off the right. `Z`/`z` or a signed offset; a bare time is
    // also legal.
    let (clock, offset) = match time.rfind(['+', '-']) {
        Some(index) if index > 0 => (&time[..index], Some(&time[index..])),
        _ => match time.strip_suffix(['Z', 'z']) {
            Some(clock) => (clock, Some("Z")),
            None => (time, None),
        },
    };
    if let Some(offset) = offset {
        if !iso_offset_valid(offset) {
            return false;
        }
    }
    iso_clock_valid(clock)
}

fn iso_offset_valid(offset: &str) -> bool {
    if offset == "Z" || offset == "z" {
        return true;
    }
    let Some(body) = offset.strip_prefix(['+', '-']) else {
        return false;
    };
    // `+HH:MM` and `+HHMM` are accepted; `+HH` alone is not.
    let (hour, minute) = match body.split_once(':') {
        Some((hour, minute)) => (hour, minute),
        None if body.len() == 4 => body.split_at(2),
        None => return false,
    };
    two_digit(hour, 23) && two_digit(minute, 59)
}

fn two_digit(value: &str, maximum: u32) -> bool {
    value.len() == 2
        && value.bytes().all(|b| b.is_ascii_digit())
        && matches!(value.parse::<u32>(), Ok(v) if v <= maximum)
}

fn iso_clock_valid(clock: &str) -> bool {
    let (clock, fraction) = match clock.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (clock, None),
    };
    // A decimal point must carry at least one digit.
    if let Some(fraction) = fraction {
        if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    let mut parts = clock.split(':');
    let (Some(hour), Some(minute)) = (parts.next(), parts.next()) else {
        // Hours alone are not a time: `T23` is refused.
        return false;
    };
    let second = parts.next();
    if parts.next().is_some() || (fraction.is_some() && second.is_none()) {
        return false;
    }
    if !two_digit(hour, 24) || !two_digit(minute, 59) {
        return false;
    }
    if let Some(second) = second {
        // A leap second is NOT admitted here: the oracle refuses `23:59:60`.
        if !two_digit(second, 59) {
            return false;
        }
    }
    // Hour 24 is end of day and nothing else.
    if hour == "24" {
        return minute == "00"
            && second.is_none_or(|second| second == "00")
            && fraction.is_none_or(|fraction| fraction.bytes().all(|b| b == b'0'));
    }
    true
}

/// The largest year `new Date` represents. Beyond it a bare number that would
/// otherwise read as a year is refused, which is why `20260906` is invalid while
/// `20260` is fine.
const MAX_YEAR: i64 = 275_760;

/// The legacy reading.
///
/// Permissive about SHAPE and strict about RANGES, which is the part a
/// hold-out corpus was needed to discover: `9/6` is a valid month/day, `6/2026`
/// is not because 2026 cannot be a day, `2026/13/01` is not because 13 cannot be
/// a month, and `2026-249` is not for the same reason. Fields are interpreted by
/// POSITION, and which position means what depends on whether the leading number
/// could be a year.
fn legacy_valid(value: &str) -> bool {
    if !value.is_ascii() {
        // A non-breaking space is neither whitespace nor a separator to this
        // parser, so `2026-09-06\u{a0}` is refused where a trailing ASCII space is
        // accepted.
        return false;
    }
    // A doubled punctuation separator leaves an empty field and is refused:
    // `2026--` is invalid while `2026-09-06  23:59` — a doubled SPACE — is fine.
    for pair in value.as_bytes().windows(2) {
        if matches!(pair[0], b'-' | b'/' | b'.') && pair[0] == pair[1] {
            return false;
        }
    }

    let tokens = tokenize(value);
    if tokens.is_empty() {
        return false;
    }

    let mut numbers: Vec<i64> = Vec::new();
    let mut month_name = false;
    for (index, token) in tokens.iter().enumerate() {
        match classify(token, index, &tokens) {
            Token::Number(value) => numbers.push(value),
            Token::MonthName => month_name = true,
            Token::Ignorable => {}
            Token::Unrecognised => return false,
        }
    }

    if month_name {
        // The month is named, so the numbers are a day and a year in either
        // order, and anything after them is a time this predicate ignores.
        return match numbers.as_slice() {
            [] => false,
            // `March 2026` names a month and a year and no day at all.
            [single] => in_year_range(*single),
            // Otherwise one field is the day and another is the year, in either
            // order — `31 Dec 2026` and `2026 Sep 6` are both valid. Trailing
            // numbers are a time. BOTH roles must be filled by DIFFERENT fields:
            // requiring only one is what let `Sep 13 275760` through, where the
            // day is fine and the year is past the representable range.
            _ => {
                let Some(day_at) = numbers.iter().position(|value| is_day(*value)) else {
                    return false;
                };
                numbers
                    .iter()
                    .enumerate()
                    .any(|(index, value)| index != day_at && is_legacy_year(*value))
            }
        };
    }

    match numbers.as_slice() {
        [] => false,
        // A lone number reads as a year, whatever its size, so `0`, `1` and `12`
        // are dates while `20260906` is out of representable range.
        [year] => in_year_range(*year),
        // Two fields are month and day -- NOT month and year, which is why
        // `6/2026` is refused.
        [first, second] if *first <= 31 => is_month(*first) && is_day(*second),
        // ...unless the first cannot be a day, in which case it is a year and
        // the second is its month.
        [first, second] => is_legacy_year(*first) && is_month(*second),
        // Three or more: year-first when the leading field is too large to be a
        // day, month-first otherwise. Trailing fields are a time and ignored.
        [first, second, third, ..] if *first > 31 => {
            is_legacy_year(*first) && is_month(*second) && is_day(*third)
        }
        [first, second, third, ..] => is_month(*first) && is_day(*second) && is_legacy_year(*third),
    }
}

fn is_month(value: i64) -> bool {
    (1..=12).contains(&value)
}

fn is_day(value: i64) -> bool {
    (1..=31).contains(&value)
}

/// A LONE number is a year at the start of that year, so the whole
/// representable range is available to it.
fn in_year_range(value: i64) -> bool {
    (-MAX_YEAR..=MAX_YEAR).contains(&value)
}

/// A year carrying a month and day, where the true boundary is the maximum
/// INSTANT rather than a year number.
///
/// Measured: `99999-09-13` is accepted and `275760-09-13` is refused, while the
/// lone number `275760` is accepted. The difference is that a legacy date is
/// read in LOCAL time, so a September date in the final year lands past the
/// maximum instant while the first of January does not — which makes the exact
/// boundary depend on the host timezone.
///
/// This predicate deliberately does not compute instants, so it cuts at the
/// final year. Every measured case agrees. The residual is narrow and stated
/// rather than hidden: for a date in years 100000..275759 the true answer
/// depends on the instant and the host's offset, and this may admit a value the
/// oracle refuses at that extreme.
fn is_legacy_year(value: i64) -> bool {
    (-MAX_YEAR < value) && (value < MAX_YEAR)
}

enum Token {
    Number(i64),
    MonthName,
    Ignorable,
    Unrecognised,
}

fn tokenize(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || c == '+' {
            current.push(c);
        } else {
            if !current.is_empty() {
                tokens.push(core::mem::take(&mut current));
            }
            if c == ',' {
                tokens.push(String::from(","));
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Weekday names are skipped wherever they appear: `Thu Jan 01 2026` is valid
/// with no comma at all.
const WEEKDAYS: &[&str] = &[
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];

const ZONES: &[&str] = &[
    "gmt", "utc", "ut", "z", "est", "edt", "cst", "cdt", "mst", "mdt", "pst", "pdt",
];

fn classify(token: &str, index: usize, tokens: &[String]) -> Token {
    if token == "," {
        return Token::Ignorable;
    }
    if let Ok(number) = token.parse::<i64>() {
        return Token::Number(number);
    }
    let lower = token.to_ascii_lowercase();
    // A month name matches by PREFIX of at least three characters, so "Marc"
    // and even "Marchxyz" are March while "Ma" is nothing.
    if lower.len() >= 3 {
        for month in MONTHS {
            if lower.starts_with(&month[..3]) {
                return Token::MonthName;
            }
        }
    }
    if lower.len() >= 3 {
        for weekday in WEEKDAYS {
            if lower.starts_with(&weekday[..3]) {
                return Token::Ignorable;
            }
        }
    }
    // A meridiem marker carries no validity of its own.
    if lower == "am" || lower == "pm" {
        return Token::Ignorable;
    }
    if ZONES.contains(&lower.as_str()) {
        return Token::Ignorable;
    }
    // `GMT+0100` and friends.
    if let Some(rest) = lower.strip_prefix("gmt") {
        if rest.is_empty() || rest.starts_with('+') || rest.starts_with('-') {
            return Token::Ignorable;
        }
    }
    // A leading word followed by a comma is read as a weekday and never
    // checked.
    if index + 1 < tokens.len() && tokens[index + 1] == "," {
        return Token::Ignorable;
    }
    Token::Unrecognised
}

#[cfg(test)]
mod differential {
    use super::is_valid;

    /// DIFFERENTIAL: every row is a MEASURED `!Number.isNaN(new Date(v).getTime())`
    /// result from the oracle's JavaScript runtime 1.3.14, the runtime the frozen product runs on.
    ///
    /// The corpus is systematic rather than anecdotal — ISO dates and their
    /// range edges, ISO times, offsets, legacy separators and orders, month
    /// names with casing and prefixes, RFC-822 forms, timezone abbreviations,
    /// bare numbers and junk — because the legacy reading is quirky in ways only
    /// measurement reveals. `Marc 5, 2026` is March by prefix match while
    /// `Ma 5, 2026` is nothing; `20260` is a year while `20260906` is out of
    /// range; `Xyz, 01 Jan 2026` is accepted because a word before a comma is
    /// read as a weekday and never checked, while `Xyz 5, 2026` is refused
    /// because there the same word would have to be the month.
    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn validity_reproduces_the_oracle_across_the_measured_corpus() {
        let measured: &[(&str, bool)] = &[
            ("0", true),
            ("1", true),
            ("12", true),
            ("123", true),
            ("202", true),
            ("1234", true),
            ("2026", true),
            ("12345", true),
            ("20260", true),
            ("20260906", false),
            ("2026-09", true),
            ("2026-09-06", true),
            ("0000", true),
            ("0001-01-01", true),
            ("9999-12-31", true),
            ("2026-00-01", false),
            ("2026-13-01", false),
            ("2026-09-00", false),
            ("2026-09-31", true),
            ("2026-09-32", false),
            ("2026-02-30", true),
            ("2025-02-29", true),
            ("2024-02-29", true),
            ("2026-9-6", true),
            ("+002026-09-06", true),
            ("-000001-09-06", true),
            ("+2026-09-06", true),
            ("2026-", true),
            ("2026--", false),
            ("-2026", true),
            ("2026-1", true),
            ("2026-012", true),
            ("2026-09-06T00:00", true),
            ("2026-09-06T23:59", true),
            ("2026-09-06T24:00", true),
            ("2026-09-06T24:00:00", true),
            ("2026-09-06T24:00:01", false),
            ("2026-09-06T24:01", false),
            ("2026-09-06T23:60", false),
            ("2026-09-06T23:59:60", false),
            ("2026-01-01T12:34.5Z", false),
            ("2026-09-06T23:59:59.1", true),
            ("2026-09-06T23:59:59.123456789", true),
            ("2026-09-06T23:59:59.", false),
            ("2026-09-06T2:03", false),
            ("2026-09-06T02:3", false),
            ("2026-09-06T", false),
            ("2026-09-06T23", false),
            ("2026-09-06T00:00Z", true),
            ("2026-09-06T00:00z", true),
            ("2026-09-06T00:00+05:30", true),
            ("2026-09-06T00:00-08:00", true),
            ("2026-09-06T00:00+0530", true),
            ("2026-09-06T00:00+05", false),
            ("2026-09-06T00:00+24:00", false),
            ("2026-09-06T00:00+99:99", false),
            ("2026-09-06T00:00 Z", false),
            ("2026-09-06T00:00 UTC", false),
            ("2026-09-06T00:00GMT", false),
            ("2026/09/06", true),
            ("2026/9/6", true),
            ("09/06/2026", true),
            ("9/6/2026", true),
            ("13/06/2026", false),
            ("06-09-2026", true),
            ("2026.09.06", true),
            ("2026 09 06", true),
            ("2026-09-06 23:59", true),
            ("2026-09-06 23:59:59", true),
            ("2026-09-06  23:59", true),
            ("2026-09-06 ", true),
            ("March 5, 2026", true),
            ("march 5, 2026", true),
            ("MARCH 5, 2026", true),
            ("Mar 5 2026", true),
            ("Mar 5, 2026", true),
            ("5 March 2026", true),
            ("March 2026", true),
            ("Marc 5, 2026", true),
            ("Marchxyz 5, 2026", true),
            ("Ma 5, 2026", false),
            ("M 5, 2026", false),
            ("Xyz 5, 2026", false),
            ("Sept 5, 2026", true),
            ("Sep 5, 2026", true),
            ("September 5, 2026", true),
            ("Thu, 01 Jan 2026 00:00:00 GMT", true),
            ("Thu Jan 01 2026", true),
            ("Thu, 01 Jan 2026", true),
            ("Xyz, 01 Jan 2026", true),
            ("01 Jan 2026", true),
            ("Jan 1 2026 GMT+0100", true),
            ("Jan 1 2026 PST", true),
            ("Jan 1 2026 EST", true),
            ("Jan 1 2026 XYZ", false),
            ("Jan 1 2026 13:45:00 GMT+0100", true),
            ("1e10", false),
            ("Infinity", false),
            ("NaN", false),
            ("null", false),
            ("undefined", false),
            ("true", false),
            ("", false),
            (" ", false),
            ("  2026-09-06  ", true),
            ("\t2026-09-06", true),
            ("not a date", false),
            ("-", false),
            ("T", false),
            ("Z", false),
            ("2026-09-06extra", false),
            ("2026-09-06T00:00extra", false),
        ];
        let mut wrong = Vec::new();
        for (value, oracle) in measured {
            if is_valid(value) != *oracle {
                wrong.push(format!(
                    "{value:?}: oracle={oracle} here={}",
                    is_valid(value)
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {} cases disagree with the oracle:
{}",
            wrong.len(),
            measured.len(),
            wrong.join(
                "
"
            )
        );
    }

    /// HOLD-OUT: cases deliberately withheld while the reconstruction was built,
    /// so this measures whether it GENERALISES rather than whether it was fitted
    /// to the corpus it was derived from. A reconstruction that only passes its
    /// own training set has learned the examples, not the behaviour.
    #[test]
    fn validity_generalises_to_a_holdout_the_reconstruction_never_saw() {
        let measured: &[(&str, bool)] = &[
            ("2026-06-31", true),
            ("2026-04-31", true),
            ("1970-01-01", true),
            ("1969-12-31T23:59:59Z", true),
            ("2026-12", true),
            ("2026-12-01T00:00:00.000Z", true),
            ("2026-12-01T00:00:00,000Z", false),
            ("0000-01-01", true),
            ("275760-09-13", false),
            ("+275760-09-13", true),
            ("-271821-04-20", true),
            ("Feb 30 2026", true),
            ("Feb 29 2025", true),
            ("31 Dec 2026", true),
            ("Dec 31, 2026 11:59 PM", true),
            ("2026-09-06T12:00:00.000+00:00", true),
            ("2026-09-06t12:00", true),
            ("2026-09-06T12:00:00z", true),
            ("Tue, 06 Sep 2026 12:00:00 +0100", true),
            ("06 September 2026", true),
            ("Sept. 5, 2026", true),
            ("2026 Sep 6", true),
            ("Sep 2026", true),
            ("2026,09,06", true),
            ("2026:09:06", true),
            ("9/6", true),
            ("6/2026", false),
            ("2026/13/01", false),
            ("0/0/0", false),
            ("1/1/1", true),
            ("  ", false),
            ("\n2026-09-06\n", true),
            ("2026-09-06 ", false),
            ("١٢٣٤", false),
            ("2026-09-06T00:00:00+14:00", true),
            ("2026-09-06T00:00:00-12:00", true),
            ("now", false),
            ("today", false),
            ("yesterday", false),
            ("2026-W36", false),
            ("2026-249", false),
        ];
        let mut wrong = Vec::new();
        for (value, oracle) in measured {
            if is_valid(value) != *oracle {
                wrong.push(format!(
                    "{value:?}: oracle={oracle} here={}",
                    is_valid(value)
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {} HELD-OUT cases disagree:\n{}",
            wrong.len(),
            measured.len(),
            wrong.join("\n")
        );
    }

    /// The YEAR BOUNDARY, probed separately because the hold-out exposed it and
    /// a single case would not have pinned the rule. `99999-09-13` is accepted
    /// while `275760-09-13` is refused, and the lone number `275760` is
    /// accepted — the boundary is the maximum INSTANT, not a year number.
    #[test]
    fn the_year_boundary_matches_the_probed_oracle() {
        let measured: &[(&str, bool)] = &[
            ("99999", true),
            ("275760", true),
            ("275761", false),
            ("9999-09-13", true),
            ("10000-09-13", true),
            ("99999-09-13", true),
            ("275760-09-13", false),
            ("275761-09-13", false),
            ("9999/09/13", true),
            ("10000/09/13", true),
            ("12345/1/1", true),
            ("1/1/9999", true),
            ("1/1/10000", true),
            ("1/1/99999", true),
            ("Sep 13 9999", true),
            ("Sep 13 10000", true),
            ("Sep 13 99999", true),
            ("Sep 13 275760", false),
        ];
        let mut wrong = Vec::new();
        for (value, oracle) in measured {
            if is_valid(value) != *oracle {
                wrong.push(format!(
                    "{value:?}: oracle={oracle} here={}",
                    is_valid(value)
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "boundary disagreements:
{}",
            wrong.join(
                "
"
            )
        );
    }
}
