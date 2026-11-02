//! Enums that grow additively (hermit 3.1): a value this build does not know decodes as the enum's `Unknown`
//! variant (`#[serde(other)]` on a derived `Deserialize`), so an older caller keeps decoding a newer hermit's
//! answers. `Unknown` is never sent: [`wire_enum!`] writes `Serialize` by hand and refuses it, so a value this build
//! could only have received is never passed on as if it were a real one.

/// Gives an enum with an `Unknown` variant `as_str` (the wire value, `"unknown"` for `Unknown` in logs only),
/// `Display`, a `Serialize` that refuses `Unknown`, and `KNOWN` (every other value, in declaration order).
macro_rules! wire_enum {
    ($t:ident { $($v:ident => $s:literal),* $(,)? }) => {
        impl $t {
            /// Every value but `Unknown`.
            pub const KNOWN: &'static [Self] = &[$(Self::$v),*];

            /// The wire value (`"unknown"` for [`Self::Unknown`], which is never sent).
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$v => $s,)*
                    Self::Unknown => "unknown",
                }
            }
        }

        impl ::std::fmt::Display for $t {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $t {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                match self {
                    Self::Unknown => Err(<S::Error as ::serde::ser::Error>::custom(concat!(
                        "an unknown ",
                        stringify!($t),
                        " is never sent"
                    ))),
                    v => s.serialize_str(v.as_str()),
                }
            }
        }
    };
}

pub(crate) use wire_enum;
