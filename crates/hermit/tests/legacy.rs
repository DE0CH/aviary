//! The switch's one-off commands (hermit 15.4, 22.3; cutover 8.1) on the CI Postgres and a plain directory standing
//! in for the `JuiceFS` mount: `import-jarvis` from an old-stack database built by `fixtures/jarvis-legacy.sql`
//! (every mapping, idempotence by source id, `--delta` and its lookback, share folders cloned once into their
//! `uuidv5` id), `jfs-owner` (ownership without following symlinks) and `legacy-cleanup` (refused without
//! `--confirm`; keeps every folder magpie names, live sandboxes' and folders with content).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

mod common;

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{TestDb, fresh_db};
use hermit::fabric::Fut;
use hermit::legacy::{
    self, Cleanup, EXISTS_BATCH, ID_LOOKBACK, LegacyError, Referenced, old_release_code, share_workspace,
};
use hermit::workspaces::{CliError, JfsCli, JuiceFs, Quota};
use time::OffsetDateTime;
use uuid::Uuid;

fn pool(db: &TestDb) -> aviary_pg::Pool {
    aviary_pg::connect(&db.url, "hermit-legacy-test", 4, Duration::from_secs(15)).unwrap()
}

/// hermit's database (migrated) and an old-stack one (the fixture's tables).
async fn dbs(test: &str) -> (TestDb, aviary_pg::Pool, TestDb, aviary_pg::Pool) {
    let h = fresh_db(&format!("{test}_h")).await;
    let hp = pool(&h);
    hermit::db::migrate(&hp).await.unwrap();
    let o = fresh_db(&format!("{test}_o")).await;
    let op = pool(&o);
    op.get()
        .await
        .unwrap()
        .batch_execute(include_str!("fixtures/jarvis-legacy.sql"))
        .await
        .unwrap();
    (h, hp, o, op)
}

async fn exec(p: &aviary_pg::Pool, sql: &str) {
    p.get().await.unwrap().batch_execute(sql).await.unwrap();
}

async fn rows(p: &aviary_pg::Pool, sql: &str) -> Vec<aviary_pg::Row> {
    p.get().await.unwrap().query(sql, &[]).await.unwrap()
}

async fn count(p: &aviary_pg::Pool, sql: &str) -> i64 {
    p.get().await.unwrap().query_one(sql, &[]).await.unwrap().get(0)
}

/// A scratch directory standing in for `/jfs`, removed with the guard.
struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hermit-legacy-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(p.join("ws")).unwrap();
        Self(p)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dst = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &dst)?;
        } else {
            std::fs::copy(e.path(), dst)?;
        }
    }
    Ok(())
}

/// The `juicefs` CLI on a plain directory: copy for `clone`, remove for `rmr`, quotas in memory.
#[derive(Default)]
struct Cli {
    root: PathBuf,
    quotas: Mutex<HashMap<String, (u32, u64)>>,
    calls: Mutex<Vec<String>>,
}

impl JfsCli for Cli {
    fn quota_get<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>> {
        Box::pin(async move {
            Ok(self.quotas.lock().unwrap().get(path).map(|&(g, i)| Quota {
                capacity_bytes: u64::from(g) << 30,
                inodes: i,
                used_bytes: 0,
                used_inodes: 0,
            }))
        })
    }
    fn quota_set<'a>(&'a self, path: &'a str, gib: u32, inodes: u64) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("quota set {path}"));
            self.quotas.lock().unwrap().insert(path.to_owned(), (gib, inodes));
            Ok(())
        })
    }
    fn quota_delete<'a>(&'a self, path: &'a str) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("quota delete {path}"));
            self.quotas.lock().unwrap().remove(path);
            Ok(())
        })
    }
    fn clone_tree<'a>(&'a self, from: &'a Path, to: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            let rel = from.strip_prefix(&self.root).unwrap_or(from).display().to_string();
            self.calls.lock().unwrap().push(format!("clone {rel}"));
            copy_tree(from, to).map_err(|e| CliError::Failed(e.to_string()))
        })
    }
    fn rmr<'a>(&'a self, path: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            let rel = path.strip_prefix(&self.root).unwrap_or(path).display().to_string();
            self.calls.lock().unwrap().push(format!("rmr {rel}"));
            std::fs::remove_dir_all(path).map_err(|e| CliError::Failed(e.to_string()))
        })
    }
}

