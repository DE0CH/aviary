//! fake-magpie applies M9's query rule before any scripted answer, as magpie does: `liveAfter` goes with `liveTurn`,
//! and both only with `after`. A pigeon that forgets `liveTurn` fails its test here rather than against magpie.
#![allow(clippy::unwrap_used)] // tests

use aviary_core::auth::Caller;
use aviary_fakes::FakeMagpie;
use magpie_api::client::ClientError;
use magpie_api::rows::TimelineQuery;
use uuid::Uuid;

const CONV: Uuid = Uuid::from_u128(0x0190_f6c2_4a0d_7b2c_8e1f_0a1b_2c3d_4e5f);
const PERSON: &str = "chan.tai.man@example.com";

#[tokio::test]
async fn live_after_without_live_turn_is_a_400_even_when_scripted() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let running = FakeMagpie::new().serve().await.unwrap();
    let c = running.client(reqwest::Client::new(), Caller::Pigeon);
    let status = async |q: TimelineQuery| -> u16 {
        running.fake.script_fixture("timeline", 0);
        match c.person(PERSON).timeline(CONV, &q).await {
            Ok(_) => 200,
            Err(ClientError::Api(e)) => {
                assert_eq!(serde_json::to_value(&e).unwrap()["code"], "bad_request");
                e.status
            }
            Err(e) => panic!("{e:?}"),
        }
    };
    let turn = Uuid::from_u128(7);
    let q = |live_after: Option<u64>, live_turn: Option<Uuid>, after: Option<u64>| TimelineQuery {
        after,
        live_after,
        live_turn,
        ..TimelineQuery::default()
    };
    assert_eq!(status(q(Some(3), None, Some(8))).await, 400);
    assert_eq!(status(q(None, Some(turn), Some(8))).await, 400);
    assert_eq!(status(q(Some(3), Some(turn), None)).await, 400);
    assert_eq!(status(q(Some(3), Some(turn), Some(8))).await, 200);
    assert_eq!(status(q(None, None, Some(8))).await, 200);
}
