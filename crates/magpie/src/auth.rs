//! Who may call what (magpie 3): the route-to-caller table is magpie-api's [`magpie_api::routes::ROUTES`]; the pair
//! tokens come from `MAGPIE_ACCEPT_<CALLER>` (platform 3.7).
//!
//! The check is one route layer over magpie-api's router, so it covers every route the router has: an unknown or
//! missing token answers 401 `unauthenticated` (whatever the method), a caller the route does not list 403
//! `wrong_caller`, `HEAD` is checked as the route's `GET`, and any other method the table lacks answers 405 here. Then every
//! `Aviary-*` header the caller may not set is removed (platform 3.5), so a handler can trust what is left:
//! pigeon sets the person, viewer and upload headers, the gateway the sandbox identity and the lease, admin
//! `Aviary-Admin`, hermit none. A person route without `Aviary-Person` answers 400 `bad_request`.

use aviary_core::auth::{Caller, Callers};
use aviary_core::http::ApiError;
use aviary_core::http::headers::{
    AVIARY_ADMIN, AVIARY_LEASE, AVIARY_PERSON, AVIARY_SANDBOX_CONVERSATION, AVIARY_SANDBOX_ID, AVIARY_SANDBOX_PERSON,
    AVIARY_UPLOAD_KIND, AVIARY_UPLOAD_NAME, AVIARY_VIEW_SESSION, AVIARY_VIEWER, strip_aviary,
};
use axum::extract::{MatchedPath, Request, State};
use axum::http::HeaderName;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use magpie_api::routes::{ROUTES, Route};

/// The callers magpie accepts (magpie 1), each from `MAGPIE_ACCEPT_<CALLER>`.
pub const CALLERS: [Caller; 4] = [Caller::Pigeon, Caller::Gateway, Caller::Hermit, Caller::Admin];

/// The pair tokens of every caller, from the environment (a missing or empty variable aborts start).
#[must_use]
pub fn callers_from_env() -> Callers {
    Callers::from_env("MAGPIE", &CALLERS)
}

/// The `Aviary-*` headers (beyond `Aviary-Request` and `Aviary-Protocol`) `caller` may set.
#[must_use]
pub fn allowed_headers(caller: Caller) -> &'static [HeaderName] {
    static PIGEON: [HeaderName; 5] = [
        AVIARY_PERSON,
        AVIARY_VIEWER,
        AVIARY_VIEW_SESSION,
        AVIARY_UPLOAD_NAME,
        AVIARY_UPLOAD_KIND,
    ];
    static GATEWAY: [HeaderName; 4] = [
        AVIARY_SANDBOX_ID,
        AVIARY_SANDBOX_CONVERSATION,
        AVIARY_SANDBOX_PERSON,
        AVIARY_LEASE,
    ];
    static ADMIN: [HeaderName; 1] = [AVIARY_ADMIN];
    match caller {
        Caller::Pigeon => &PIGEON,
        Caller::Gateway => &GATEWAY,
        Caller::Admin => &ADMIN,
        _ => &[],
    }
}

/// The route of a request, by its matched path template and method. `HEAD` is the route's `GET`: axum's `get()`
/// answers `HEAD` with the `GET` handler, so it must pass the same checks.
#[must_use]
pub fn route_of(req: &Request) -> Option<&'static Route> {
    let template = req.extensions().get::<MatchedPath>()?.as_str();
    let method = if req.method() == axum::http::Method::HEAD {
        "GET"
    } else {
        req.method().as_str()
    };
    ROUTES
        .iter()
        .find(|r| r.path == template && r.method.as_str() == method)
}

/// The route layer: caller check, header stripping, the person header on person routes.
pub async fn check(State(callers): State<Callers>, mut req: Request, next: Next) -> Response {
    let Some(caller) = callers.identify_headers(req.headers()) else {
        return ApiError::unauthenticated().into_response();
    };
    let Some(route) = route_of(&req) else {
        // a method the route table does not have (OPTIONS, TRACE, …): no handler serves it, the router answers 405.
        // Nothing reaches a handler without a token and a route that lists its caller.
        return ApiError::new(
            axum::http::StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "this method is not served on this path",
        )
        .into_response();
    };
    if !route.callers.contains(&caller) {
        return ApiError::wrong_caller(caller).into_response();
    }
    strip_aviary(req.headers_mut(), allowed_headers(caller));
    if route.person && caller == Caller::Pigeon {
        let has_person = req
            .headers()
            .get(&AVIARY_PERSON)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().is_empty());
        if !has_person {
            return ApiError::bad_request("Aviary-Person is required").into_response();
        }
    }
    req.extensions_mut().insert(caller);
    next.run(req).await
}