fn store(root: &Path) -> (Arc<Cli>, JuiceFs) {
    let cli = Arc::new(Cli {
        root: root.to_owned(),
        ..Cli::default()
    });
    let fs = JuiceFs::new(root, cli.clone(), 20, 2_000_000);
    (cli, fs)
}

#[test]
fn release_codes_of_the_old_strings() {
    let cases = [
        ("idle", Some("sandbox_released")),
        ("pool", Some("sandbox_released")),
        ("deleted", Some("sandbox_released")),
        ("egress-check", Some("sandbox_released")),
        ("live check", Some("sandbox_released")),
        ("test done", Some("sandbox_released")),
        ("unhealthy", Some("sandbox_lost")),
        ("host lost", Some("sandbox_lost")),
        ("sandbox vanished", Some("sandbox_lost")),
        ("lost: instance no longer exists", Some("sandbox_lost")),
        ("stuck", None),
        (
            "start failed: no sandbox host became available in time",
            Some("sandbox_busy"),
        ),
        (
            "start failed: all sandbox hosts are full, try again in a few minutes",
            Some("sandbox_busy"),
        ),
        (
            "start failed: k8s create pod: 403 … exceeded quota: sandboxes, …",
            Some("sandbox_busy"),
        ),
        (
            "start failed: the sandbox did not become ready in time",
            Some("sandbox_failed"),
        ),
        (
            "start failed: sandboxd POST /v1/sandboxes: 503 the workspace store (JuiceFS) is not mounted",
            Some("sandbox_failed"),
        ),
    ];
    for (reason, code) in cases {
        assert_eq!(old_release_code(Some(reason)), code, "{reason}");
    }
    assert_eq!(old_release_code(None), None);
}

#[test]
fn share_ids_follow_magpies_convention() {
    let ns = Uuid::from_u128(0x6f1c_9d2e_0b7a_4f53_9a43_5b2f_3f1e_8c61);
    assert_eq!(
        share_workspace("QlS2kl8oUYDeqoJK9CMpAA"),
        Uuid::new_v5(&ns, b"share:QlS2kl8oUYDeqoJK9CMpAA")
    );
}

const OLD: &str = "
insert into users (email) values ('Alice@Example.com');
insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, created_at, last_active_at,
    expires_at, released_at, release_reason, host, ip, owner) values
  ('00000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001', 'sbx-0000000000004000', 'h1', 'x',
   'gone', '2026-10-30 01:02:03.456789+00', '2026-10-30 01:10:00+00', '2026-10-30 09:02:03.456789+00',
   '2026-10-30 01:40:00.000001+00', 'idle', 'aws-6582f2', '10.200.0.10', 'Alice@Example.com'),
  ('00000000-0000-4000-8000-000000000002', '10000000-0000-4000-8000-000000000002', 'sbx-b', 'h2', 'x', 'gone',
   now() - interval '2 days', now() - interval '2 days', now(), now() - interval '2 days',
   'start failed: all sandbox hosts are full, try again in a few minutes', null, 'not an ip', 'bob@example.com'),
  -- the old UI's sandbox (no owner): dropped
  ('00000000-0000-4000-8000-000000000003', '10000000-0000-4000-8000-000000000003', 'sbx-c', 'h3', 'x', 'gone',
   now(), now(), now(), now(), 'idle', null, null, null),
  -- live: web's until the end
  ('00000000-0000-4000-8000-000000000004', '10000000-0000-4000-8000-000000000004', 'sbx-d', 'h4', 'x', 'ready',
   now(), now(), now(), null, null, 'aws-1', '10.200.0.11', 'bob@example.com'),
  -- released 401 days ago: outside hermit's retention
  ('00000000-0000-4000-8000-000000000005', '10000000-0000-4000-8000-000000000005', 'sbx-e', 'h5', 'x', 'gone',
   now() - interval '402 days', now() - interval '402 days', now(), now() - interval '401 days', 'idle', null, null,
   'bob@example.com');
