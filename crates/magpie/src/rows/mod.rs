//! The timeline projection (magpie 7.3): events into protocol-neutral rows, in the same transaction as the event,
//! under the conversation row lock.
//!
//! WP-11 ships the seam only: [`project`] is called by [`crate::events::append`] for every event and writes no row
//! yet. WP-13 fills it with rules 1-10, row seq and epoch, and sends [`crate::fanout::ROWS`] for every transaction
//! that writes rows or bumps the epoch.

use aviary_pg::Tx;

use crate::error::Result;
use crate::events::Stored;

/// Projects one stored event into the conversation's rows.
///
/// # Errors
/// When a statement fails.
#[allow(clippy::unused_async)] // the seam WP-13 fills; its statements are awaited
pub async fn project(_tx: &Tx<'_>, _event: &Stored) -> Result<()> {
    Ok(())
}
