//! The `magpie serve` process itself (magpie 18): a termination signal while the start still waits for Postgres
//! ends the process at once, instead of being held until the grace period's SIGKILL.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead as _, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_signal_while_postgres_cannot_be_reached_ends_the_start() {
    // a port nothing listens on: the start retries the migrations every 2 s
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut child = Command::new(env!("CARGO_BIN_EXE_magpie"))
        .arg("serve")
        .env_clear()
        .env(
            "MAGPIE_DATABASE_URL",
            format!("postgres://magpie@127.0.0.1:{port}/magpie"),
        )
        .env("MAGPIE_ACCEPT_PIGEON", "test-pigeon")
        .env("MAGPIE_ACCEPT_GATEWAY", "test-gateway")
        .env("MAGPIE_ACCEPT_HERMIT", "test-hermit")
        .env("MAGPIE_ACCEPT_ADMIN", "test-admin")
        .env("MAGPIE_TOKEN_HERMIT", "test-magpie-hermit")
        .env("MAGPIE_TOKEN_IMPORT", "test-magpie-import")
        .env("POD_NAME", "magpie-binary-test")
        .env("MAGPIE_S3_ENDPOINT", "http://127.0.0.1:1")
        .env("MAGPIE_S3_REGION", "us-east-1")
        .env("MAGPIE_S3_BUCKET", "unused")
        .env("MAGPIE_S3_ACCESS_KEY_ID", "test")
        .env("MAGPIE_S3_SECRET_ACCESS_KEY", "test")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    // wait until the start is in its retry loop
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.contains("migrations not applied yet") {
                let _ = tx.send(());
            }
        }
    });
    if rx.recv_timeout(Duration::from_secs(20)).is_err() {
        let _ = child.kill();
        panic!("the start never reached its retry loop (exit: {:?})", child.try_wait());
    }
    let pid = child.id().to_string();
    assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
    let signalled = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if signalled.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("SIGTERM during the start was ignored");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0), "drained and exited");
    assert!(
        signalled.elapsed() < Duration::from_secs(2),
        "{:?}",
        signalled.elapsed()
    );
}
