//! A conversation the deployment closes or ends (src/conversations/delete.rs; Deyao, 2026-11-12): a session whose
//! lifecycle is managed elsewhere. Closed (the session is paused): the person's messages are refused with the
//! deployment's sentence, the deployment's own are still taken, and opening it again takes them as before. Ended (the
//! session is retired): the conversation leaves the person's list.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    dead_code
)]

mod common;

use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use magpie_api::client::ClientError;
use magpie_api::conversations::{CreateRequest, ListQuery};
use magpie_api::errors::ErrorCode;
use magpie_api::ops::HermitSendRequest;
use magpie_api::turns::SendRequest;
use uuid::Uuid;

const ALICE: &str = "alice@example.org";
const PAUSED: &str = "This session is paused. Resume it in Jarvis 4 to go on.";

fn msg(cmid: &str, text: &str) -> SendRequest {
    SendRequest {
        client_message_id: cmid.into(),
        text: text.into(),
        images: Vec::new(),
        files: Vec::new(),
    }
}

fn code(e: &ClientError) -> ErrorCode {
    e.code().cloned().unwrap()
}

#[tokio::test]
async fn a_closed_conversation_refuses_its_person_and_an_ended_one_leaves_the_list() {
    let db = common::fresh_db("closed").await;
    let t = common::Tokens::new();
    let hermit = FakeHermit::unguarded(HermitConfig::default()).serve().await.unwrap();
    let a = common::replica_with(&db.url, &t, &hermit.url(), common::idle()).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let deployment = a.hermit();
    let deployment = deployment.service(None);
    let key = Uuid::new_v4().to_string();
    let conv = alice
        .create_conversation(&CreateRequest {
            idempotency_key: key.clone(),
            message: msg(&format!("{key}:m"), "hello"),
        })
        .await
        .unwrap()
        .get()
        .conversation
        .id;
    alice.cancel_turn(conv).await.unwrap();
    assert_eq!(alice.get_conversation(conv).await.unwrap().closed, None);

    // closed: the conversation says why, and the person's message is refused with that sentence
    let c = deployment.hermit_close_conversation(conv, Some(PAUSED)).await.unwrap();
    assert_eq!(c.closed.as_deref(), Some(PAUSED));
    assert_eq!(alice.get_conversation(conv).await.unwrap().closed.as_deref(), Some(PAUSED));
    let e = alice.send_turn(conv, &msg("m2", "are you there?")).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::ConversationClosed);
    assert!(e.to_string().contains(PAUSED), "{e}");
    // closing twice changes nothing; an empty reason is no reason
    deployment.hermit_close_conversation(conv, Some(PAUSED)).await.unwrap();
    let e = deployment.hermit_close_conversation(conv, Some("")).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadRequest);
    // the deployment's own message is still taken (a wakeup resumes the session)
    deployment
        .hermit_send_turn(
            conv,
            &HermitSendRequest {
                person: ALICE.into(),
                message: msg("wakeup-1", "Scheduled wakeup"),
            },
        )
        .await
        .unwrap();
    alice.cancel_turn(conv).await.unwrap();

    // open again: the person's message starts a turn
    let c = deployment.hermit_close_conversation(conv, None).await.unwrap();
    assert_eq!(c.closed, None);
    alice.send_turn(conv, &msg("m3", "back")).await.unwrap();
    alice.cancel_turn(conv).await.unwrap();

    // ended: it leaves the person's list (it sits in Recently deleted until the purge)
    let c = deployment.hermit_delete_conversation(conv).await.unwrap();
    assert!(c.deleted_at.is_some());
    let list = alice.list_conversations(&ListQuery::default()).await.unwrap();
    assert!(list.conversations.iter().all(|c| c.id != conv));
    // someone else's id is not found
    let e = deployment.hermit_delete_conversation(Uuid::new_v4()).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
    let e = deployment
        .hermit_close_conversation(Uuid::new_v4(), Some(PAUSED))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
}
