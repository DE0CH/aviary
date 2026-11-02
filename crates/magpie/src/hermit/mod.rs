//! hermit as magpie uses it (hermit 3.6): the driver's get-or-start and long-poll are in [`crate::turns::driver`],
//! the watchdog's read in [`crate::turns::sweeper`], the outbox's release and workspace deletion in
//! [`crate::jobs::outbox`]; this module follows hermit's sandbox-events feed ([`feed`]), the one path by which magpie
//! learns that a sandbox died (platform R8: hermit pushes nothing).

pub mod feed;
