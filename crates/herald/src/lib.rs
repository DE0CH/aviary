//! herald: Telegram notification channels for a conversation (crates/herald/README.md).
//!
//! A sandbox asks for a channel; herald creates a private broadcast channel as its Telegram account, makes its bot an
//! admin that may post, and answers the channel's handle and invite link. Posting goes through the bot (Bot API).
//! A handle is the channel's chat id signed for the conversation that created it, so only that conversation can post
//! to it, and herald keeps no state of its own.

pub mod app;
pub mod handle;
pub mod session;
pub mod telegram;