insert into ops_events (at, kind, ok, ms, sandbox_id, conversation_id, host, detail) values
  ('2026-11-01 06:37:09.159458+00', 'sandbox_start', true, 63245, '00000000-0000-4000-8000-000000000001',
   '10000000-0000-4000-8000-000000000001', 'aws-6582f2', '{\"startMs\": 63240, \"provider\": \"vm\"}'),
  (now() - interval '1 day', 'sandbox_start', false, 81918, '00000000-0000-4000-8000-000000000009', null, null,
   '{\"error\": \"sandboxd: token=not-a-real-value refused\", \"reason\": \"start failed\"}'),
  (now() - interval '31 days', 'sandbox_start', true, 1, null, null, null, '{}'),
  (now(), 'view_as', true, 1, null, null, null, '{}'),
  (now(), 'budget', true, 1, null, null, null, '{}');
insert into vm_events (at, kind, host, sandbox_id, data) values
  ('2026-11-03 08:03:38.252642+00', 'terminate', 'aws-0d12c6', null, '{\"upMs\": 902587, \"reason\": \"empty\"}'),
  (now(), 'launch-failed', null, '00000000-0000-4000-8000-000000000001', '{}');
insert into vm_hosts (name, kind, url, wg_ip, instance_id, private_ip, status, mem_total, cpus, created_at, gone_at,
    gone_reason) values
  ('aws-6582f2', 'aws', 'https://10.201.0.10:7443', '10.201.0.10', 'i-0c9e752819c138ef4', '172.31.25.64', 'gone',
   16464269312, 4, '2026-11-03 06:36:06.117207+00', '2026-11-03 07:28:38.831752+00', 'empty'),
  ('old-hub', 'hub', 'https://10.201.0.2:7443', '10.201.0.2', null, null, 'gone', null, null, now(), now(), 'retired'),
  ('yyc-sbx-gw', 'hub', 'https://10.201.0.1:7443', '10.201.0.1', null, null, 'active', null, 4, now(), null, null);
";

