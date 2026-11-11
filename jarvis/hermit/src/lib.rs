//! Jarvis 4's side of the aviary harness (aviary/README.md): Jarvis 4 sessions run DE0CH/aviary's supervisor, and the
//! services tenant runs aviary's pigeon and magpie; this crate is what makes a Jarvis 4 session an aviary sandbox.
//!
//! - `hermit-jarvis` (the services tenant): the hermit contract ([`hermit`]), the central half of the sandbox gateway
//!   and the machines' boot ([`central`]), the sessions' truth from the router ([`reconcile`], [`jarvis`]).
//! - `aviary-gateway` (each session machine): the machine's half of the gateway ([`machine`]).

pub mod app;
pub mod central;
pub mod db;
pub mod hermit;
pub mod jarvis;
pub mod machine;
pub mod reconcile;
pub mod wire;
