//! The conversation search (Deyao, 2026-11-17): the supervisor's todo and put of its conversation's message vectors, and
//! pigeon's search of a person's messages by their words and by their meaning.
#![allow(
    clippy::assert_is_empty,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    dead_code
)]

mod common;

use common::cc::Sandbox;
use common::ev::{self, ALICE};
use magpie_api::errors::ErrorCode;
use magpie_api::rows::SearchQuery;
use magpie_api::search::{
    ConversationSearchRequest, DIMENSIONS, EmbeddingVector, EmbeddingsPut, EmbeddingsTodoQuery, QueryEmbedding,
};
use serde_json::json;

const MODEL: &str = "test/embedding";
const BOB: &str = "bob@example.com";

/// A unit vector mostly along `axis`, a little along `near` (so two vectors with the same axis are close and others
/// are far: cosine distance 0 for the same axis, about 1 for another).
fn unit(axis: usize, near: Option<usize>) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIMENSIONS];
    v[axis] = 1.0;
    if let Some(n) = near {
        v[n] = 0.2;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in &mut v {
        *x /= norm;
    }
    v
}

fn todo_q(model: &str) -> EmbeddingsTodoQuery {
    EmbeddingsTodoQuery {
        model: model.into(),
        limit: None,
    }
}

fn search(q: &str, vector: Option<Vec<f32>>) -> ConversationSearchRequest {
    ConversationSearchRequest {
        q: q.into(),
        embedding: vector.map(|v| QueryEmbedding {
            model: MODEL.into(),
            vector: v,
        }),
        limit: None,
    }
}

