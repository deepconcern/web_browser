//! Date parsing and wall-clock time.
//!
//! [RFC 6265bis §5.1.1](https://datatracker.ietf.org/doc/html/draft-ietf-httpbis-rfc6265bis#section-5.1.1)
//! defines a lenient date algorithm for cookies. It also accepts every
//! HTTP-date format of [RFC 9110 §5.6.7](https://www.rfc-editor.org/rfc/rfc9110#section-5.6.7)
//! (IMF-fixdate, RFC 850 and asctime), so the cache uses it too.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, now.
pub(crate) fn now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Days from 1970-01-01 to the given proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
    }
}

fn is_delimiter(b: u8) -> bool {
    matches!(b, 0x09 | 0x20..=0x2F | 0x3B..=0x40 | 0x5B..=0x60 | 0x7B..=0x7E)
}

/// Parses `1*max DIGIT` at the start of `token`, requiring that it is
/// followed by end-of-token or a non-digit. Returns the value and the rest.
fn leading_digits(token: &[u8], min: usize, max: usize) -> Option<(u32, &[u8])> {
    let n = token.iter().take_while(|b| b.is_ascii_digit()).count();
    if n < min || n > max {
        return None;
    }
    let (digits, rest) = token.split_at(n);
    let mut v: u32 = 0;
    for d in digits {
        v = v * 10 + u32::from(d - b'0');
    }
    Some((v, rest))
}

fn parse_time(token: &[u8]) -> Option<(u32, u32, u32)> {
    let (h, rest) = leading_digits(token, 1, 2)?;
    let rest = rest.strip_prefix(b":")?;
    let (m, rest) = leading_digits(rest, 1, 2)?;
    let rest = rest.strip_prefix(b":")?;
    let (s, _) = leading_digits(rest, 1, 2)?;
    Some((h, m, s))
}

fn parse_month(token: &[u8]) -> Option<u32> {
    let prefix = token.get(..3)?.to_ascii_lowercase();
    const MONTHS: [&[u8]; 12] = [
        b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov",
        b"dec",
    ];
    let i = MONTHS.iter().position(|m| *m == prefix.as_slice())?;
    u32::try_from(i + 1).ok()
}

/// Parses a cookie date (RFC 6265bis §5.1.1). Returns Unix seconds.
pub(crate) fn parse_cookie_date(input: &str) -> Option<i64> {
    let mut time = None;
    let mut day = None;
    let mut month = None;
    let mut year = None;
    for token in input.as_bytes().split(|b| is_delimiter(*b)) {
        if token.is_empty() {
            continue;
        }
        if time.is_none()
            && let Some(t) = parse_time(token)
        {
            time = Some(t);
            continue;
        }
        if day.is_none()
            && let Some((d, _)) = leading_digits(token, 1, 2)
        {
            day = Some(d);
            continue;
        }
        if month.is_none()
            && let Some(m) = parse_month(token)
        {
            month = Some(m);
            continue;
        }
        if year.is_none()
            && let Some((y, _)) = leading_digits(token, 2, 4)
        {
            year = Some(y);
            continue;
        }
    }
    let (hour, minute, second) = time?;
    let (day, month, mut year) = (day?, month?, year?);
    if (70..=99).contains(&year) {
        year += 1900;
    } else if year <= 69 {
        year += 2000;
    }
    let year = i64::from(year);
    if !(1..=31).contains(&day) || year < 1601 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    if day > days_in_month(year, month) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_date_formats() {
        let t = 784_111_777; // 1994-11-06T08:49:37Z
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(t));
        assert_eq!(parse_cookie_date("Sunday, 06-Nov-94 08:49:37 GMT"), Some(t));
        assert_eq!(parse_cookie_date("Sun Nov  6 08:49:37 1994"), Some(t));
    }

    #[test]
    fn lenient_cookie_dates() {
        assert_eq!(parse_cookie_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_cookie_date("1 jan 1970 0:0:0"), Some(0));
        assert_eq!(
            parse_cookie_date("Wed, 09 Jun 2021 10:18:14 GMT"),
            Some(1_623_233_894)
        );
        assert_eq!(
            parse_cookie_date("09 June 21 10:18:14"),
            Some(1_623_233_894)
        );
        assert_eq!(
            parse_cookie_date("Sat, 29 Feb 2020 00:00:00 GMT"),
            Some(1_582_934_400)
        );
        assert_eq!(
            parse_cookie_date("Fri, 01 Jan 1601 00:00:00 GMT"),
            Some(-11_644_473_600)
        );
    }

    #[test]
    fn invalid_dates() {
        assert_eq!(parse_cookie_date(""), None);
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1994"), None, "no time");
        assert_eq!(parse_cookie_date("Sun, 31 Feb 1994 08:49:37 GMT"), None);
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1600 08:49:37 GMT"), None);
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1994 24:49:37 GMT"), None);
        assert_eq!(parse_cookie_date("Sun, 06 Xyz 1994 08:49:37 GMT"), None);
        assert_eq!(parse_cookie_date("Sun, 32 Nov 1994 08:49:37 GMT"), None);
    }

    #[test]
    fn clock_is_sane() {
        assert!(now() > 1_600_000_000);
    }
}
