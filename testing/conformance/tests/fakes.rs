//! The gateway suite's fakes: hermit's own listener really refuses connects during the outage's refuse phase.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use aviary_conformance::fakes::{Expected, Fakes, Outage};

#[tokio::test]
async fn the_refuse_phase_refuses_connects_on_hermits_own_listener() {
    let fakes = Fakes::new(
        Expected {
            magpie: "m".into(),
            hermit: "h".into(),
            tools: "t".into(),
            model: "x".into(),
            route: None,
        },
        Vec::new(),
    );
    let addr = fakes.serve_hermit("127.0.0.1:0".parse().unwrap()).unwrap();
    // a kept-alive connection is closed by the outage too
    let kept = tokio::net::TcpStream::connect(addr).await.unwrap();
    fakes.set_outage(Outage::Refuse).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let e = tokio::net::TcpStream::connect(addr).await.unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::ConnectionRefused, "{e}");
    let mut buf = [0u8; 1];
    kept.readable().await.unwrap();
    assert_eq!(kept.try_read(&mut buf).unwrap_or(0), 0, "the kept connection is closed");
    // back up on the same port
    fakes.set_outage(Outage::Hang).unwrap();
    tokio::net::TcpStream::connect(addr).await.unwrap();
    fakes.set_outage(Outage::Up).unwrap();
    tokio::net::TcpStream::connect(addr).await.unwrap();
}
