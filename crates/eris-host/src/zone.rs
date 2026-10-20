//! The clock the operator reads.
//!
//! Timestamps are stored in UTC, which is the only form that can be re-read
//! correctly later. Everything shown is converted through one offset, resolved
//! once per page, so a transcript never mixes two clocks.
//!
//! The zone is a setting with three shapes: the host's own zone, UTC, or an
//! explicit offset. Host zone is the default and the only one that follows
//! daylight saving, because the conversion is delegated to the operating
//! system's zone database rather than to a table of ours that would go stale
//! twice a year. An explicit offset is for the operator who wants a different
//! clock than the machine's, and the settings page says plainly that a fixed
//! offset does not move when the clocks do.

use crate::db::Db;

/// The `settings` row that holds the operator's choice.
pub const SETTING_TIMEZONE: &str = "display.timezone";

/// Follow the host's own zone, daylight saving included.
pub const HOST: &str = "host";

/// Where a stamp is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The machine's zone, resolved by the operating system per query.
    Host,
    /// Coordinated Universal Time.
    Utc,
    /// A fixed number of minutes east of UTC.
    Fixed(i64),
}

impl Zone {
    /// Parse a stored value, falling back to the host zone.
    ///
    /// Anything unrecognized means the host: a corrupt row must not silently
    /// move every clock in the product.
    #[must_use]
    pub fn parse(stored: Option<&str>) -> Self {
        let Some(raw) = stored.map(str::trim).filter(|raw| !raw.is_empty()) else {
            return Self::Host;
        };
        match raw.to_ascii_lowercase().as_str() {
            HOST | "" => Self::Host,
            "utc" | "z" | "+00:00" | "-00:00" => Self::Utc,
            _ => Self::parse_offset(raw).unwrap_or(Self::Host),
        }
    }

    /// Parse `+HH:MM`, `-HH:MM`, `+HHMM` or `+HH`.
    ///
    /// A three-digit remainder is refused rather than guessed at: `-240` could
    /// be four hours or two hours forty, and a clock is a bad place to be
    /// creative.
    fn parse_offset(raw: &str) -> Option<Self> {
        let (sign, rest) = match raw.as_bytes().first()? {
            b'+' => (1, &raw[1..]),
            b'-' => (-1, &raw[1..]),
            _ => return None,
        };
        let (hours, minutes) = match rest.split_once(':') {
            Some((hours, minutes)) => (hours, minutes),
            // `+0530`: four digits with no colon are hours then minutes.
            None if rest.len() == 4 => (&rest[0..2], &rest[2..4]),
            None if rest.len() <= 2 => (rest, "0"),
            None => return None,
        };
        let hours: i64 = hours.parse().ok()?;
        let minutes: i64 = minutes.parse().ok()?;
        if minutes >= 60 {
            return None;
        }
        Some(Self::Fixed(sign * (hours * 60 + minutes)))
    }

    /// The SQLite modifier that converts `now` into this zone.
    #[must_use]
    pub fn modifier(self) -> String {
        match self {
            Self::Host => "localtime".to_owned(),
            Self::Utc => "utc".to_owned(),
            Self::Fixed(minutes) => format!("{minutes} minutes"),
        }
    }

    /// The stored spelling, for the settings page to echo back.
    #[must_use]
    pub fn as_str(self) -> String {
        match self {
            Self::Host => HOST.to_owned(),
            Self::Utc => "UTC".to_owned(),
            Self::Fixed(minutes) => {
                let sign = if minutes < 0 { '-' } else { '+' };
                let absolute = minutes.abs();
                format!("{sign}{:02}:{:02}", absolute / 60, absolute % 60)
            }
        }
    }
}

/// Validate what the settings form submitted.
///
/// Separate from [`Zone::parse`] on purpose: parsing a stored row must never
/// fail, but a typo the operator just typed must be named rather than quietly
/// replaced with the host zone.
///
/// # Errors
///
/// The message shown next to the field.
pub fn validate(raw: &str) -> Result<Zone, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Zone::Host);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        HOST => return Ok(Zone::Host),
        "utc" | "z" => return Ok(Zone::Utc),
        _ => {}
    }
    Zone::parse_offset(trimmed).ok_or_else(|| {
        format!(
            "`{trimmed}` is not a zone: use `host`, `UTC`, or an offset like `-04:00` or `+05:30`"
        )
    })
}

/// Minutes east of UTC for a zone, right now.
///
/// One query per page, not one per message: a transcript renders many stamps and
/// they must all agree with each other.
#[must_use]
pub fn offset_minutes(db: &Db, zone: Zone) -> i64 {
    let modifier = zone.modifier();
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT CAST(strftime('%s', 'now', ?1) AS INTEGER)
                  - CAST(strftime('%s', 'now') AS INTEGER)",
            [modifier],
            |row| row.get::<_, i64>(0),
        )
    })
    .map(|seconds| seconds / 60)
    .unwrap_or(0)
}