#[tokio::test]
async fn a_conversations_messages_are_embedded_in_its_sandbox_and_found_by_words_and_meaning() {
    let db = common::fresh_db("search").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&a, ALICE).await;
    let id = s.conversation;
    let long = format!(
        "{}\n\n{}",
        "火車票退票要在開車前辦理。".repeat(70),
        "改簽也可以在網上辦理。".repeat(70)
    );
    let seqs = ev::append(
        a.pool(),
        ALICE,
        id,
        vec![
            ev::user_sent("c1", "Where does the shuttle bus to Macau leave from?"),
            ev::assistant("m1", json!([ev::thinking("let me think")])),
            ev::assistant("m1", json!([ev::tool_use("tu1", "Bash")])),
            ev::results(&[("tu1", "shuttle timetable.pdf", false)]),
            ev::assistant(
                "m2",
                json!([ev::text(
                    "The shuttle bus leaves from the Hong Kong Port every 15 minutes."
                )]),
            ),
            ev::prompt("u2", &long),
        ],
    )
    .await;
    let (user, tool, answer, prompt) = (seqs[0], seqs[2], seqs[4], seqs[5]);

    // the todo: the user and assistant messages only (no thinking, tool call or tool result), whole messages
    let todo = s.calls().embeddings_todo(&todo_q(MODEL)).await.unwrap();
    let got: Vec<(u64, u32)> = todo.items.iter().map(|i| (i.seq, i.chunk)).collect();
    assert_eq!(
        got,
        vec![
            (user as u64, 0),
            (answer as u64, 0),
            (prompt as u64, 0),
            (prompt as u64, 1)
        ],
        "{todo:?}"
    );
    assert_eq!(todo.items[0].text, "Where does the shuttle bus to Macau leave from?");
    assert!(todo.items[2].text.ends_with('。') && !todo.items[2].text.contains('\n'));

    // a vector of the wrong length, of a piece that is no message, or of a piece the message lacks is refused
    for (seq, chunk, v) in [
        (user, 0, vec![0.5; 3]),
        (tool, 0, unit(1, None)),
        (user, 1, unit(1, None)),
    ] {
        let e = s
            .calls()
            .embeddings_put(&EmbeddingsPut {
                model: MODEL.into(),
                items: vec![EmbeddingVector {
                    seq: seq as u64,
                    chunk,
                    vector: v,
                }],
            })
            .await
            .unwrap_err();
        assert_eq!(e.code(), Some(&ErrorCode::BadRequest), "{seq}/{chunk}: {e:?}");
    }
    // a sandbox without the lease stores nothing
    let stranger = Sandbox::new(&a, id, uuid::Uuid::new_v4(), ALICE);
    let e = stranger
        .calls()
        .embeddings_put(&EmbeddingsPut {
            model: MODEL.into(),
            items: vec![],
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::LeaseLost));

    // the vectors: the question and the answer about the bus near each other, the long prompt elsewhere
    let vectors = [
        (user, 0, unit(1, Some(2))),
        (answer, 0, unit(1, Some(3))),
        (prompt, 0, unit(7, None)),
        (prompt, 1, unit(8, None)),
    ];
    let put = EmbeddingsPut {
        model: MODEL.into(),
        items: vectors
            .iter()
            .map(|(seq, chunk, v)| EmbeddingVector {
                seq: *seq as u64,
                chunk: *chunk,
                vector: v.clone(),
            })
            .collect(),
    };
    s.calls().embeddings_put(&put).await.unwrap();
    // again: kept as it was
    s.calls().embeddings_put(&put).await.unwrap();
    assert!(
        s.calls()
            .embeddings_todo(&todo_q(MODEL))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // another model's todo is every piece again
    assert_eq!(
        s.calls()
            .embeddings_todo(&todo_q("other/model"))
            .await
            .unwrap()
            .items
            .len(),
        4
    );
    // a page of messages with no piece (a lone no-break space: text to Postgres, whitespace to Rust) holds nothing back
    let blank: Vec<_> = (0..3).map(|i| ev::user_sent(&format!("nb{i}"), "\u{a0}")).collect();
    ev::append(a.pool(), ALICE, id, blank).await;
    let later = ev::append1(a.pool(), ALICE, id, ev::user_sent("c9", "and the ferry?")).await;
    let page = s
        .calls()
        .embeddings_todo(&EmbeddingsTodoQuery {
            model: MODEL.into(),
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(
        page.items.iter().map(|i| i.seq).collect::<Vec<_>>(),
        vec![later as u64],
        "{page:?}"
    );
    s.calls()
        .embeddings_put(&EmbeddingsPut {
            model: MODEL.into(),
            items: vec![EmbeddingVector {
                seq: later as u64,
                chunk: 0,
                vector: unit(40, None),
            }],
        })
        .await
        .unwrap();

    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    // words only: both messages that say "shuttle bus", each at its row (the one the in-conversation search names)
    let r = alice.conversation_search(&search("Shuttle  BUS", None)).await.unwrap();
    assert!(!r.semantic);
    let rows: Vec<u64> = alice
        .search(
            id,
            &SearchQuery {
                q: "shuttle bus".into(),
                cursor: None,
                limit: None,
            },
        )
        .await
        .unwrap()
        .locations
        .iter()
        .map(|l| l.seq)
        .collect();
    let mut hit_rows: Vec<u64> = r.hits.iter().map(|h| h.seq).collect();
    hit_rows.sort_unstable();
    assert_eq!(hit_rows, rows);
    assert!(
        r.hits
            .iter()
            .all(|h| h.matched == "words" && h.conversation_id == id && h.epoch == "1")
    );
    let answer_hit = r.hits.iter().find(|h| h.kind == "assistant").unwrap();
    assert!(
        answer_hit.snippet.contains("shuttle bus leaves"),
        "{}",
        answer_hit.snippet
    );
    // the words must all be there, in any order; a like wildcard is literal
    assert_eq!(
        alice
            .conversation_search(&search("bus macau", None))
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(
        alice
            .conversation_search(&search("bus_", None))
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(
        alice.conversation_search(&search("改簽", None)).await.unwrap().hits[0].kind,
        "user"
    );

    // words and meaning: the message both find comes first
    let r = alice
        .conversation_search(&search("leaves from", Some(unit(1, Some(3)))))
        .await
        .unwrap();
    assert!(r.semantic);
    assert_eq!(r.hits[0].seq, answer_hit.seq);
    assert_eq!(r.hits[0].matched, "both");
    // meaning only: other words, the same subject
    let r = alice
        .conversation_search(&search("ferry timetable", Some(unit(1, Some(2)))))
        .await
        .unwrap();
    assert!(r.semantic);
    assert_eq!(
        r.hits
            .iter()
            .map(|h| (h.kind.as_str(), h.matched.as_str()))
            .collect::<Vec<_>>(),
        vec![("user", "meaning"), ("assistant", "meaning")],
        "{r:?}"
    );
    // a piece past the first is found at its own place in the message
    let r = alice
        .conversation_search(&search("change a ticket", Some(unit(8, None))))
        .await
        .unwrap();
    assert_eq!(r.hits.len(), 1);
    assert!(r.hits[0].snippet.contains("改簽"), "{}", r.hits[0].snippet);
    // nothing near: no meaning hits at all
    let r = alice
        .conversation_search(&search("weather in Oslo", Some(unit(500, None))))
        .await
        .unwrap();
    assert!(r.semantic && r.hits.is_empty(), "{r:?}");
    // another model's vectors are not compared
    let mut other = search("ferry timetable", Some(unit(1, Some(2))));
    other.embedding.as_mut().unwrap().model = "other/model".into();
    let r = alice.conversation_search(&other).await.unwrap();
    assert!(!r.semantic && r.hits.is_empty());

    // another person finds nothing of Alice's
    let bob = pigeon.person(BOB);
    let r = bob
        .conversation_search(&search("shuttle", Some(unit(1, Some(2)))))
        .await
        .unwrap();
    assert!(r.hits.is_empty() && !r.semantic);
    // a bad query
    for q in ["", "   "] {
        let e = alice.conversation_search(&search(q, None)).await.unwrap_err();
        assert_eq!(e.code(), Some(&ErrorCode::BadRequest));
    }
    let e = alice
        .conversation_search(&search("x", Some(vec![1.0; 2])))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::BadRequest));

    // a deleted conversation is left out
    alice.delete_conversation(id).await.unwrap();
    let r = alice
        .conversation_search(&search("shuttle", Some(unit(1, Some(2)))))
        .await
        .unwrap();
    assert!(r.hits.is_empty(), "{r:?}");
}