#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, every mapped column
async fn import_maps_every_table_and_is_idempotent() {
    let (_h, hp, _o, op) = dbs("import").await;
    exec(&op, OLD).await;
    let root = Root::new();
    let r = legacy::import_jarvis(&hp, &op, None, &root.0, false).await.unwrap();
    assert_eq!((r.sandboxes.read, r.sandboxes.copied), (2, 2));
    assert_eq!((r.sandbox_starts.read, r.sandbox_starts.copied), (2, 2));
    assert_eq!((r.host_events.read, r.host_events.copied), (2, 2));
    assert_eq!((r.hosts.read, r.hosts.copied), (2, 2));
    assert!(r.shares.is_none());

    let s = rows(
        &hp,
        "select id::text, conversation_id::text, person, status, name, token_kid, host, host(ip), cpu, memory_bytes,
                created_at::text, last_active_at::text, expires_at::text, hard_expires_at::text, released_at::text,
                release_reason, release_code, imported
           from sandboxes order by id",
    )
    .await;
    assert_eq!(s.len(), 2);
    let a = &s[0];
    assert_eq!(a.get::<_, String>(0), "00000000-0000-4000-8000-000000000001");
    assert_eq!(a.get::<_, String>(1), "10000000-0000-4000-8000-000000000001");
    assert_eq!(a.get::<_, String>(2), "alice@example.com");
    assert_eq!(a.get::<_, String>(3), "gone");
    assert_eq!(a.get::<_, String>(4), "sbx-0000000000004000");
    assert_eq!(a.get::<_, Option<i16>>(5), None);
    assert_eq!(a.get::<_, Option<String>>(6).as_deref(), Some("aws-6582f2"));
    assert_eq!(a.get::<_, Option<String>>(7).as_deref(), Some("10.200.0.10"));
    assert!((a.get::<_, f32>(8) - 1.0).abs() < f32::EPSILON);
    assert_eq!(a.get::<_, i64>(9), 1 << 30);
    assert_eq!(a.get::<_, String>(10), "2026-10-30 01:02:03.456789+00");
    assert_eq!(a.get::<_, String>(11), "2026-10-30 01:10:00+00");
    assert_eq!(a.get::<_, String>(12), "2026-10-30 09:02:03.456789+00");
    assert_eq!(a.get::<_, String>(13), "2026-10-30 09:02:03.456789+00");
    assert_eq!(
        a.get::<_, Option<String>>(14).as_deref(),
        Some("2026-10-30 01:40:00.000001+00")
    );
    assert_eq!(a.get::<_, Option<String>>(15).as_deref(), Some("idle"));
    assert_eq!(a.get::<_, Option<String>>(16).as_deref(), Some("sandbox_released"));
    assert!(a.get::<_, bool>(17));
    let b = &s[1];
    assert_eq!(b.get::<_, Option<String>>(7), None, "an unreadable ip is null");
    assert_eq!(b.get::<_, Option<String>>(16).as_deref(), Some("sandbox_busy"));

    let st = rows(
        &hp,
        "select at::text, ok, ms, start_ms, sandbox_id::text, person, host, reason, error, source_id
           from sandbox_starts order by source_id",
    )
    .await;
    assert_eq!(st.len(), 2);
    assert_eq!(st[0].get::<_, String>(0), "2026-11-01 06:37:09.159458+00");
    assert!(st[0].get::<_, bool>(1));
    assert_eq!(st[0].get::<_, i32>(2), 63245);
    assert_eq!(st[0].get::<_, Option<i32>>(3), Some(63240));
    assert_eq!(st[0].get::<_, Option<String>>(5).as_deref(), Some("alice@example.com"));
    assert_eq!(st[0].get::<_, Option<String>>(6).as_deref(), Some("aws-6582f2"));
    assert_eq!(st[0].get::<_, i64>(9), 1);
    assert!(!st[1].get::<_, bool>(1));
    assert_eq!(
        st[1].get::<_, Option<String>>(5),
        None,
        "a sandbox the old database does not know has no person"
    );
    assert_eq!(st[1].get::<_, Option<String>>(7).as_deref(), Some("start failed"));
    let err = st[1].get::<_, Option<String>>(8).unwrap();
    assert!(
        !err.contains("abcdefabcdefabcdefabcdef1234"),
        "the error is redacted: {err}"
    );
    assert!(err.starts_with("sandboxd: "), "{err}");

    let he = rows(
        &hp,
        "select at::text, kind, host, sandbox_id::text, data::text, source_id from host_events order by source_id",
    )
    .await;
    assert_eq!(he.len(), 2);
    assert_eq!(he[0].get::<_, String>(0), "2026-11-03 08:03:38.252642+00");
    assert_eq!(he[0].get::<_, String>(1), "terminate");
    assert_eq!(he[0].get::<_, String>(4), r#"{"upMs": 902587, "reason": "empty"}"#);
    assert_eq!(
        he[1].get::<_, Option<String>>(3).as_deref(),
        Some("00000000-0000-4000-8000-000000000001")
    );

    let ho = rows(
        &hp,
        "select name, kind, url, host(wg_ip), instance_id, host(private_ip), status, mem_total, cpus, created_at::text,
                gone_at::text, gone_reason from hosts order by name",
    )
    .await;
    assert_eq!(ho.len(), 2, "only gone hosts");
    assert_eq!(ho[0].get::<_, String>(0), "aws-6582f2");
    assert_eq!(ho[0].get::<_, String>(1), "aws");
    assert_eq!(ho[0].get::<_, String>(3), "10.201.0.10");
    assert_eq!(ho[0].get::<_, Option<String>>(5).as_deref(), Some("172.31.25.64"));
    assert_eq!(ho[0].get::<_, String>(6), "gone");
    assert_eq!(ho[0].get::<_, String>(9), "2026-11-03 06:36:06.117207+00");
    assert_eq!(
        ho[0].get::<_, Option<String>>(10).as_deref(),
        Some("2026-11-03 07:28:38.831752+00")
    );
    assert_eq!(ho[1].get::<_, String>(0), "old-hub");
    assert_eq!(ho[1].get::<_, String>(1), "gateway", "kind hub becomes gateway");

    // a second full run and a delta run copy nothing new
    for delta in [false, true] {
        let again = legacy::import_jarvis(&hp, &op, None, &root.0, delta).await.unwrap();
        assert_eq!(
            [
                again.sandboxes.copied,
                again.sandbox_starts.copied,
                again.host_events.copied,
                again.hosts.copied
            ],
            [0, 0, 0, 0],
            "delta {delta}"
        );
    }
    assert_eq!(count(&hp, "select count(*) from sandboxes").await, 2);
    assert_eq!(count(&hp, "select count(*) from sandbox_starts").await, 2);
}

#[tokio::test]
async fn delta_reads_only_what_is_new_and_the_lookback() {
    let (_h, hp, _o, op) = dbs("delta").await;
    exec(&op, OLD).await;
    let root = Root::new();
    legacy::import_jarvis(&hp, &op, None, &root.0, false).await.unwrap();
    // new rows of every kind
    exec(
        &op,
        "insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, released_at,
             release_reason, owner) values
           ('00000000-0000-4000-8000-0000000000aa', '10000000-0000-4000-8000-0000000000aa', 'sbx-aa', 'haa', 'x',
            'gone', now(), now(), 'unhealthy', 'bob@example.com');
         update sandboxes set status = 'gone', released_at = now(), release_reason = 'host lost'
          where id = '00000000-0000-4000-8000-000000000004';
         insert into vm_events (kind) values ('active');
         insert into ops_events (kind, ok, ms) values ('sandbox_start', true, 5);",
    )
    .await;
    let d = legacy::import_jarvis(&hp, &op, None, &root.0, true).await.unwrap();
    assert_eq!(d.sandboxes.copied, 2, "the new sandbox and the one that became gone");
    assert_eq!(
        d.sandboxes.read, 3,
        "the two new ones and the latest one imported before (the lookback), not the older ones: {d:?}"
    );
    assert_eq!(d.host_events.copied, 1);
    assert_eq!(d.sandbox_starts.copied, 1);
    let codes = rows(&hp, "select release_code from sandboxes where id in ('00000000-0000-4000-8000-0000000000aa', '00000000-0000-4000-8000-000000000004')").await;
    assert!(
        codes
            .iter()
            .all(|r| r.get::<_, Option<String>>(0).as_deref() == Some("sandbox_lost"))
    );

    // ids handed out before a neighbour's but committed after it: found by --delta within ID_LOOKBACK
    let top: i64 = count(&hp, "select max(source_id) from host_events").await;
    exec(&op, &format!("select setval('vm_events_id_seq', {})", top + 10)).await;
    exec(&op, "insert into vm_events (kind) values ('pack')").await;
    legacy::import_jarvis(&hp, &op, None, &root.0, true).await.unwrap();
    exec(
        &op,
        &format!("insert into vm_events (id, kind) values ({}, 'drain')", top + 3),
    )
    .await;
    let late = legacy::import_jarvis(&hp, &op, None, &root.0, true).await.unwrap();
    assert_eq!(late.host_events.copied, 1);
    assert!(late.host_events.read < u64::try_from(ID_LOOKBACK).unwrap() + 20);
    assert_eq!(
        count(
            &hp,
            &format!("select count(*) from host_events where source_id = {}", top + 3)
        )
        .await,
        1
    );
    assert_eq!(
        count(&op, "select count(*) from vm_events").await,
        count(&hp, "select count(*) from host_events").await
    );
}

