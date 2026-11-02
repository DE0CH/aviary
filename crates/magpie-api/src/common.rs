//! Shared wire conventions: timestamps, the empty object, `0|1` query flags, and open string enumerations.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

/// A point in time on the wire: RFC 3339 UTC with milliseconds, `2026-11-02T04:05:06.789Z` (platform 3.6).
///
/// Parsing accepts any RFC 3339 time and converts it to UTC; writing always uses the millisecond form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub OffsetDateTime);

const WIRE: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

impl Timestamp {
    /// Now, in UTC.
    #[must_use]
    pub fn now() -> Self {
        Timestamp(OffsetDateTime::now_utc())
    }

    /// Parse an RFC 3339 time.
    ///
    /// # Errors
    /// When `s` is not RFC 3339.
    pub fn parse(s: &str) -> Result<Self, time::error::Parse> {
        OffsetDateTime::parse(s, &Rfc3339).map(|t| Timestamp(t.to_offset(UtcOffset::UTC)))
    }

    /// The wire form.
    #[must_use]
    pub fn to_wire(&self) -> String {
        // the format has no fallible component for a UTC time; an out-of-range year is the only failure
        self.0
            .to_offset(UtcOffset::UTC)
            .format(WIRE)
            .unwrap_or_else(|_| String::from("invalid-time"))
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(t: OffsetDateTime) -> Self {
        Timestamp(t)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_wire())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Timestamp::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// The empty JSON object `{}`: the body of routes that answer or take nothing but must be JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

/// The answer of an idempotent create: a new resource (201 or 202) or a repeat of an earlier call (200, same body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admitted<T> {
    /// Created by this call.
    New(T),
    /// The same answer as the earlier call with the same idempotency key.
    Repeat(T),
}

impl<T> Admitted<T> {
    /// The body.
    pub fn into_inner(self) -> T {
        match self {
            Admitted::New(t) | Admitted::Repeat(t) => t,
        }
    }

    /// The body, by reference.
    pub fn get(&self) -> &T {
        match self {
            Admitted::New(t) | Admitted::Repeat(t) => t,
        }
    }

    /// Created by this call.
    pub fn is_new(&self) -> bool {
        matches!(self, Admitted::New(_))
    }
}

/// `0|1` query flags (`create=0|1`, `inline=1`, `tail=1`): written as `1`/`0`, read from `1`, `0`, `true` or `false`.
pub mod flag01 {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialise a flag as `1` or `0`.
    ///
    /// # Errors
    /// Never, beyond the serializer's own.
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` signature
    pub fn serialize<S: Serializer>(v: &bool, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(if *v { "1" } else { "0" })
    }

    /// Read a flag.
    ///
    /// # Errors
    /// When the value is not one of `1`, `0`, `true`, `false`.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" | "" => Ok(false),
            other => Err(serde::de::Error::custom(format!("expected 0 or 1, got {other:?}"))),
        }
    }

    /// `skip_serializing_if` helper: leave a false flag out of the query.
    #[must_use]
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` signature
    pub fn is_false(v: &bool) -> bool {
        !*v
    }
}

/// An enumeration of wire strings that magpie may extend: every known value is a variant, anything else is kept
/// verbatim in `Other`, so a reader of an older build keeps working and a round trip never loses a value.
macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this build does not know, kept as received.
            Other(String),
        }

        impl $name {
            /// Every value this build knows.
            pub const KNOWN: &'static [$name] = &[$($name::$variant),+];

            /// The wire string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $( $name::$variant => $wire, )+
                    $name::Other(s) => s.as_str(),
                }
            }

            /// From the wire string (never fails: unknown values become `Other`).
            #[must_use]
            pub fn from_wire(s: &str) -> Self {
                match s {
                    $( $wire => $name::$variant, )+
                    other => $name::Other(other.to_owned()),
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <::std::borrow::Cow<'de, str> as ::serde::Deserialize>::deserialize(d)?;
                Ok($name::from_wire(&s))
            }
        }
    };
}

pub(crate) use open_enum;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_written_with_milliseconds_in_utc() {
        let t = Timestamp::parse("2026-11-02T12:05:06.789123+08:00").unwrap();
        assert_eq!(t.to_wire(), "2026-11-02T04:05:06.789Z");
        let t = Timestamp::parse("2026-11-02T04:05:06Z").unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"2026-11-02T04:05:06.000Z\"");
        assert!(Timestamp::parse("yesterday").is_err());
    }
}
