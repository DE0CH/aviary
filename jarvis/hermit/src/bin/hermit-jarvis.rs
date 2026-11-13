//! `hermit-jarvis serve`: the hermit contract on :8080, the gateway listener on :8081, pigeon's models route on
//! :8082, ops on :9090 (aviary/README.md).

use std::sync::Arc;
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_core::{drain, log, ops, version};
use hermit_api::server::{RouteGuard, guarded_router};
use jarvis_aviary::app::{App, Env, Inner, Keys};
use jarvis_aviary::jarvis::Jarvis;

fn build() -> version::Build {
    version::Build::new("hermit")
        .surface(hermit_api::CORE.0, hermit_api::CORE.1)
        .surface(hermit_api::SANDBOX.0, hermit_api::SANDBOX.1)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let build = build();
    if std::env::args().nth(1).as_deref() == Some("--version") {
        std::io::Write::write_all(
            &mut std::io::stdout(),
            format!("{}\n", build.version_line()).as_bytes(),
        )?;
        return Ok(());
    }
    log::init_in_pod(&build, log::Output::Stdout);
    let env = Env::read();
    let callers = Callers::from_env(
        "HERMIT",
        &[
            Caller::Pigeon,
            Caller::Magpie,
            Caller::Gateway,
            Caller::Admin,
        ],
    );
    let model_callers = Callers::from_env("MODEL", &[Caller::Pigeon]);
    let registry = ops::registry(&build);
    let ops = ops::Ops::new(build, registry);
    ops.spawn(ops::ADDR).await?;
    let migrations = ops.readiness().gate("migrations");
    let pool = jarvis_aviary::db::open(&env.database_url).await?;
    migrations.open();
    let http = jarvis_aviary::central::http()?;
    let app = App(Arc::new(Inner {
        pool,
        keys: Keys::parse(&env.token_keys)?,
        magpie: magpie_api::client::Client::new(
            http.clone(),
            "http://magpie",
            aviary_core::auth::outbound_token("HERMIT", "MAGPIE"),
        ),
        jarvis: Jarvis::new(
            &env.jarvis_url,
            &env.jarvis_access_id,
            &env.jarvis_access_secret,
        )?,
        owner: env.owner,
        access_team: env.access_team,
        access_aud: env.access_aud,
        magpie_url: "http://magpie".into(),
        hermit_url: "http://127.0.0.1:8080".into(),
        gateway_to_magpie: aviary_core::auth::outbound_token("GATEWAY", "MAGPIE"),
        gateway_to_hermit: aviary_core::auth::outbound_token("GATEWAY", "HERMIT"),
        http,
        settings: jarvis_aviary::settings::Settings::load(&env.settings)?,
    }));
    let models_route = app.settings.models_route();
    let api = guarded_router(Arc::new(app.clone()), RouteGuard::new(callers));
    let gw = jarvis_aviary::central::router(app.clone());
    let models = axum::Router::new().route(
        gateway_api::models::MODELS,
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let ok = model_callers.identify_headers(&headers).is_some();
            let models_route = models_route.clone();
            async move {
                if ok {
                    axum::response::IntoResponse::into_response(axum::Json(models_route))
                } else {
                    axum::response::IntoResponse::into_response((
                        axum::http::StatusCode::UNAUTHORIZED,
                        axum::Json(serde_json::json!({"code": "unauthenticated", "message": "a pair token is required"})),
                    ))
                }
            }
        }),
    );
    let serve = |router: axum::Router, port: u16| async move {
        let l = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
        tokio::spawn(async move {
            if let Err(e) = axum::serve(l, router).await {
                tracing::error!(error = %e, port, "listener ended");
            }
        });
        Ok::<_, std::io::Error>(())
    };
    serve(api, 8080).await?;
    serve(gw, 8081).await?;
    serve(models, 8082).await?;
    tokio::spawn(jarvis_aviary::reconcile::run(app.clone()));
    ops.readiness().started();
    let mut d = drain::Drain::new(ops.readiness().clone());
    // long-polls answer within 25 s; in-flight forwards get as long as the gateway contract gives magpie and hermit
    d.hook("in-flight requests", Duration::from_secs(60), || async {
        tokio::time::sleep(Duration::from_secs(26)).await;
    });
    let outcome = d.run(&mut drain::Signals::os()?).await;
    drain::exit(outcome)
}
