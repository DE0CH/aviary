//! Times on the wire: RFC 3339 UTC with exactly three fraction digits and `Z` (`2026-11-02T04:05:06.789Z`, platform
//! 3.6). The monitor compares these strings lexically (hermit 18.3), so the format is part of the contract.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

/// A point in time, kept to the millisecond, serialised as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
///
/// Parsing accepts any RFC 3339 time (any offset, any number of fraction digits) and normalises it to UTC
/// milliseconds; serialising always writes the one canonical form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

const WIRE: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

const HTTP_DATE: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT");

impl Timestamp {
    /// The current time, truncated to the millisecond.
    #[must_use]
    pub fn now() -> Self {
        Self::from_datetime(OffsetDateTime::now_utc())
    }

    /// Truncates to the millisecond and converts to UTC.
    #[must_use]
    pub fn from_datetime(at: OffsetDateTime) -> Self {
        let at = at.to_offset(UtcOffset::UTC);
        let millis = at.millisecond();
        // replace_millisecond cannot fail for a value taken from the same datetime
        Self(at.replace_millisecond(millis).unwrap_or(at))
    }

    /// From milliseconds since the Unix epoch; `None` outside the representable range.
    #[must_use]
    pub fn from_unix_millis(millis: i64) -> Option<Self> {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000)
            .ok()
            .map(Self)
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub fn unix_millis(self) -> i64 {
        // within i64 for every year time can represent
        i64::try_from(self.0.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
    }

    /// Seconds since the Unix epoch.
    #[must_use]
    pub fn unix_seconds(self) -> i64 {
        self.0.unix_timestamp()
    }

    /// The inner `time` value (UTC).
    #[must_use]
    pub fn datetime(self) -> OffsetDateTime {
        self.0
    }

    /// Parses any RFC 3339 time.
    ///
    /// # Errors
    /// When `s` is not RFC 3339.
    pub fn parse(s: &str) -> Result<Self, time::error::Parse> {
        OffsetDateTime::parse(s, &Rfc3339).map(Self::from_datetime)
    }

    /// The HTTP date form (`Sun, 02 Nov 2026 04:05:06 GMT`) for `last-modified`.
    #[must_use]
    pub fn http_date(self) -> String {
        self.0.format(HTTP_DATE).unwrap_or_default()
    }

    /// Parses the HTTP date form.
    ///
    /// # Errors
    /// When `s` is not an IMF-fixdate.
    pub fn parse_http_date(s: &str) -> Result<Self, time::error::Parse> {
        time::PrimitiveDateTime::parse(s, HTTP_DATE).map(|t| Self(t.assume_utc()))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.format(WIRE).map_err(|_| fmt::Error)?;
        f.write_str(&s)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(at: OffsetDateTime) -> Self {
        Self::from_datetime(at)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::Timestamp;

    #[test]
    fn canonical_form() {
        let t = Timestamp::parse("2026-11-02T12:05:06.789123+08:00").unwrap();
        assert_eq!(t.to_string(), "2026-11-02T04:05:06.789Z");
        let t = Timestamp::parse("2026-11-02T04:05:06Z").unwrap();
        assert_eq!(t.to_string(), "2026-11-02T04:05:06.000Z");
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"2026-11-02T04:05:06.000Z\"");
        assert_eq!(Timestamp::from_unix_millis(t.unix_millis()), Some(t));
    }

    #[test]
    fn http_date_round_trips() {
        let t = Timestamp::parse("2026-11-02T04:05:06Z").unwrap();
        assert_eq!(t.http_date(), "Mon, 02 Nov 2026 04:05:06 GMT");
        assert_eq!(Timestamp::parse_http_date(&t.http_date()).unwrap(), t);
    }

    #[test]
    fn refuses_non_rfc3339() {
        assert!(serde_json::from_str::<Timestamp>("\"yesterday\"").is_err());
        assert!(serde_json::from_str::<Timestamp>("1700000000").is_err());
    }
}
