//! M1, M2, M3, M5, M6.

use magpie_api::Admitted;
use magpie_api::conversations::{Conversation, CreateRequest, CreateResponse, ListQuery, ListResponse, RenameRequest};
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::{Person, create, delete, list, rename};
use crate::legacy;

/// M1.
pub async fn list(app: &Magpie, ctx: Ctx, q: ListQuery) -> Answer<ListResponse> {
    answer(async { list::list(&app.pool, &Person::from_ctx(&ctx)?, q).await }.await)
}

/// M2.
pub async fn get(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(async { list::get(&app.pool, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// M3 (and the first turn's driver on this replica, when the create admitted one).
pub async fn create(app: &Magpie, ctx: Ctx, mut body: CreateRequest) -> Answer<Admitted<CreateResponse>> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            create::validate(&body)?;
            // the switch window's old asset paths in the first message (until A7), as M7's; a repeat admits nothing
            if legacy::paths::has_legacy_path(&person, &body.message)
                && !create::exists(&app.pool, &person, &body.idempotency_key).await?
            {
                legacy::paths::resolve(app, &person, &mut body.message).await?;
            }
            let r = create::create(&app.pool, &person, body)
                .await
                .inspect_err(|e| super::turns::unknown_upload(app, e))?;
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
    answer(
        async {
            let (person, body) = (Person::from_ctx(&ctx)?, &body);
            legacy::fence::native(app, id, || rename::rename(&app.pool, &person, id, body.clone())).await
        }
        .await,
    )
}

/// M6 delete.
pub async fn delete(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            legacy::fence::native(app, id, || delete::delete(&app.pool, &person, id)).await
        }
        .await,
    )
}

/// M6 restore.
pub async fn restore(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            legacy::fence::native(app, id, || delete::restore(&app.pool, &person, id)).await
        }
        .await,
    )
}

/// M6 delete for good.
pub async fn purge(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<()> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            legacy::fence::native(app, id, || delete::purge(&app.pool, &person, id)).await
        }
        .await,
    )
}
