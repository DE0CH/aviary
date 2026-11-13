//! The supervisor's cc routes (magpie 16.2): the lease and the harness homes as files. Paths are mapped between
//! the caller's form and the stored one here ([`homes`]) and nowhere else.

use std::sync::Arc;

use axum::body::Body;
use axum::response::Response;
use magpie_api::sandbox::{
    CcFileQuery, CcMkdir, CcPathOp, CcRename, CcSetattr, CcSizeVersion, CcTree, CcTruncate, CcVersion, CcWriteQuery,
    LeaseResponse,
};
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::ccfs::{self, Caller, homes, lease, ops, reconstruct};
use crate::error::Error;
use crate::limits;

fn caller(ctx: &Ctx) -> Result<Caller, Error> {
    Caller::from_ctx(ctx)
}

pub async fn lease(app: &Magpie, ctx: Ctx) -> Answer<LeaseResponse> {
    answer(
        async {
            let mut l = lease::lease(app, &caller(&ctx)?).await?;
            l.main_transcript = l.main_transcript.map(|p| homes::shown(&ctx, &p));
            Ok(l)
        }
        .await,
    )
}

pub async fn tree(app: &Magpie, ctx: Ctx) -> Answer<CcTree> {
    answer(
        async {
            let mut t = reconstruct::tree(app, &caller(&ctx)?).await?;
            for f in &mut t.files {
                f.path = homes::shown(&ctx, &f.path);
            }
            Ok(t)
        }
        .await,
    )
}

pub async fn file(app: Arc<Magpie>, ctx: Ctx, mut q: CcFileQuery) -> Answer<Response> {
    answer(
        async {
            q.path = homes::stored(&ctx, &q.path)?;
            reconstruct::file(app, &caller(&ctx)?, &q).await
        }
        .await,
    )
}

pub async fn write(app: &Magpie, ctx: Ctx, mut q: CcWriteQuery, body: Body) -> Answer<CcSizeVersion> {
    answer(
        async {
            let c = caller(&ctx)?;
            q.path = homes::stored(&ctx, &q.path)?;
            // one byte over the cap is enough to refuse with 413
            let data = axum::body::to_bytes(body, limits::CC_WRITE_MAX + 1)
                .await
                .map_err(|_| Error::code(magpie_api::errors::ErrorCode::TooLarge, "a cc write is over 16 MiB"))?;
            ops::write(app, &c, &q, &data).await
        }
        .await,
    )
}

pub async fn truncate(app: &Magpie, ctx: Ctx, mut b: CcTruncate) -> Answer<CcSizeVersion> {
    answer(
        async {
            b.path = homes::stored(&ctx, &b.path)?;
            ops::truncate(app, &caller(&ctx)?, &b).await
        }
        .await,
    )
}

pub async fn rename(app: &Magpie, ctx: Ctx, mut b: CcRename) -> Answer<CcVersion> {
    answer(
        async {
            b.from = homes::stored(&ctx, &b.from)?;
            b.to = homes::stored(&ctx, &b.to)?;
            ops::rename(app, &caller(&ctx)?, &b).await
        }
        .await,
    )
}

pub async fn unlink(app: &Magpie, ctx: Ctx, mut b: CcPathOp) -> Answer<()> {
    answer(
        async {
            b.path = homes::stored(&ctx, &b.path)?;
            ops::unlink(app, &caller(&ctx)?, &b).await
        }
        .await,
    )
}

pub async fn mkdir(app: &Magpie, ctx: Ctx, mut b: CcMkdir) -> Answer<()> {
    answer(
        async {
            b.path = homes::stored(&ctx, &b.path)?;
            ops::mkdir(app, &caller(&ctx)?, &b).await
        }
        .await,
    )
}

pub async fn rmdir(app: &Magpie, ctx: Ctx, mut b: CcPathOp) -> Answer<()> {
    answer(
        async {
            b.path = homes::stored(&ctx, &b.path)?;
            ops::rmdir(app, &caller(&ctx)?, &b).await
        }
        .await,
    )
}

pub async fn setattr(app: &Magpie, ctx: Ctx, mut b: CcSetattr) -> Answer<CcVersion> {
    answer(
        async {
            b.path = homes::stored(&ctx, &b.path)?;
            ops::setattr(app, &caller(&ctx)?, &b).await
        }
        .await,
    )
}

/// Unused: keeps the module's import of `ccfs` meaningful for docs.
#[allow(dead_code)]
fn _doc(_: ccfs::Kind) {}