/// An old share folder with a file and a subfolder.
fn old_share(root: &Path, key: &str, id: &str, text: &str) {
    let ws = root.join("users").join(key).join("shares").join(id).join("ws");
    std::fs::create_dir_all(ws.join("out")).unwrap();
    std::fs::write(ws.join("out/report.md"), text).unwrap();
    std::fs::write(
        root.join("users")
            .join(key)
            .join("shares")
            .join(id)
            .join("messages.json"),
        "[]",
    )
    .unwrap();
}

#[tokio::test]
async fn shares_are_cloned_once_into_their_uuidv5_and_never_replaced() {
    let (_h, hp, _o, op) = dbs("shares").await;
    let root = Root::new();
    let (cli, fs) = store(&root.0);
    old_share(&root.0, "uc1d7ac9eb7211ea16c6b8e79", "QlS2kl8oUYDeqoJK9CMpAA", "first");
    old_share(&root.0, "uc1d7ac9eb7211ea16c6b8e79", "RVe_Lsura7cKpH6VoVETnQ", "second");
    old_share(&root.0, "ucddc0353bc1de2879a505ae2", "dWk39oSyoF6V7ac-fB8Hyg", "third");
    // made already (magpie or an earlier pass): kept as it is
    let made = share_workspace("dWk39oSyoF6V7ac-fB8Hyg");
    std::fs::create_dir_all(root.0.join("ws").join(made.to_string())).unwrap();
    std::fs::write(root.0.join("ws").join(made.to_string()).join("mine"), "keep").unwrap();
    // not share folders: a name that is not plain, a share without ws/
    std::fs::create_dir_all(root.0.join("users/u1/shares/bad name/ws")).unwrap();
    std::fs::create_dir_all(root.0.join("users/u1/shares/nows")).unwrap();
    std::fs::create_dir_all(root.0.join("users/u1/claude")).unwrap();

    let r = legacy::import_jarvis(&hp, &op, Some(&fs), &root.0, false)
        .await
        .unwrap();
    let s = r.shares.unwrap();
    assert_eq!((s.found, s.cloned, s.skipped), (3, 2, 1), "{s:?}");
    assert_eq!(s.failed, Vec::<String>::new());
    let first = share_workspace("QlS2kl8oUYDeqoJK9CMpAA");
    assert_eq!(
        std::fs::read_to_string(root.0.join("ws").join(first.to_string()).join("out/report.md")).unwrap(),
        "first"
    );
    assert!(
        !root.0.join("ws").join(first.to_string()).join("messages.json").exists(),
        "only ws/ is cloned"
    );
    assert_eq!(
        std::fs::read_to_string(root.0.join("ws").join(made.to_string()).join("mine")).unwrap(),
        "keep"
    );
    assert!(
        cli.quotas.lock().unwrap().contains_key(&format!("/ws/{first}")),
        "the clone has its quota"
    );
    assert_eq!(
        count(&hp, &format!("select count(*) from workspaces where id = '{first}'")).await,
        1
    );
    // no temporary clone tree left behind
    let leftovers: Vec<_> = std::fs::read_dir(root.0.join("ws"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let clones = || {
        cli.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("clone "))
            .count()
    };
    assert_eq!(clones(), 2);
    let again = legacy::import_jarvis(&hp, &op, Some(&fs), &root.0, true)
        .await
        .unwrap()
        .shares
        .unwrap();
    assert_eq!((again.found, again.cloned, again.skipped), (3, 0, 3));
    assert_eq!(clones(), 2, "a second pass clones nothing");
    // a new share folder is picked up by the next delta
    old_share(&root.0, "ucddc0353bc1de2879a505ae2", "yZQTXLXpVnAOnBnHLzDeWw", "fourth");
    let next = legacy::import_jarvis(&hp, &op, Some(&fs), &root.0, true)
        .await
        .unwrap()
        .shares
        .unwrap();
    assert_eq!((next.found, next.cloned), (4, 1));
}

#[test]
fn jfs_owner_owns_every_entry_without_following_symlinks() {
    let root = Root::new();
    let ws = root.0.join("ws");
    let outside = root.0.join("outside");
    std::fs::write(&outside, "x").unwrap();
    for w in ["a", "b"] {
        std::fs::create_dir_all(ws.join(w).join("deep/er")).unwrap();
        std::fs::write(ws.join(w).join("deep/er/f"), "y").unwrap();
    }
    std::os::unix::fs::symlink(&outside, ws.join("a/link")).unwrap();
    let me = std::fs::metadata(&outside).unwrap();
    if me.uid() == 0 {
        // CI's root step and the runner: real ownership changes
        let r = legacy::fix_owner(&root.0, 1000, 1000, 4).unwrap();
        assert_eq!(r.error_count, 0, "{r:?}");
        assert_eq!(
            r.scanned, 10,
            "ws, 2 x (dir, deep, er, f), the link and nothing outside: {r:?}"
        );
        assert_eq!(r.changed, r.scanned);
        for p in ["", "a", "a/deep/er/f", "b/deep"] {
            let m = std::fs::symlink_metadata(ws.join(p)).unwrap();
            assert_eq!((m.uid(), m.gid()), (1000, 1000), "{p}");
        }
        let l = std::fs::symlink_metadata(ws.join("a/link")).unwrap();
        assert_eq!(l.uid(), 1000, "the link itself is owned");
        assert_eq!(std::fs::metadata(&outside).unwrap().uid(), 0, "its target is not");
        let again = legacy::fix_owner(&root.0, 1000, 1000, 4).unwrap();
        assert_eq!(again.changed, 0, "a second run changes nothing");
    } else {
        // as a user: entries already owned are left alone, the rest are counted as errors (EPERM)
        let r = legacy::fix_owner(&root.0, me.uid(), me.gid(), 4).unwrap();
        assert_eq!((r.scanned, r.changed, r.error_count), (10, 0, 0), "{r:?}");
        let r = legacy::fix_owner(&root.0, me.uid() + 1, me.gid(), 4).unwrap();
        assert_eq!(r.changed, 0);
        assert_eq!(r.error_count, 10);
        assert_eq!(r.errors.len(), 10);
    }
}

/// magpie's `workspaces/exists` as a set, with the batch sizes it was asked.
struct Magpie {
    known: Vec<Uuid>,
    batches: Mutex<Vec<usize>>,
    down: bool,
}

impl Referenced for Magpie {
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>> {
        Box::pin(async move {
            if self.down {
                return Err("magpie workspaces/exists: unreachable".into());
            }
            self.batches.lock().unwrap().push(ids.len());
            Ok(ids.iter().filter(|i| self.known.contains(i)).copied().collect())
        })
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one volume, every kind of folder
async fn cleanup_refuses_without_confirm_and_keeps_what_is_referenced() {
    let (_h, hp, _o, _op) = dbs("cleanup").await;
    let root = Root::new();
    let (cli, _fs) = store(&root.0);
    let ws = root.0.join("ws");
    let id = |n: u128| Uuid::from_u128(0xa000_0000_0000_4000_8000_0000_0000_0000 + n);
    let (referenced, live, unreferenced, with_content) = (id(1), id(2), id(3), id(4));
    for w in [referenced, live, unreferenced, with_content] {
        std::fs::create_dir(ws.join(w.to_string())).unwrap();
    }
    std::fs::write(ws.join(with_content.to_string()).join("f"), "x").unwrap();
    // more empty unreferenced folders than one exists call takes
    let many: Vec<Uuid> = (100..100 + EXISTS_BATCH as u128 + 3).map(id).collect();
    for w in &many {
        std::fs::create_dir(ws.join(w.to_string())).unwrap();
    }
    std::fs::create_dir(ws.join(format!(".clone-{unreferenced}-1-abc"))).unwrap();
    std::fs::create_dir(ws.join("not-a-uuid")).unwrap();
    std::fs::create_dir_all(root.0.join("users/u1/claude")).unwrap();
    std::fs::create_dir_all(root.0.join("sent/x")).unwrap();
    let now = OffsetDateTime::now_utc();
    hp.get()
        .await
        .unwrap()
        .execute(
            "insert into sandboxes (id, conversation_id, person, status, name, token_kid, cpu, memory_bytes,
                                    start_deadline, expires_at, hard_expires_at)
             values ($1, $2, 'a@example.com', 'ready', 'sbx-live', 1, 1, 1073741824, $3, $3, $3)",
            &[&Uuid::new_v4(), &live, &now],
        )
        .await
        .unwrap();
    hp.get()
        .await
        .unwrap()
        .execute(
            "insert into workspaces (id, created_at) values ($1, now())",
            &[&unreferenced],
        )
        .await
        .unwrap();
    let magpie = Magpie {
        known: vec![referenced],
        batches: Mutex::new(Vec::new()),
        down: false,
    };

    let refused = legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), false)
        .await
        .unwrap();
    let Cleanup::Refused { plan } = refused else {
        panic!("not refused: {refused:?}")
    };
    assert_eq!(plan.trees, ["users", "sent"]);
    assert_eq!(plan.empty_workspaces.len(), many.len() + 1);
    assert!(plan.empty_workspaces.contains(&unreferenced));
    assert!(!plan.empty_workspaces.contains(&referenced) && !plan.empty_workspaces.contains(&live));
    assert_eq!(
        (plan.kept_referenced, plan.kept_live, plan.kept_with_content),
        (1, 1, 1)
    );
    assert_eq!(
        *magpie.batches.lock().unwrap(),
        [EXISTS_BATCH, 5],
        "every empty folder without a live sandbox"
    );
    assert!(
        cli.calls.lock().unwrap().is_empty(),
        "nothing removed without --confirm"
    );
    assert!(root.0.join("users").exists() && ws.join(unreferenced.to_string()).exists());

    // a folder with content by the time of the run is kept
    let Cleanup::Done { plan, done } = ({
        std::fs::write(ws.join(many[0].to_string()).join("late"), "x").unwrap();
        legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), true)
            .await
            .unwrap()
    }) else {
        panic!("not done")
    };
    assert_eq!(done.trees_removed, ["users", "sent"]);
    assert_eq!(
        done.workspaces_removed,
        u64::try_from(plan.empty_workspaces.len()).unwrap()
    );
    assert!(!root.0.join("users").exists() && !root.0.join("sent").exists());
    assert!(!ws.join(unreferenced.to_string()).exists());
    for keep in [referenced, live, with_content] {
        assert!(ws.join(keep.to_string()).exists(), "{keep}");
    }
    assert!(ws.join(format!(".clone-{unreferenced}-1-abc")).exists() && ws.join("not-a-uuid").exists());
    assert!(ws.join(many[0].to_string()).join("late").exists());
    assert_eq!(
        count(
            &hp,
            &format!("select count(*) from workspaces where id = '{unreferenced}'")
        )
        .await,
        0
    );
    assert!(
        cli.calls
            .lock()
            .unwrap()
            .contains(&format!("quota delete /ws/{unreferenced}"))
    );

    // magpie away: no plan, nothing removed
    std::fs::create_dir(ws.join(unreferenced.to_string())).unwrap();
    let down = Magpie {
        known: vec![],
        batches: Mutex::new(Vec::new()),
        down: true,
    };
    let e = legacy::legacy_cleanup(&root.0, &hp, &down, cli.as_ref(), true)
        .await
        .unwrap_err();
    assert!(
        matches!(e, LegacyError::Other(ref m) if m.contains("unreachable")),
        "{e}"
    );
    assert!(ws.join(unreferenced.to_string()).exists());
}
