//! M9 (timeline), the live state, M10 (prompts) and search: reads of a person's conversation (a deleted one
//! included), allowed while viewing as.

use magpie_api::live::LiveState;
use magpie_api::rows::{PromptsResponse, SearchQuery, SearchResponse, TimelinePage, TimelineQuery};
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::live;
use crate::rows::read;

/// M9.
pub async fn timeline(app: &Magpie, ctx: Ctx, id: Uuid, q: TimelineQuery) -> Answer<TimelinePage> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            read::timeline(app.reader(), id, Some(&person.email), &q).await
        }
        .await,
    )
}

/// `GET /v1/conversations/{id}/live`.
pub async fn live(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<Option<LiveState>> {
    answer(async { live::get(&app.pool, id, &Person::from_ctx(&ctx)?.email).await }.await)
}

/// M10.
pub async fn prompts(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<PromptsResponse> {
    answer(async { read::prompts(&app.pool, id, Some(&Person::from_ctx(&ctx)?.email)).await }.await)
}

/// Search.
pub async fn search(app: &Magpie, ctx: Ctx, id: Uuid, q: SearchQuery) -> Answer<SearchResponse> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            read::search(&app.pool, id, Some(&person.email), &q.q, q.cursor.as_deref(), q.limit).await
        }
        .await,
    )
}
