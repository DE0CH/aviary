//! The fakes' models: magpie's sandbox surface, hermit's contract and the supervisor's control API, driven through
//! the contract clients the way the other packages' tests use them.
#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::many_single_char_names)] // tests

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use aviary_core::auth::Caller;
use aviary_fakes::hermit::HermitConfig;
use aviary_fakes::{FakeHermit, FakeMagpie, FakeSupervisor};
use hermit_api::sandbox::{
    ConversationReleaseReason, Cursor, PutSandboxRequest, ReleaseReason, ReleaseSandboxRequest, SandboxEventsQuery,
    SandboxQuery, SandboxState,
};
use hermit_api::sandbox_surface::{ActivityReplyState, ActivityState};
use hermit_api::token::KeySet;
use hermit_api::workspaces::{CloneRequest, EntriesQuery, FilesQuery};
use hermit_api::{SandboxIdentity, SelfAnswer};
use magpie_api::client::ClientError;
use magpie_api::sandbox::{
    AckRequest, AckResult, CcFileQuery, CcKind, CcMkdir, CcPathOp, CcRename, CcSetattr, CcTruncate, CcWriteQuery,
    CommandBody, CommandsQuery,
};
use supervisor_api::client::ControlClient;
use supervisor_api::control::{Outcome, ReleaseRequest};
use uuid::Uuid;

const CONV: Uuid = Uuid::from_u128(0x0190_f6c2_4a0d_7b2c_8e1f_0a1b_2c3d_4e5f);

fn setup() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn gateway_http(sandbox: &str, conv: Uuid) -> reqwest::Client {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("aviary-sandbox-id", sandbox.parse().unwrap());
    h.insert("aviary-sandbox-conversation", conv.to_string().parse().unwrap());
    h.insert("aviary-sandbox-person", "staff@example.org".parse().unwrap());
    reqwest::Client::builder().default_headers(h).build().unwrap()
}

