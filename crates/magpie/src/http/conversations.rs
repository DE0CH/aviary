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

/// `GET /v1/conversations/{id}/commands`: the slash commands its Claude Code last named (404 for another's).
pub async fn commands(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<magpie_api::sandbox::CommandsList> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            let conn = app.pool.get().await?;
            let row = conn
                .query_opt(
                    "select commands from conversations where id = $1 and person = $2",
                    &[&id, &person.email],
                )
                .await?
                .ok_or_else(crate::error::Error::not_found)?;
            let commands = match row.get::<_, Option<serde_json::Value>>(0) {
                Some(v) => serde_json::from_value(v)?,
                None => Vec::new(),
            };
            Ok(magpie_api::sandbox::CommandsList { commands })
        }
        .await,
    )
}

/// M3 (and the first turn's driver on this replica, when the create admitted one).
pub async fn create(app: &Magpie, ctx: Ctx, mut body: CreateRequest) -> Answer<Admitted<CreateResponse>> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            create::validate(&body)?;
            // old asset paths in the first message (the legacy-path set, until A7), as M7's; a repeat admits nothing
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
