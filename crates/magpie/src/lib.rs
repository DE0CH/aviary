//! magpie: conversations, events, turns, files and uploads (magpie spec). The library behind the `magpie` binary;
//! its integration tests run replicas of it in-process against a real Postgres.
//!
//! The internal seam the other magpie packages build on: [`events::append`] (which calls [`rows::project`]),
//! [`changes::emit`] / [`changes::upserted`], [`fanout::notify`] and [`fanout::Fanout::hold`], the conversation row
//! lock [`conversations::lock`], and [`jobs::outbox::enqueue`].

pub mod admin;
pub mod app;
pub mod auth;
pub mod blobs;
pub mod ccfs;
pub mod changes;
pub mod conversations;
pub mod db;
pub mod error;
pub mod events;
pub mod fanout;
pub mod files;
pub mod hermit;
pub mod http;
pub mod jobs;
pub mod legacy;
pub mod limits;
pub mod live;
pub mod rows;
pub mod sandbox;
pub mod schedules;
pub mod shares;
pub mod stats;
pub mod turns;
pub mod uploads;
pub mod viewas;