fn code(r: Result<impl std::fmt::Debug, ClientError>) -> (u16, String) {
    match r {
        Err(ClientError::Api(e)) => (
            e.status,
            serde_json::to_value(&e).unwrap()["code"].as_str().unwrap().to_owned(),
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn write(path: &str, offset: u64, create: bool) -> CcWriteQuery {
    CcWriteQuery {
        path: path.into(),
        offset,
        mtime_ns: 1,
        op_id: Uuid::new_v4(),
        create,
        mode: 0o600,
        turn_id: None,
        append: false,
    }
}

#[tokio::test]
async fn magpie_lease_and_cc_files() {
    setup();
    let running = FakeMagpie::new().serve().await.unwrap();
    let fake = &running.fake;
    let c = running.client(gateway_http("sbx-1", CONV), Caller::Gateway);
    let first = c.sandbox(None).lease().await.unwrap();
    let lease = c.sandbox(None).lease().await.unwrap();
    assert_eq!((first.epoch, lease.epoch), (1, 2));
    assert_eq!(lease.conversation_id, CONV);
    let old = c.sandbox(Some(first.lease_id));
    let s = c.sandbox(Some(lease.lease_id));

    // an earlier lease is dead at once; no lease at all is lost too
    assert_eq!(code(old.cc_mkdir(&mkdir("claude/projects")).await).1, "lease_lost");
    assert_eq!(
        code(c.sandbox(None).cc_mkdir(&mkdir("claude/projects")).await).1,
        "lease_lost"
    );

    // a write into a missing parent is 404 (Claude Code then runs mkdir -p)
    let t = "claude/projects/-workspace/s1.jsonl";
    assert_eq!(
        code(s.cc_write(&write(t, 0, true), b"x\n".to_vec()).await),
        (404, "not_found".into())
    );
    s.cc_mkdir(&mkdir("claude/projects")).await.unwrap();
    assert_eq!(code(s.cc_mkdir(&mkdir("claude/projects")).await).1, "exists");
    s.cc_mkdir(&mkdir("claude/projects/-workspace")).await.unwrap();
    let w = s.cc_write(&write(t, 0, true), b"{\"a\":1}\n".to_vec()).await.unwrap();
    assert_eq!((w.size, w.version), (8, 1));
    let w = s.cc_write(&write(t, 8, false), b"{\"b\":2}\n".to_vec()).await.unwrap();
    assert_eq!((w.size, w.version), (16, 2));
    assert_eq!(
        code(s.cc_write(&write(t, 17, false), b"z".to_vec()).await).1,
        "offset_beyond_eof"
    );
    assert_eq!(
        code(
            s.cc_write(&write("claude/projects/nope", 0, false), b"z".to_vec())
                .await
        )
        .1,
        "not_found"
    );

    // an opId replay answers the stored result and does nothing
    let q = write(t, 16, false);
    let a = s.cc_write(&q, b"{\"c\":3}\n".to_vec()).await.unwrap();
    let b = s.cc_write(&q, b"{\"c\":3}\n".to_vec()).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(fake.node(CONV, t).unwrap().data.len(), 24);
    // an append expects the file's size: a stale one is refused unless its bytes are already there (rule 0)
    let stale = CcWriteQuery {
        append: true,
        ..write(t, 8, false)
    };
    assert_eq!(
        code(s.cc_write(&stale, b"{\"x\":9}\n".to_vec()).await).1,
        "offset_mismatch"
    );
    assert_eq!(s.cc_write(&stale, b"{\"b\":2}\n".to_vec()).await.unwrap().size, 24);
    assert_eq!(fake.node(CONV, t).unwrap().kind, CcKind::Lines);

    // read back, whole and ranged
    let all = s
        .cc_file(&CcFileQuery {
            path: t.into(),
            offset: None,
            length: None,
        })
        .await
        .unwrap();
    assert_eq!(all.status(), 200);
    assert_eq!(all.bytes().await.unwrap().len(), 24);
    let part = s
        .cc_file(&CcFileQuery {
            path: t.into(),
            offset: Some(8),
            length: Some(8),
        })
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(&part.bytes().await.unwrap()[..], b"{\"b\":2}\n");

    // truncate, compaction's rename-over, setattr, the tree
    let tmp = "claude/projects/-workspace/s1.jsonl.compact.tmp.3f";
    s.cc_write(&write(tmp, 0, true), b"{\"x\":0}\n".to_vec()).await.unwrap();
    let tr = s
        .cc_truncate(&CcTruncate {
            path: t.into(),
            size: 8,
            mtime_ns: 2,
            op_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
    assert_eq!(tr.size, 8);
    let rn = s
        .cc_rename(&CcRename {
            from: tmp.into(),
            to: t.into(),
            op_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
    assert!(rn.version > tr.version);
    assert_eq!(fake.node(CONV, t).unwrap().data, b"{\"x\":0}\n");
    assert!(fake.node(CONV, tmp).is_none());
    s.cc_setattr(&CcSetattr {
        path: t.into(),
        mode: Some(0o644),
        mtime_ns: None,
        op_id: Uuid::new_v4(),
    })
    .await
    .unwrap();
    let tree = s.cc_tree().await.unwrap();
    assert_eq!(tree.files.len(), 3);
    assert_eq!(tree.cc_bytes, 8);
    assert_eq!(tree.files.iter().find(|f| f.path == t).unwrap().mode, 0o644);

    // the main transcript is protected once it is known; a non-empty directory stays
    fake.set_provider_session(CONV, "s1");
    let unlink = |p: &str| CcPathOp {
        path: p.into(),
        op_id: Uuid::new_v4(),
    };
    assert_eq!(code(s.cc_unlink(&unlink(t)).await), (403, "protected".into()));
    assert_eq!(code(s.cc_rmdir(&unlink("claude/projects")).await).1, "not_empty");
    s.cc_mkdir(&mkdir("plans")).await.unwrap();
    s.cc_rmdir(&unlink("plans")).await.unwrap();
    assert_eq!(code(s.cc_rmdir(&unlink("plans")).await).1, "not_found");

    // the quota
    fake.set_quota(10);
    assert_eq!(
        code(s.cc_write(&write(t, 8, false), b"{\"q\":1}\n".to_vec()).await),
        (507, "quota_exceeded".into())
    );

    // resync and the main transcript in the next lease
    fake.set_resync(CONV);
    let l = c.sandbox(None).lease().await.unwrap();
    assert!(l.resync);
    assert_eq!(l.main_transcript.as_deref(), Some(t));
    assert!(!c.sandbox(None).lease().await.unwrap().resync);
}

fn mkdir(p: &str) -> CcMkdir {
    CcMkdir {
        path: p.into(),
        mode: 0o700,
        mtime_ns: 1,
        op_id: Uuid::new_v4(),
    }
}

#[tokio::test]
async fn magpie_a_write_of_bytes_already_in_place_changes_nothing() {
    setup();
    let running = FakeMagpie::new().serve().await.unwrap();
    let fake = &running.fake;
    let c = running.client(gateway_http("sbx-1", CONV), Caller::Gateway);
    let lease = c.sandbox(None).lease().await.unwrap();
    let s = c.sandbox(Some(lease.lease_id));
    let t = "s1.jsonl";
    s.cc_write(&write(t, 0, true), b"{\"a\":1}\n".to_vec()).await.unwrap();
    let w = s.cc_write(&write(t, 8, false), b"{\"b\":2}\n".to_vec()).await.unwrap();
    assert_eq!((w.size, w.version), (16, 2));

    // the sentry's resend under a new opId: the same offset and bytes apply once
    let mut resend = write(t, 8, false);
    resend.mtime_ns = 99;
    let again = s.cc_write(&resend, b"{\"b\":2}\n".to_vec()).await.unwrap();
    assert_eq!((again.size, again.version), (16, 2));
    let node = fake.node(CONV, t).unwrap();
    assert_eq!((node.version, node.mtime_ns), (2, 1));
    // any range inside the file that matches, a create flag included
    let mid = s.cc_write(&write(t, 2, true), b"a\":1".to_vec()).await.unwrap();
    assert_eq!((mid.size, mid.version), (16, 2));
    // the rule comes before every other one: the quota, even when it is already exceeded
    fake.set_quota(1);
    let q = s.cc_write(&write(t, 0, false), b"{\"a\":1}\n".to_vec()).await.unwrap();
    assert_eq!((q.size, q.version), (16, 2));
    fake.set_quota(1 << 30);
    // different bytes, or a range running past the end, are ordinary writes
    let other = s.cc_write(&write(t, 8, false), b"{\"b\":3}\n".to_vec()).await.unwrap();
    assert_eq!((other.size, other.version), (16, 3));
    let past = s.cc_write(&write(t, 8, false), b"{\"b\":3}\nX".to_vec()).await.unwrap();
    assert_eq!((past.size, past.version), (17, 4));
}

#[tokio::test]
async fn magpie_a_refused_create_leaves_no_path() {
    setup();
    let running = FakeMagpie::new().serve().await.unwrap();
    let fake = &running.fake;
    let c = running.client(gateway_http("sbx-1", CONV), Caller::Gateway);
    let lease = c.sandbox(None).lease().await.unwrap();
    let s = c.sandbox(Some(lease.lease_id));
    fake.set_quota(5);
    assert_eq!(
        code(s.cc_write(&write("big.json", 0, true), vec![b'x'; 10]).await),
        (507, "quota_exceeded".into())
    );
    assert_eq!(
        code(s.cc_write(&write("far.json", 3, true), b"x".to_vec()).await).1,
        "offset_beyond_eof"
    );
    assert_eq!(fake.paths(CONV), Vec::<String>::new());
    assert_eq!(s.cc_tree().await.unwrap().files.len(), 0);
    // within the quota the same create goes through
    let w = s.cc_write(&write("big.json", 0, true), vec![b'x'; 5]).await.unwrap();
    assert_eq!((w.size, w.version), (5, 1));
}

#[tokio::test]
async fn magpie_sent_files_stage_put_and_commit_once_per_tool_use_id() {
    use magpie_api::sandbox::{SentFileSpec, SentFilesRequest};
    setup();
    let running = FakeMagpie::new().serve().await.unwrap();
    let fake = &running.fake;
    let c = running.client(gateway_http("sbx-1", CONV), Caller::Gateway);
    let lease = c.sandbox(None).lease().await.unwrap();
    let s = c.sandbox(Some(lease.lease_id));
    let have = b"already stored".to_vec();
    let have_sha = fake.seed_blob(&have);
    let new = b"the report".to_vec();
    let new_sha = aviary_fakes_sha(&new);
    let spec = |path: &str, data: &[u8], sha: &str| SentFileSpec {
        path: path.into(),
        size: data.len() as u64,
        sha256: sha.into(),
        mime_type: "text/plain".into(),
    };
    let req = SentFilesRequest {
        tool_use_id: "call_9f01".into(),
        caption: Some("the files".into()),
        files: vec![
            spec("file:///workspace/report/q3.txt", &new, &new_sha),
            spec("/workspace/a.txt", &have, &have_sha),
            spec("/workspace/b.txt", &new, &new_sha),
        ],
    };
    // no lease, no send
    assert_eq!(code(c.sandbox(None).sent_files(&req).await).1, "lease_lost");
    // the answer names each blob magpie lacks once
    assert_eq!(s.sent_files(&req).await.unwrap().missing, vec![new_sha.clone()]);
    // the commit needs every blob
    assert_eq!(code(s.sent_files_commit("call_9f01").await).0, 400);
    s.sandbox_blob_put(&new_sha, new.clone()).await.unwrap();
    assert_eq!(s.sent_files(&req).await.unwrap().missing, Vec::<String>::new());
    let done = s.sent_files_commit("call_9f01").await.unwrap();
    assert_eq!((done.kept, done.seq), (3, 1));
    // idempotent per toolUseId
    assert_eq!(s.sent_files_commit("call_9f01").await.unwrap(), done);
    s.sent_files(&req).await.unwrap();
    assert_eq!(s.sent_files_commit("call_9f01").await.unwrap(), done);
    let sent = fake.sent(CONV);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent["call_9f01"].request.files[0].path, "/workspace/report/q3.txt");
    assert_eq!(sent["call_9f01"].request.caption.as_deref(), Some("the files"));

    // validation
    assert_eq!(
        code(s.sent_files_commit("never-staged").await),
        (404, "not_found".into())
    );
    assert_eq!(code(s.sent_files_commit("bad id!").await).0, 400);
    let mut bad = req.clone();
    bad.tool_use_id = "call_2".into();
    bad.files[0].path = "/workspace/../etc/passwd".into();
    assert_eq!(code(s.sent_files(&bad).await).1, "bad_path");
    bad.files[0].path = "/tmp/x".into();
    assert_eq!(code(s.sent_files(&bad).await).1, "bad_path");
    bad.files[0].path = "/workspace/x".into();
    bad.files[0].size = (64 << 20) + 1;
    assert_eq!(code(s.sent_files(&bad).await).1, "too_large");
    bad.files.clear();
    assert_eq!(code(s.sent_files(&bad).await).0, 400);
    // a later send gets the next seq
    let mut next = req.clone();
    next.tool_use_id = "call_2".into();
    s.sent_files(&next).await.unwrap();
    assert_eq!(s.sent_files_commit("call_2").await.unwrap().seq, 2);
}

#[tokio::test]
async fn magpie_lease_only_for_hermits_current_sandbox() {
    setup();
    let running = FakeMagpie::new().serve().await.unwrap();
    running.fake.set_current_sandbox(CONV, Some("sbx-2"));
    let other = running.client(gateway_http("sbx-1", CONV), Caller::Gateway);
    assert_eq!(
        code(other.sandbox(None).lease().await),
        (409, "not_current_sandbox".into())
    );
    let current = running.client(gateway_http("sbx-2", CONV), Caller::Gateway);
    current.sandbox(None).lease().await.unwrap();
}

#[tokio::test]
async fn magpie_commands_long_poll_and_acks() {
    setup();
    let running = FakeMagpie::new().serve().await.unwrap();
    let fake = running.fake.clone();
    let c = running.client(gateway_http("sbx-1", CONV), Caller::Gateway);
    let lease = c.sandbox(None).lease().await.unwrap();
    let s = c.sandbox(Some(lease.lease_id));

    // a poll with nothing waits, and an empty answer comes at waitMs
    let t = Instant::now();
    let empty = s
        .commands(&CommandsQuery {
            after: 0,
            wait_ms: Some(300),
        })
        .await
        .unwrap();
    assert!(empty.commands.is_empty() && t.elapsed() >= Duration::from_millis(300));

    // a command pushed during the poll wakes it
    let f2 = fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        f2.push_command(CONV, None, CommandBody::Interrupt(magpie_api::Empty {}));
    });
    let t = Instant::now();
    let got = s
        .commands(&CommandsQuery {
            after: 0,
            wait_ms: Some(20_000),
        })
        .await
        .unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!((got.commands.len(), got.head), (1, 1));
    // a restarted supervisor polls from 0 and gets the unacked command again
    assert_eq!(
        s.commands(&CommandsQuery {
            after: 0,
            wait_ms: None
        })
        .await
        .unwrap()
        .commands
        .len(),
        1
    );
    s.ack_command(
        1,
        &AckRequest {
            result: AckResult {
                delivered: Some(true),
                ..AckResult::default()
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(fake.acks(CONV)[&1].delivered, Some(true));
    assert_eq!(code(s.ack_command(9, &AckRequest::default()).await).1, "not_found");

    // blobs by sha256
    let data = b"blob bytes".to_vec();
    let sha = aviary_fakes_sha(&data);
    assert!(s.sandbox_blob_put(&sha, data.clone()).await.unwrap().is_new());
    assert!(!s.sandbox_blob_put(&sha, data.clone()).await.unwrap().is_new());
    assert_eq!(
        code(s.sandbox_blob_put(&sha, b"other".to_vec()).await).1,
        "sha_mismatch"
    );
    let got = s
        .sandbox_blob_get(&sha, &magpie_api::uploads::SandboxBlobQuery::default())
        .await
        .unwrap();
    assert_eq!(&got.bytes().await.unwrap()[..], &data[..]);
}

fn aviary_fakes_sha(data: &[u8]) -> String {
    let fake = FakeMagpie::new();
    let sha = fake.seed_blob(data);
    assert_eq!(fake.blob(&sha).unwrap(), data);
    sha
}

fn put() -> PutSandboxRequest {
    PutSandboxRequest {
        person: "Staff@Example.org ".into(),
    }
}

#[tokio::test]
async fn hermit_one_sandbox_per_conversation_and_the_long_poll() {
    setup();
    let running = FakeHermit::with_config(HermitConfig {
        ready_after: None,
        ..HermitConfig::default()
    })
    .serve()
    .await
    .unwrap();
    let fake = running.fake.clone();
    let magpie = running.client(reqwest::Client::new(), Caller::Magpie);

    // idempotent PUT from two clients at once: one sandbox
    let body = put();
    let (a, b) = tokio::join!(magpie.put_sandbox(CONV, &body), magpie.put_sandbox(CONV, &body));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.sandbox().id, b.sandbox().id);
    assert_eq!((a.status(), b.status()), (202, 202));
    assert_eq!(a.sandbox().person, "staff@example.org");
    assert_eq!(fake.sandboxes().len(), 1);
    let v = a.sandbox().version;

    // the long-poll times out with the same version, and wakes on a change
    let t = Instant::now();
    let same = magpie
        .get_sandbox(
            CONV,
            SandboxQuery {
                wait_ms: Some(300),
                after_version: Some(v),
            },
        )
        .await
        .unwrap();
    assert!(t.elapsed() >= Duration::from_millis(300));
    assert_eq!((same.version, same.state), (v, SandboxState::Starting));
    let f2 = fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        f2.make_ready(CONV);
    });
    let t = Instant::now();
    let ready = magpie
        .get_sandbox(
            CONV,
            SandboxQuery {
                wait_ms: Some(20_000),
                after_version: Some(v),
            },
        )
        .await
        .unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(ready.state, SandboxState::Ready);
    assert_eq!(magpie.put_sandbox(CONV, &put()).await.unwrap().status(), 200);

    // people rules
    let not_allowed = PutSandboxRequest {
        person: "x@elsewhere.test".into(),
    };
    assert_eq!(
        magpie
            .put_sandbox(Uuid::new_v4(), &not_allowed)
            .await
            .unwrap_err()
            .code(),
        Some("not_allowed")
    );
    let other = PutSandboxRequest {
        person: "admin@example.com".into(),
    };
    assert_eq!(
        magpie.put_sandbox(CONV, &other).await.unwrap_err().code(),
        Some("person_mismatch")
    );
    let p = magpie.person(" ADMIN@example.com").await.unwrap();
    assert_eq!((p.allowed, p.role), (true, Some(hermit_api::people::Role::Admin)));
    assert!(magpie.person("someone@example.org").await.unwrap().allowed);
    assert!(!magpie.person("someone@sub.example.org").await.unwrap().allowed);
    assert_eq!(magpie.person("a@b@c").await.unwrap_err().code(), Some("bad_email"));
    assert_eq!(magpie.people().await.unwrap().domains, vec!["example.org".to_owned()]);

    // the feed: order, latest, release (idempotent) adds releasing then gone, 410 after retention
    let latest = magpie
        .sandbox_events(SandboxEventsQuery {
            after: Cursor::Latest,
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    assert_eq!(latest.events, vec![]);
    let rel = ReleaseSandboxRequest {
        reason: ConversationReleaseReason::Deleted,
    };
    assert_eq!(magpie.release_sandbox(CONV, &rel).await.unwrap().released, 1);
    assert_eq!(magpie.release_sandbox(CONV, &rel).await.unwrap().released, 0);
    let page = magpie
        .sandbox_events(SandboxEventsQuery {
            after: Cursor::Seq(0),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    let states: Vec<_> = page.events.iter().map(|e| e.state).collect();
    assert_eq!(
        states,
        vec![
            SandboxState::Starting,
            SandboxState::Ready,
            SandboxState::Releasing,
            SandboxState::Gone
        ]
    );
    assert!(page.events.windows(2).all(|w| w[0].seq < w[1].seq));
    let after = magpie
        .sandbox_events(SandboxEventsQuery {
            after: Cursor::Seq(latest.head_seq),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    assert_eq!(after.events.len(), 2);
    let gone = magpie.get_sandbox(CONV, SandboxQuery::default()).await.unwrap();
    assert_eq!(gone.state, SandboxState::Gone);
    assert_eq!(gone.release.unwrap().reason, ReleaseReason::Deleted);
    fake.expire_events_before(3);
    let expired = magpie
        .sandbox_events(SandboxEventsQuery {
            after: Cursor::Seq(0),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap_err();
    assert_eq!((expired.status(), expired.code()), (Some(410), Some("cursor_expired")));

    // a feed long-poll wakes on the next event
    let head = page.events.last().unwrap().seq;
    let f2 = fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        f2.end(CONV, ReleaseReason::HostLost, "lost");
    });
    let next = magpie.put_sandbox(CONV, &put()).await.unwrap();
    assert_eq!(next.status(), 202, "a new generation after gone");
    let woke = magpie
        .sandbox_events(SandboxEventsQuery {
            after: Cursor::Seq(head + 1),
            limit: None,
            wait_ms: Some(20_000),
        })
        .await
        .unwrap();
    assert_ne!(woke.events, vec![]);
}

#[tokio::test]
async fn hermit_tokens_descriptor_secrets_activity_and_workspaces() {
    setup();
    let running = FakeHermit::new().serve().await.unwrap();
    let fake = running.fake.clone();
    let http = reqwest::Client::new();
    let magpie = running.client(http.clone(), Caller::Magpie);
    let s = magpie.put_sandbox(CONV, &put()).await.unwrap().into_sandbox();
    let id = SandboxIdentity {
        id: s.id,
        conversation: CONV,
        person: s.person.clone(),
    };
    let gw = running.client(http.clone(), Caller::Gateway).sandbox_identity(&id);

    // a minted token verifies with the published keys; liveness follows the sandbox
    let keys = gw.token_keys().await.unwrap();
    let token = fake.mint_token(CONV, Ipv4Addr::new(10, 0, 0, 7)).unwrap();
    let claims = KeySet::from_keys(&keys)
        .verify(&token, 0, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)))
        .unwrap();
    assert_eq!((claims.sandbox_id, claims.person.as_str()), (s.id, "staff@example.org"));
    assert!(gw.token_liveness(s.id, Duration::from_secs(1)).await.unwrap().live);
    assert_eq!(
        gw.token_liveness(Uuid::new_v4(), Duration::from_secs(1))
            .await
            .unwrap_err()
            .code(),
        Some("unknown_sandbox")
    );

    // the descriptor, its etag and 304; identity mismatch is no_sandbox
    let SelfAnswer::Fresh(d) = gw.sandbox_self(None).await.unwrap() else {
        panic!("fresh")
    };
    assert_eq!((d.sandbox.id, d.etag.clone()), (s.id, d.compute_etag()));
    let ctl = fake.control_token(s.id).unwrap();
    assert!(supervisor_api::control_token_matches(&ctl, &d.control.token_sha256));
    assert!(matches!(
        gw.sandbox_self(Some(&d.etag)).await.unwrap(),
        SelfAnswer::NotModified { .. }
    ));
    let wrong = running
        .client(http.clone(), Caller::Gateway)
        .sandbox_identity(&SandboxIdentity {
            id: Uuid::new_v4(),
            ..id.clone()
        });
    assert_eq!(wrong.sandbox_self(None).await.unwrap_err().code(), Some("no_sandbox"));

    // secrets by grant
    assert_eq!(gw.sandbox_secrets().await.unwrap().keys, Vec::<String>::new());
    assert_eq!(
        gw.sandbox_secret("API_KEY").await.unwrap_err().code(),
        Some("not_granted")
    );
    fake.configure(|c| {
        c.secrets.insert("API_KEY".into(), "v".into());
    });
    assert_eq!(gw.sandbox_secret("API_KEY").await.unwrap().value, "v");

    // activity: busy flips are versioned and on the feed; a pending release rides on the reply
    let mut report: hermit_api::sandbox_surface::ActivityReport =
        serde_json::from_value(aviary_fakes::fixtures::hermit("activity-report")).unwrap();
    report.state = ActivityState::Running;
    let reply = gw.sandbox_activity(&report).await.unwrap();
    assert_eq!(
        (reply.state, reply.release.is_none()),
        (ActivityReplyState::Ready, true)
    );
    let busy = magpie.get_sandbox(CONV, SandboxQuery::default()).await.unwrap();
    assert!(busy.busy);
    assert_eq!(busy.supervisor.unwrap().boot_id, report.boot_id);
    fake.request_release(CONV, ReleaseReason::Idle, false);
    let reply = gw.sandbox_activity(&report).await.unwrap();
    assert_eq!(reply.release.unwrap().reason, ReleaseReason::Idle);

    // workspaces
    let w = Uuid::new_v4();
    assert_eq!(magpie.put_workspace(w).await.unwrap().status(), 201);
    assert_eq!(magpie.put_workspace(w).await.unwrap().status(), 200);
    fake.write_file(w, "uploads/a.txt", b"hello world");
    fake.write_file(w, "b.txt", b"b");
    let root = magpie.workspace_entries(w, &EntriesQuery::default()).await.unwrap();
    let names: Vec<_> = root.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["b.txt", "uploads"]);
    let range = hermit_api::workspaces::ByteRange { start: 6, end: None };
    let file = magpie
        .workspace_file(
            w,
            &FilesQuery {
                path: "uploads/a.txt".into(),
            },
            Some(range),
        )
        .await
        .unwrap();
    assert_eq!(&file.bytes().await.unwrap()[..], b"world");
    let copy = Uuid::new_v4();
    magpie.clone_workspace(copy, &CloneRequest { from: w }).await.unwrap();
    assert_eq!(fake.read_file(copy, "uploads/a.txt").unwrap(), b"hello world");
    magpie.delete_workspace(w).await.unwrap();
    assert_eq!(magpie.get_workspace(w).await.unwrap_err().code(), Some("no_workspace"));

    // pause, resume and snapshot answer 501 consistently
    for r in [
        magpie.pause_sandbox(CONV).await.map(|_| ()),
        magpie.resume_sandbox(CONV).await.map(|_| ()),
        magpie.snapshot_sandbox(CONV).await.map(|_| ()),
    ] {
        assert_eq!(r.unwrap_err().code(), Some("not_supported"));
    }
}

#[tokio::test]
async fn supervisor_control_token_release_refusal_and_force() {
    setup();
    let running = FakeSupervisor::new("hct_right").serve().await.unwrap();
    let wrong = ControlClient::new(reqwest::Client::new(), running.url(), "hct_wrong");
    assert_eq!(wrong.health().await.unwrap_err().code(), Some("unauthenticated"));
    let c = ControlClient::new(reqwest::Client::new(), running.url(), "hct_right");
    let turn = Uuid::new_v4();
    running.fake.set_run(ActivityState::Running, Some(turn), 0);
    let h = c.health().await.unwrap();
    assert!(h.busy && h.ready);
    let soft = ReleaseRequest::new(ReleaseReason::Idle, false, 30_000).unwrap();
    match c.release(&soft).await.unwrap() {
        Outcome::Busy(run) => assert_eq!(run.turn_id, Some(turn)),
        Outcome::Done => panic!("a busy supervisor refuses a soft release"),
    }
    let forced = ReleaseRequest::new(ReleaseReason::Deleted, true, 30_000).unwrap();
    assert_eq!(c.release(&forced).await.unwrap(), Outcome::Done);
    assert_eq!(running.fake.run().state, ActivityState::Released);
    running.fake.set_ready(false);
    assert_eq!(
        c.prepare(&supervisor_api::control::PrepareRequest::flush())
            .await
            .unwrap_err()
            .status(),
        Some(503)
    );
}

#[tokio::test]
async fn supervisor_checks_the_control_token_before_a_scripted_answer() {
    setup();
    let running = FakeSupervisor::new("hct_right").serve().await.unwrap();
    let fake = &running.fake;
    let wrong = ControlClient::new(reqwest::Client::new(), running.url(), "hct_wrong");
    let right = ControlClient::new(reqwest::Client::new(), running.url(), "hct_right");
    fake.script().once(
        "release",
        aviary_fakes::Reply::ok(serde_json::json!({"released": true})),
    );
    let soft = ReleaseRequest::new(ReleaseReason::Idle, false, 30_000).unwrap();
    let e = wrong.release(&soft).await.unwrap_err();
    assert_eq!((e.status(), e.code()), (Some(401), Some("unauthenticated")));
    assert!(
        fake.script().calls_to("release").is_empty(),
        "a refused call is not recorded"
    );
    // the scripted answer is still there for the authorised caller
    assert_eq!(right.release(&soft).await.unwrap(), Outcome::Done);
    assert_eq!(fake.script().calls_to("release").len(), 1);
    assert_eq!(
        fake.run().state,
        ActivityState::Idle,
        "the scripted answer, not the model, served it"
    );

    // every control route, exec included, scripted or not
    for route in ["health", "state", "prepare", "resumed", "exec"] {
        fake.script()
            .once(route, aviary_fakes::Reply::ok(serde_json::json!({})));
    }
    let exec = supervisor_api::ExecRequest {
        argv: vec!["true".into()],
        cwd: None,
        stdin: None,
        timeout_ms: None,
        env: std::collections::BTreeMap::new(),
    };
    let codes = [
        wrong.health().await.unwrap_err().status(),
        wrong.state().await.unwrap_err().status(),
        wrong
            .prepare(&supervisor_api::control::PrepareRequest::flush())
            .await
            .unwrap_err()
            .status(),
        wrong
            .resumed(&serde_json::from_value(aviary_fakes::fixtures::supervisor("resumed")).unwrap())
            .await
            .unwrap_err()
            .status(),
        wrong.exec(&exec).await.unwrap_err().status(),
    ];
    assert_eq!(codes, [Some(401); 5]);
    let anonymous = reqwest::Client::new()
        .post(format!("{}{}", running.url(), supervisor_api::routes::EXEC))
        .json(&exec)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
}
