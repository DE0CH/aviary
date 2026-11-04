//! The fence's crash and race cases (cutover 7.12 item 6, DESIGN 9.9) and the reverse fence (cutover 14.2):
//! - fence-race fuzz: 1 000 sends at random instants around fences that release or lift, some carrying a
//!   magpie-only file that pigeon copies into the old child under the send's ticket: each send accepted exactly
//!   once (by the old stack through its ticket, or natively), never refused, a conversation never goes back to the
//!   old stack once a native send was accepted, the native user rows dense, the attachment's bytes what whichever
//!   stack ran the turn read;
//! - a database switchover during a fence (every connection of the replica cut, then the database back): sends
//!   retried through the outage are each accepted once, natively after a release within the lease, through the old
//!   stack after a lease that lapsed meanwhile;
//! - the reverse fence: native writes wait in their call while its lease is renewed, then run natively when it is
//!   lifted, or follow the reopen to 409 `route: "legacy"`; a lapsed one holds nothing; an owned conversation that is
//!   not idle is not fenced; the per-person gate in the window state.
//!
//! "pigeon" here is the test's own client loop: the route it would take for each send (native while magpie owns the
//! conversation, else a ticket and the old stack, native again when the ticket answers so).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    dead_code
)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use magpie::legacy::fence::{self, Fence, ReverseFence};
use magpie_api::client::ClientError;
use magpie_api::conversations::RenameRequest;
use magpie_api::errors::{ErrorCode, RouteHint};
use magpie_api::legacy::{TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketRoute};
use magpie_api::turns::{FileRef, SendRequest};
use magpie_api::uploads::{UploadKind, UploadResponse};
use sha2::Digest as _;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

/// A small deterministic generator (the seed is printed, so a failing run can be repeated).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn seed() -> u64 {
    aviary_core::env::test_only("AVIARY_TEST_FENCE_RACE_SEED")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            let d = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            (d.as_secs() << 30 ^ u64::from(d.subsec_nanos())) | 1
        })
}

fn legacy_409(e: &ClientError) -> bool {
    e.api()
        .is_some_and(|r| r.code == ErrorCode::ConversationMigrating && r.route == Some(RouteHint::Legacy))
}

/// A failure a retry cures: no answer (the database or the replica cut), or a 503.
fn transient(e: &ClientError) -> bool {
    e.api().is_none() || e.code() == Some(&ErrorCode::Unavailable)
}

async fn migrating(r: &common::Replica, k: &str) -> Uuid {
    let c = common::files::conversation(r, ALICE, k).await;
    r.pool()
        .get()
        .await
        .unwrap()
        .execute("update conversations set migrating = true where id = $1", &[&c])
        .await
        .unwrap();
    c
}

/// What the old stack accepted: (conversation, clientMessageId) → the sha256 of the attachment it was handed.
type OldSide = Arc<Mutex<BTreeMap<(Uuid, String), Vec<Option<[u8; 32]>>>>>;

/// How a send went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Native,
    Legacy,
}

