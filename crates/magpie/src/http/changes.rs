//! M12.

use magpie_api::changes::{ChangesQuery, ChangesResponse};
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::changes;
use crate::conversations::Person;

/// M12.
pub async fn changes(app: &Magpie, ctx: Ctx, q: ChangesQuery) -> Answer<ChangesResponse> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            changes::read(app.read(), &person.email, q.after, q.limit, q.wait_ms).await
        }
        .await,
    )
}
