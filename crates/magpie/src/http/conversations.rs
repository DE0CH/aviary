//! M1, M2, M3, M5, M6.

use magpie_api::Admitted;
use magpie_api::conversations::{Conversation, CreateRequest, CreateResponse, ListQuery, ListResponse, RenameRequest};
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::{Person, create, delete, list, rename};

/// M1.
pub async fn list(app: &Magpie, ctx: Ctx, q: ListQuery) -> Answer<ListResponse> {
    answer(async { list::list(&app.pool, &Person::from_ctx(&ctx)?, q).await }.await)
}

/// M2.
pub async fn get(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(async { list::get(&app.pool, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// M3 (and the first turn's driver on this replica, when the create admitted one).
pub async fn create(app: &Magpie, ctx: Ctx, body: CreateRequest) -> Answer<Admitted<CreateResponse>> {
    answer(
        async {
            let r = create::create(&app.pool, &Person::from_ctx(&ctx)?, body).await?;
            if let Admitted::New(c) = &r
                && let Some(turn) = &c.turn
            {
                crate::turns::driver::spawn(crate::turns::driver::Driver::of(app), turn.id);
            }
            Ok(r)
        }
        .await,
    )
}

/// M5.
pub async fn rename(app: &Magpie, ctx: Ctx, id: Uuid, body: RenameRequest) -> Answer<Conversation> {
    answer(async { rename::rename(&app.pool, &Person::from_ctx(&ctx)?, id, body).await }.await)
}

/// M6 delete.
pub async fn delete(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(async { delete::delete(&app.pool, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// M6 restore.
pub async fn restore(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(async { delete::restore(&app.pool, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// M6 delete for good.
pub async fn purge(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<()> {
    answer(async { delete::purge(&app.pool, &Person::from_ctx(&ctx)?, id).await }.await)
}