/// One send as pigeon routes it, retried through transient failures; never refused.
async fn deliver(r: &common::Replica, old: &OldSide, c: Uuid, cmid: &str, up: Option<&UploadResponse>) -> Route {
    let pigeon = r.pigeon();
    let me = pigeon.person(ALICE);
    for _ in 0..2_000 {
        let conv = match me.get_conversation(c).await {
            Ok(c) => c,
            Err(e) if transient(&e) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("{e:?}"),
        };
        if !conv.migrating {
            let files: Vec<FileRef> = up
                .iter()
                .map(|u| FileRef {
                    path: u.path.clone(),
                    name: u.name.clone(),
                    mime_type: u.mime_type.clone(),
                    size: u.size,
                })
                .collect();
            let req = SendRequest {
                client_message_id: cmid.to_owned(),
                text: format!("send {cmid}"),
                images: Vec::new(),
                files,
            };
            match me.send_turn(c, &req).await {
                Ok(_) => {
                    // the turn ends at once (nothing delivered it), so the next send is admitted
                    loop {
                        match me.cancel_turn(c).await {
                            Ok(_) => return Route::Native,
                            Err(e) if transient(&e) => tokio::time::sleep(Duration::from_millis(50)).await,
                            Err(e) => panic!("cancel: {e:?}"),
                        }
                    }
                }
                Err(e) if legacy_409(&e) => continue,
                Err(e) if transient(&e) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(e) => panic!("send {cmid} refused: {e:?}"),
            }
        }
        let tk = match me
            .legacy_ticket(
                c,
                &TicketRequest {
                    kind: TicketKind::Send,
                    client_message_id: Some(cmid.to_owned()),
                },
            )
            .await
        {
            Ok(t) => t,
            Err(e) if transient(&e) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("ticket {cmid}: {e:?}"),
        };
        if tk.route == TicketRoute::Native {
            continue;
        }
        // the old route: a magpie-only file is copied into the old child under the send's ticket, then the send
        let copied = match up {
            Some(u) => loop {
                match me.upload_raw(u.upload_id).await {
                    Ok(res) => break Some(sha2::Sha256::digest(res.bytes().await.unwrap()).into()),
                    Err(e) if transient(&e) => tokio::time::sleep(Duration::from_millis(50)).await,
                    Err(e) => panic!("raw: {e:?}"),
                }
            },
            None => None,
        };
        old.lock()
            .unwrap()
            .entry((c, cmid.to_owned()))
            .or_default()
            .push(copied);
        let ticket = tk.ticket.unwrap();
        loop {
            match me
                .legacy_ticket_done(
                    ticket,
                    &TicketDone {
                        outcome: TicketOutcome::Started,
                        take: tk.take,
                    },
                )
                .await
            {
                Ok(()) => return Route::Legacy,
                Err(e) if transient(&e) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(e) => panic!("done: {e:?}"),
            }
        }
    }
    panic!("send {cmid} never went anywhere");
}

/// The importer's side for one conversation: fences at random instants (retried while a ticket is open), holds each
/// fence a random while, and releases it or lifts it, until released or `stop`.
async fn importer(
    pool: aviary_pg::Pool,
    app: Arc<common::Replica>,
    c: Uuid,
    mut rng: Rng,
    stop: Arc<tokio::sync::Notify>,
) {
    let stopped = stop.notified();
    tokio::pin!(stopped);
    loop {
        tokio::select! {
            () = &mut stopped => return,
            () = tokio::time::sleep(Duration::from_millis(rng.below(60))) => {}
        }
        match fence::fence(&pool, c).await.unwrap() {
            Fence::Fenced(_) => {}
            Fence::NotMigrating => return,
            Fence::OpenTicket | Fence::AlreadyFenced => continue,
        }
        tokio::time::sleep(Duration::from_millis(rng.below(40))).await;
        if rng.below(4) == 0 {
            fence::unfence(&pool, c).await.unwrap();
        } else if fence::release(&pool, c, &fence::Release::default(), "idle", &app.app().legacy)
            .await
            .unwrap()
            .is_ok()
        {
            return;
        }
    }
}

