//! The unit tests' Postgres (`AVIARY_TEST_DATABASE_URL`, platform 6.1): for the internals the public API does not
//! reach (the integration tests in `tests/` give each test a fresh database).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

pub(crate) fn url() -> String {
    aviary_core::env::test_only("AVIARY_TEST_DATABASE_URL")
        .unwrap_or_else(|| panic!("AVIARY_TEST_DATABASE_URL is not set (ci.yml's test job sets it)"))
}

pub(crate) fn pool(app: &str, size: usize) -> crate::Pool {
    crate::connect(&url(), app, size, Duration::from_secs(15)).unwrap()
}
