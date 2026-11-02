//! The canary against the CI services (ci.yml test job): a TLS connection to the production Postgres image, and an S3
//! round trip through the AWS SDK on its rustls/ring HTTP client. Test code reads `AVIARY_TEST_*` directly
//! (platform 6.1); the test job always sets them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;

#[allow(clippy::disallowed_methods)] // test-only configuration (platform 6.1)
fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set (ci.yml's test job sets it)"))
}

#[tokio::test]
async fn postgres_over_tls() {
    let url = env("AVIARY_TEST_DATABASE_URL");
    let (client, connection) = tokio_postgres::connect(&url, aviary_tls_canary::postgres_tls().unwrap())
        .await
        .unwrap();
    tokio::spawn(connection);
    let row = client
        .query_one("select ssl from pg_stat_ssl where pid = pg_backend_pid()", &[])
        .await
        .unwrap();
    assert!(row.get::<_, bool>(0), "connection is not encrypted");
}

#[tokio::test]
async fn s3_round_trip() {
    let shared = aws_config::defaults(BehaviorVersion::latest())
        .http_client(aviary_tls_canary::aws_http_client())
        .region(Region::new(env("AVIARY_TEST_S3_REGION")))
        .endpoint_url(env("AVIARY_TEST_S3_ENDPOINT"))
        .credentials_provider(Credentials::new(
            env("AVIARY_TEST_S3_ACCESS_KEY_ID"),
            env("AVIARY_TEST_S3_SECRET_ACCESS_KEY"),
            None,
            None,
            "aviary-test",
        ))
        .load()
        .await;
    let s3 = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(true)
            .build(),
    );
    let bucket = format!("tls-canary-{}", std::process::id());
    s3.create_bucket().bucket(&bucket).send().await.unwrap();
    s3.put_object()
        .bucket(&bucket)
        .key("a/b")
        .body(ByteStream::from_static(b"canary"))
        .send()
        .await
        .unwrap();
    let got = s3.get_object().bucket(&bucket).key("a/b").send().await.unwrap();
    assert_eq!(got.body.collect().await.unwrap().into_bytes().as_ref(), b"canary");
    s3.delete_object().bucket(&bucket).key("a/b").send().await.unwrap();
    s3.delete_bucket().bucket(&bucket).send().await.unwrap();
}
