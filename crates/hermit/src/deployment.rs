//! H1 `GET /v1/deployment` (hermit 3.3): served from memory (`hermit.json`), never touches Postgres.

use hermit_api::deployment::{AccessApp, Deployment, Product, SignIn};

use crate::config::Config;

/// The deployment facts of the configuration.
#[must_use]
pub fn of(cfg: &Config) -> Deployment {
    let f = &cfg.file;
    Deployment {
        product: Product {
            name: f.product.name.clone(),
            locale: f.product.locale.clone(),
        },
        sign_in: SignIn {
            access: f
                .sign_in
                .access
                .iter()
                .map(|a| AccessApp {
                    team_domain: a.team_domain.clone(),
                    audience: a.audience.clone(),
                })
                .collect(),
            app_scheme: f.sign_in.app_scheme.clone(),
        },
    }
}