/// Rewrite a stored UTC stamp as the operator's clock, `HH:MM`.
///
/// No date is shown anywhere in the transcript, so a stamp that crosses midnight
/// reports the new day's time and nothing pretends otherwise.
#[must_use]
pub fn clock_in(stamp: &str, offset_minutes: i64) -> String {
    let Some((_date, time)) = stamp.split_once('T') else {
        return stamp.to_owned();
    };
    if time.len() < 5 {
        return stamp.to_owned();
    }
    let (Ok(hour), Ok(minute)) = (time[0..2].parse::<i64>(), time[3..5].parse::<i64>()) else {
        return stamp.to_owned();
    };

    let shifted = (hour * 60 + minute + offset_minutes).rem_euclid(24 * 60);
    format!("{:02}:{:02}", shifted / 60, shifted % 60)
}

/// The hour of the operator's day, for the greeting.
#[must_use]
pub fn hour_in(offset_minutes: i64) -> u32 {
    let utc = time::OffsetDateTime::now_utc();
    let minutes =
        (utc.hour() as i64 * 60 + utc.minute() as i64 + offset_minutes).rem_euclid(24 * 60);
    ((minutes / 60) % 24) as u32
}

/// The zone this host is configured for, when the symlink can be read.
#[must_use]
pub fn host_zone() -> Option<String> {
    let link = std::fs::read_link("/etc/localtime").ok()?;
    let text = link.to_string_lossy().into_owned();
    text.split("zoneinfo/")
        .nth(1)
        .map(str::to_owned)
        .filter(|zone| !zone.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utc_stamp_moves_by_the_offset() {
        assert_eq!(clock_in("2026-10-20T12:47:09.123Z", -240), "08:47");
        assert_eq!(clock_in("2026-10-20T12:47:09.123Z", 330), "18:17");
        assert_eq!(clock_in("2026-10-20T12:47:09.123Z", 0), "12:47");
    }

    #[test]
    fn a_stamp_that_crosses_midnight_wraps_in_both_directions() {
        assert_eq!(clock_in("2026-10-20T02:30:00Z", -240), "22:30");
        assert_eq!(clock_in("2026-10-20T23:30:00Z", 120), "01:30");
    }

    #[test]
    fn a_stamp_with_no_time_part_is_returned_untouched() {
        assert_eq!(clock_in("not-a-stamp", -240), "not-a-stamp");
        assert_eq!(clock_in("2026-10-20", -240), "2026-10-20");
    }

    #[test]
    fn a_zone_parses_from_every_shape_it_accepts() {
        assert_eq!(Zone::parse(None), Zone::Host);
        assert_eq!(Zone::parse(Some("host")), Zone::Host);
        assert_eq!(Zone::parse(Some("UTC")), Zone::Utc);
        assert_eq!(Zone::parse(Some("-04:00")), Zone::Fixed(-240));
        assert_eq!(Zone::parse(Some("+0530")), Zone::Fixed(330));
        assert_eq!(Zone::parse(Some("+05")), Zone::Fixed(300));
        assert_eq!(
            Zone::parse(Some("-240")),
            Zone::Host,
            "a three-digit offset is ambiguous, so it is refused"
        );
    }

    #[test]
    fn a_corrupt_zone_settles_on_the_host_rather_than_moving_the_clock() {
        assert_eq!(Zone::parse(Some("Mars/Olympus")), Zone::Host);
        assert_eq!(Zone::parse(Some("+04:75")), Zone::Host);
    }

    #[test]
    fn a_submitted_zone_is_named_when_it_is_wrong() {
        assert_eq!(validate("host").expect("host"), Zone::Host);
        assert_eq!(validate("").expect("empty"), Zone::Host);
        assert_eq!(validate("UTC").expect("utc"), Zone::Utc);
        assert_eq!(validate("-04:00").expect("offset"), Zone::Fixed(-240));

        let error = validate("Eastern").expect_err("a name is not an offset");
        assert!(error.contains("Eastern"), "{error}");
        assert!(
            error.contains("host"),
            "the refusal shows the shapes: {error}"
        );
    }

    #[test]
    fn a_zone_round_trips_through_its_stored_spelling() {
        for zone in [Zone::Host, Zone::Utc, Zone::Fixed(-240), Zone::Fixed(330)] {
            assert_eq!(Zone::parse(Some(&zone.as_str())), zone, "{}", zone.as_str());
        }
    }
}
