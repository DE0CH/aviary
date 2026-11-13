//! The harness: the agent program a conversation's turns run in. A conversation has one at a time; the model a
//! person picks names it, and it can change between turns. Every service names a harness with this type, so a new
//! one is added here once.

use serde::{Deserialize, Serialize};

/// The harnesses a sandbox can run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    /// Claude Code.
    #[default]
    Claude,
    /// Qwen Code.
    Qwen,
}

impl Harness {
    /// Every harness, in declaration order.
    pub const ALL: [Harness; 2] = [Harness::Claude, Harness::Qwen];

    /// The wire name (`claude`, `qwen`): also the directory a harness's files live under in a conversation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Qwen => "qwen",
        }
    }

    /// The harness a wire name is.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.as_str() == s)
    }

    /// Whether this is the default (the one a value written before harnesses existed means): for
    /// `skip_serializing_if`, so older readers see no new member.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for h in Harness::ALL {
            assert_eq!(serde_json::to_value(h).unwrap(), h.as_str());
            assert_eq!(
                serde_json::from_value::<Harness>(serde_json::json!(h.as_str())).unwrap(),
                h
            );
            assert_eq!(Harness::parse(h.as_str()), Some(h));
            assert_eq!(h.to_string(), h.as_str());
        }
        assert_eq!(Harness::parse("codex"), None);
        assert!(serde_json::from_str::<Harness>("\"codex\"").is_err());
    }

    /// A value written before harnesses existed is a Claude Code one.
    #[test]
    fn the_default_is_claude_code() {
        assert_eq!(Harness::default(), Harness::Claude);
        assert!(Harness::Claude.is_default());
        assert!(!Harness::Qwen.is_default());
    }
}
