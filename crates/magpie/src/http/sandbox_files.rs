//! The supervisor's sent-file snapshots (magpie 11.2).

use magpie_api::sandbox::{SentFilesCommitted, SentFilesRequest, SentFilesResponse};
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::files;

/// `POST /v1/sandbox/sent-files`.
pub async fn sent_files(app: &Magpie, ctx: Ctx, body: SentFilesRequest) -> Answer<SentFilesResponse> {
    answer(files::sent_files(app, &ctx, &body).await)
}

/// `POST /v1/sandbox/sent-files/{toolUseId}/commit`.
pub async fn commit(app: &Magpie, ctx: Ctx, tool_use_id: String) -> Answer<SentFilesCommitted> {
    answer(files::sent_files_commit(app, &ctx, &tool_use_id).await)
}