#[tokio::test]
async fn fence_race_fuzz_of_a_thousand_sends() {
    const CONVERSATIONS: usize = 100;
    const SENDS: usize = 10;
    let s = seed();
    eprintln!("AVIARY_TEST_FENCE_RACE_SEED={s}");
    let db = common::fresh_db("fr_fuzz").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica(&db.url, &t).await);
    let sql = common::sql(&db.url);
    let old: OldSide = Arc::default();
    let mut rng = Rng(s);
    let mut tasks = Vec::new();
    for i in 0..CONVERSATIONS {
        let c = migrating(&r, &format!("fz{i}")).await;
        let mut crng = Rng(rng.next() | 1);
        let irng = Rng(rng.next() | 1);
        let (r, old, sql) = (r.clone(), old.clone(), sql.clone());
        tasks.push(tokio::spawn(async move {
            let stop = Arc::new(tokio::sync::Notify::new());
            let imp = tokio::spawn(importer(sql, r.clone(), c, irng, stop.clone()));
            let mut routes = Vec::new();
            for j in 0..SENDS {
                tokio::time::sleep(Duration::from_millis(crng.below(25))).await;
                // every third send carries a file only magpie has (uploaded after T0 through pigeon)
                let up = if crng.below(3) == 0 {
                    let bytes = format!("%PDF-1.4 {c} {j}").into_bytes();
                    Some(
                        r.pigeon()
                            .person(ALICE)
                            .upload(&format!("f{j}.pdf"), UploadKind::File, "application/pdf", bytes)
                            .await
                            .unwrap(),
                    )
                } else {
                    None
                };
                let cmid = format!("{c}:{j}");
                let route = deliver(&r, &old, c, &cmid, up.as_ref()).await;
                routes.push((cmid, route, up));
            }
            stop.notify_one();
            imp.await.unwrap();
            (c, routes)
        }));
    }
    let mut natives = 0;
    let mut legacies = 0;
    let mut released = 0;
    let conn = sql.get().await.unwrap();
    for task in tasks {
        let (c, routes) = task.await.unwrap();
        // never back to the old stack once a native send was accepted
        let first_native = routes.iter().position(|(_, r, _)| *r == Route::Native);
        if let Some(n) = first_native {
            assert!(
                routes[n..].iter().all(|(_, r, _)| *r == Route::Native),
                "conversation {c} went back to the old stack: {routes:?}"
            );
            released += 1;
        }
        let user_rows: Vec<(String, i64)> = conn
            .query(
                "select data->>'clientMessageId', seq from events where conversation_id = $1 and kind = 'user.sent'
                 order by seq",
                &[&c],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let native_cmids: Vec<&str> = routes
            .iter()
            .filter(|(_, r, _)| *r == Route::Native)
            .map(|(m, _, _)| m.as_str())
            .collect();
        // the native user rows: each native send once, in send order, nothing else
        let mine = format!("{c}:");
        let cmids: Vec<&str> = user_rows
            .iter()
            .map(|(m, _)| m.as_str())
            .filter(|m| m.starts_with(&mine))
            .collect();
        assert_eq!(cmids, native_cmids, "conversation {c}");
        let events: i64 = conn
            .query_one(
                "select count(*) - (max(seq) - min(seq) + 1) from events where conversation_id = $1",
                &[&c],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(events, 0, "the event seqs of {c} are dense");
        for (cmid, route, up) in &routes {
            let at_old = old.lock().unwrap().get(&(c, cmid.clone())).cloned().unwrap_or_default();
            let turns: i64 = conn
                .query_one(
                    "select count(*) from turns where conversation_id = $1 and client_message_id = $2",
                    &[&c, cmid],
                )
                .await
                .unwrap()
                .get(0);
            match route {
                Route::Native => {
                    natives += 1;
                    assert!(at_old.is_empty() && turns == 1, "{cmid}: {at_old:?} {turns}");
                }
                Route::Legacy => {
                    legacies += 1;
                    assert!(at_old.len() == 1 && turns == 0, "{cmid}: {at_old:?} {turns}");
                }
            }
            // the attachment: the bytes the old child got, or the fresh upload copy the native turn attached
            if let Some(u) = up {
                let original: Vec<u8> = conn
                    .query_one("select blob from uploads where id = $1", &[&u.upload_id])
                    .await
                    .unwrap()
                    .get(0);
                match route {
                    Route::Legacy => assert_eq!(at_old[0].map(|s| s.to_vec()), Some(original), "{cmid}"),
                    Route::Native => {
                        let attached: Vec<Vec<u8>> = conn
                            .query(
                                "select u.blob from uploads u join turns t on t.conversation_id = u.conversation_id
                                 where t.client_message_id = $2 and u.conversation_id = $1 and u.name = $3",
                                &[&c, cmid, &u.name],
                            )
                            .await
                            .unwrap()
                            .iter()
                            .map(|r| r.get(0))
                            .collect();
                        assert!(attached.contains(&original), "{cmid}: the native turn has the file");
                    }
                }
            }
        }
        let open: i64 = conn
            .query_one(
                "select count(*) from legacy_tickets where conversation_id = $1 and done_at is null",
                &[&c],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(open, 0);
    }
    assert_eq!(natives + legacies, CONVERSATIONS * SENDS);
    eprintln!("native {natives}, legacy {legacies}, released {released}");
    assert!(
        natives > 0 && legacies > 0 && released > 0,
        "both routes and the release were exercised"
    );
}

/// A replica whose every Postgres connection goes through a proxy the test can cut (a switchover: the primary's
/// connections end, the new primary answers after a pause).
async fn behind_a_proxy(db: &common::TestDb) -> (Arc<common::Replica>, common::CutProxy) {
    let proxy = common::CutProxy::start(common::pg_addr(&db.url)).await;
    let t = common::Tokens::new();
    let bucket = common::test_bucket(&db.url).await;
    let r = common::replica_full(
        &common::with_addr(&db.url, proxy.addr),
        &t,
        "http://127.0.0.1:1",
        common::idle(),
        bucket,
    )
    .await;
    (Arc::new(r), proxy)
}

/// Three sends to `c`, one after the other (a person sends the next once the last is accepted), from now on.
fn sends_to(r: &Arc<common::Replica>, old: &OldSide, c: Uuid) -> tokio::task::JoinHandle<Vec<Route>> {
    let (r, old) = (r.clone(), old.clone());
    tokio::spawn(async move {
        let mut out = Vec::new();
        for j in 0..3 {
            out.push(deliver(&r, &old, c, &format!("{c}:{j}"), None).await);
        }
        out
    })
}

#[tokio::test]
async fn a_switchover_during_a_fence_loses_and_doubles_nothing() {
    let db = common::fresh_db("fr_switch").await;
    let (r, proxy) = behind_a_proxy(&db).await;
    // the importer's own connection (another pod): it reaches the database once it is back
    let imp = common::sql(&db.url);
    let old: OldSide = Arc::default();

    // 1. the database is back within the lease: the release goes through, the waiting sends become native
    let c = migrating(&r, "sw1").await;
    assert!(matches!(fence::fence(&imp, c).await.unwrap(), Fence::Fenced(_)));
    let sends = sends_to(&r, &old, c);
    tokio::time::sleep(Duration::from_millis(200)).await;
    proxy.cut();
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    proxy.restore();
    fence::release(&imp, c, &fence::Release::default(), "idle", &r.app().legacy)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sends.await.unwrap(), vec![Route::Native; 3]);

    // 2. the outage outlasts the lease: the release finds it lapsed, the sends go through the old stack
    let c2 = migrating(&r, "sw2").await;
    assert!(matches!(fence::fence(&imp, c2).await.unwrap(), Fence::Fenced(_)));
    proxy.cut();
    let sends = sends_to(&r, &old, c2);
    tokio::time::sleep(magpie::limits::FENCE_LEASE + Duration::from_millis(500)).await;
    assert_eq!(
        fence::release(&imp, c2, &fence::Release::default(), "idle", &r.app().legacy)
            .await
            .unwrap(),
        Err(fence::NotReleased::NotFenced)
    );
    proxy.restore();
    assert_eq!(sends.await.unwrap(), vec![Route::Legacy; 3]);

    // each send exactly once, wherever it went
    let conn = imp.get().await.unwrap();
    for (conv, route) in [(c, Route::Native), (c2, Route::Legacy)] {
        for j in 0..3 {
            let cmid = format!("{conv}:{j}");
            let turns: i64 = conn
                .query_one(
                    "select count(*) from turns where conversation_id = $1 and client_message_id = $2",
                    &[&conv, &cmid],
                )
                .await
                .unwrap()
                .get(0);
            let at_old = old.lock().unwrap().get(&(conv, cmid.clone())).map_or(0, Vec::len);
            match route {
                Route::Native => assert_eq!((turns, at_old), (1, 0), "{cmid}"),
                Route::Legacy => assert_eq!((turns, at_old), (0, 1), "{cmid}"),
            }
        }
    }
}

fn rename(
    r: &Arc<common::Replica>,
    c: Uuid,
    title: &str,
) -> tokio::task::JoinHandle<(Result<String, ClientError>, Duration)> {
    let (r, title) = (r.clone(), title.to_owned());
    tokio::spawn(async move {
        let started = Instant::now();
        let out = r
            .pigeon()
            .person(ALICE)
            .rename_conversation(c, &RenameRequest { title })
            .await
            .map(|c| c.title.unwrap_or_default());
        (out, started.elapsed())
    })
}

#[tokio::test]
async fn a_reverse_fence_holds_native_writes_then_lets_them_go_native_or_legacy() {
    let db = common::fresh_db("fr_reverse").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica(&db.url, &t).await);
    let c = common::files::conversation(&r, ALICE, "rv").await;
    let pool = r.pool().clone();
    let pigeon = r.pigeon();
    let me = pigeon.person(ALICE);

    // not idle: an active turn, then a sandbox's lease
    me.send_turn(
        c,
        &SendRequest {
            client_message_id: "rv:1".into(),
            text: "hello".into(),
            images: Vec::new(),
            files: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        fence::reverse_fence(&pool, c).await.unwrap(),
        ReverseFence::Busy(_)
    ));
    me.cancel_turn(c).await.unwrap();
    pool.get()
        .await
        .unwrap()
        .execute(
            "update conversations set lease_id = gen_random_uuid() where id = $1",
            &[&c],
        )
        .await
        .unwrap();
    assert_eq!(
        fence::reverse_fence(&pool, c).await.unwrap(),
        ReverseFence::Busy("a sandbox holds its lease")
    );
    pool.get()
        .await
        .unwrap()
        .execute("update conversations set lease_id = null where id = $1", &[&c])
        .await
        .unwrap();

    // fenced and renewed past one lease: a write waits in its call, then runs natively once the fence is lifted
    assert!(matches!(
        fence::reverse_fence(&pool, c).await.unwrap(),
        ReverseFence::Fenced(_)
    ));
    assert_eq!(
        fence::reverse_fence(&pool, c).await.unwrap(),
        ReverseFence::AlreadyFenced
    );
    // a ticket call for an owned conversation is native at once (pigeon then sends natively and waits there)
    let tk = me
        .legacy_ticket(
            c,
            &TicketRequest {
                kind: TicketKind::Rename,
                client_message_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(tk.route, TicketRoute::Native);
    let w = rename(&r, c, "after the fence");
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(fence::renew_reverse(&pool, c).await.unwrap());
        assert!(!w.is_finished(), "the write waits while the fence is renewed");
    }
    assert!(fence::unfence_reverse(&pool, c).await.unwrap());
    let (out, waited) = w.await.unwrap();
    assert_eq!(out.unwrap(), "after the fence");
    assert!(waited >= Duration::from_secs(4), "{waited:?}");

    // fenced again and reopened: the waiting write answers 409 route legacy (pigeon takes a ticket next)
    assert!(matches!(
        fence::reverse_fence(&pool, c).await.unwrap(),
        ReverseFence::Fenced(_)
    ));
    let w = rename(&r, c, "lost to the reopen");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!w.is_finished());
    assert!(
        aviary_pg::tx!(&pool, |t| -> magpie::error::Result<bool> {
            fence::reopen_in(t, c).await
        })
        .await
        .unwrap()
    );
    let (out, _) = w.await.unwrap();
    assert!(legacy_409(&out.unwrap_err()));
    let conv = me.get_conversation(c).await.unwrap();
    assert!(conv.migrating);
    let tk = me
        .legacy_ticket(
            c,
            &TicketRequest {
                kind: TicketKind::Rename,
                client_message_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(tk.route, TicketRoute::Legacy);
    // a legacy conversation is never reverse-fenced
    assert_eq!(fence::reverse_fence(&pool, c).await.unwrap(), ReverseFence::NotOwned);

    // a reverse fence nobody renews or lifts (its holder killed) holds a write for at most its lease, then native
    let c2 = common::files::conversation(&r, ALICE, "rv2").await;
    assert!(matches!(
        fence::reverse_fence(&pool, c2).await.unwrap(),
        ReverseFence::Fenced(_)
    ));
    let (out, waited) = rename(&r, c2, "after the lapse").await.unwrap();
    assert_eq!(out.unwrap(), "after the lapse");
    assert!(
        waited >= Duration::from_millis(2_500) && waited <= Duration::from_millis(3_500),
        "{waited:?}"
    );
    assert!(
        !fence::renew_reverse(&pool, c2).await.unwrap(),
        "a lapsed fence is not renewed"
    );
    // and reopening needs a live fence
    assert!(
        !aviary_pg::tx!(&pool, |t| -> magpie::error::Result<bool> {
            fence::reopen_in(t, c2).await
        })
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn the_window_shows_a_persons_own_gate() {
    let db = common::fresh_db("fr_window").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let _c = migrating(&r, "wg").await;
    let sql = common::sql(&db.url);
    assert!(
        r.pigeon()
            .person(ALICE)
            .legacy_window()
            .await
            .unwrap()
            .handover_allowed_at
            .is_none()
    );
    sql.get()
        .await
        .unwrap()
        .execute("insert into import_gate_persons (person) values ($1)", &[&ALICE])
        .await
        .unwrap();
    let w = r.pigeon().person(ALICE).legacy_window().await.unwrap();
    assert!(w.handover_allowed_at.is_some() && !w.tees_uploads());
    let other = r.pigeon().person("bob@example.com").legacy_window().await.unwrap();
    assert!(other.handover_allowed_at.is_none() && other.tees_uploads());
}
