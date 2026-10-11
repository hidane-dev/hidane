//! The proleptic Gregorian calendar in UTC, for timestamps.

/// Days since 1970-01-01 to (year, month, day) (H. Hinnant's algorithm).
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// (year, month 1 to 12, day) to days since 1970-01-01. A day past the month's end runs into
/// the next.
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

pub(crate) fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        _ => 28,
    }
}

/// Seconds since the epoch for `timestamp.value('…')`: `yyyy-MM-ddTHH:mm:ss`, an optional
/// fraction and `Z` or `±HH:mm`. Fields are read leniently, as the official emulator does: a
/// month, day or hour too large runs into the next, and digits of the fraction past the ninth
/// are dropped. Returns seconds and nanoseconds, before any range check.
pub(crate) fn parse_rfc3339(s: &str) -> Option<(i128, u32)> {
    let (date, rest) = s.split_once('T')?;
    let mut fields = date.split('-');
    let (year, month, day) = (fields.next()?, fields.next()?, fields.next()?);
    if fields.next().is_some() {
        return None;
    }
    let (time, offset) = if let Some(time) = rest.strip_suffix('Z') {
        (time, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (time, zone) = rest.split_at(at);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (h, m) = zone[1..].split_once(':')?;
        if h.len() != 2 || m.len() != 2 {
            return None;
        }
        (time, sign * (number(h)? * 3600 + number(m)? * 60))
    };
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, fraction),
        None => (time, ""),
    };
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut fields = clock.split(':');
    let (hour, minute, second) = (fields.next()?, fields.next()?, fields.next()?);
    if fields.next().is_some() {
        return None;
    }
    let (year, month, day) = (number(year)?, number(month)?, number(day)?);
    // Months and days past their end roll over.
    let months = year * 12 + month - 1;
    let days = days_from_civil(months.div_euclid(12), months.rem_euclid(12) + 1, 1) + day - 1;
    let seconds = i128::from(days) * 86_400
        + i128::from(number(hour)? * 3600 + number(minute)? * 60 + number(second)?)
        - i128::from(offset);
    let nanos = format!("{:0<9}", &fraction[..fraction.len().min(9)])
        .parse()
        .ok()?;
    Some((seconds, nanos))
}

fn number(s: &str) -> Option<i64> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2020, 1, 2), 18_263);
        assert_eq!(civil_from_days(18_263), (2020, 1, 2));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(1, 1, 1), -719_162);
        assert_eq!(days_in_month(2020, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
    }

    #[test]
    fn rfc3339() {
        let at = |s| parse_rfc3339(s);
        assert_eq!(at("2020-01-02T03:04:05Z"), Some((1_577_934_245, 0)));
        assert_eq!(
            at("2020-01-02T03:04:05.123Z"),
            Some((1_577_934_245, 123_000_000))
        );
        assert_eq!(
            at("2020-01-02T03:04:05+01:00"),
            Some((1_577_934_245 - 3600, 0))
        );
        assert_eq!(at("2020-13-02T03:04:05Z"), at("2021-01-02T03:04:05Z"));
        assert_eq!(at("2020-02-30T00:00:00Z"), at("2020-03-01T00:00:00Z"));
        assert_eq!(
            at("2020-01-02T03:04:05.1234567891Z"),
            at("2020-01-02T03:04:05.123456789Z")
        );
        for bad in [
            "2020-01-02",
            "2020-01-02T03:04:05",
            "2020-01-02 03:04:05Z",
            "2020-01-02t03:04:05z",
            "2020-01-02T03:04:05+0100",
        ] {
            assert_eq!(at(bad), None, "{bad}");
        }
    }
}
