//! hermit 24.2's fabric tests, ported from jarvis3 `vm.test.ts` (the scaler, placement, burst to AWS, packing by
//! live migration, host loss, `JuiceFS` gating, capacity planning) against fake sandboxd hosts (the `sandboxd-api`
//! router over an in-memory host) and a fake EC2, plus what the port adds: vanished sandboxes, the EC2 reconcile, a
//! migration resumed after a replica's death, and a start through the whole core on this fabric.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::unused_async_trait_impl,
    clippy::many_single_char_names
)]

mod common;

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::hermit::Timings;
use ::hermit::db;
use ::hermit::fabric::ec2::{Ec2, Ec2Error, Instance};
use ::hermit::fabric::gvisor::{ControlUrl, Deps, Gvisor, HostSecrets};
use ::hermit::fabric::network::Fixed;
use ::hermit::fabric::sandboxd::Connect;
use ::hermit::fabric::scaler::{self, Report};
use ::hermit::fabric::{Fabric, Fut, Launch, Place};
use aviary_core::auth::Caller;
use aviary_fakes::FakeSupervisor;
use aviary_fakes::supervisor::Running;
use common::{Harness, Replica, STAFF};
use hermit_api::routes;
use hermit_api::sandbox::SandboxState;
use sandboxd_api::{
    Answer, ApiError, Client, Deleted, EgressTargets, EnsureImage, Host, ImageDigest, Listed, Peer, Peers,
    ReceiveResult, Receiving, SandboxState as HostState, Sandboxes, ScratchStats, SendRequest, SendResult, Server,
    SetPeers, Spec,
};
use serde_json::json;
use tokio::sync::Notify;
use uuid::Uuid;

const GIB: u64 = 1 << 30;
const GATEWAY: &str = "hermit-gw";
const WAIT: Duration = Duration::from_secs(20);

// ---- the fake fleet ------------------------------------------------------------------------------------------

/// One sandbox on a fake host.
#[derive(Clone)]
struct OnHost {
    spec: Spec,
    state: HostState,
    alive: Option<bool>,
    /// Listed `sending` (a send of it runs here).
    sending: bool,
    scratch_used: Option<u64>,
    created_at: String,
}

/// One fake sandboxd.
struct HostSim {
    name: String,
    fleet: std::sync::Weak<Fleet>,
    info: Mutex<Host>,
    sandboxes: Mutex<BTreeMap<String, OnHost>>,
    peers: Mutex<Option<Vec<Peer>>>,
    egress: Mutex<Option<Vec<String>>>,
    images: Mutex<Vec<String>>,
    calls: Mutex<Vec<String>>,
    /// The next send answers 409 (would not converge).
    refuse_send: AtomicBool,
    /// Sends wait for `release_send` while set.
    hold_send: AtomicBool,
    send_gate: Notify,
    sends_waiting: AtomicU32,
    /// The next send stops the sandbox here (listed running, `alive: false`, as sandboxd lists a source its
    /// checkpoint stopped) and its answer is lost (500), with the target's receive still in progress; the test
    /// finishes the move itself (`Rig::finish_move`).
    stop_and_lose_answer: AtomicBool,
    /// The next send's answer is lost (500) while the send goes on here: the source lists the sandbox running,
    /// alive and `sending` until the test ends the send (`Rig::finish_move`, or `sending` cleared: it stayed).
    lose_answer_keep_sending: AtomicBool,
    /// This many deletes answer 500 and remove nothing (a host that does not answer one).
    fail_deletes: AtomicU32,
}

impl HostSim {
    fn saw(&self, c: String) {
        self.calls.lock().unwrap().push(c);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn listed(&self) -> Vec<Listed> {
        self.sandboxes
            .lock()
            .unwrap()
            .values()
            .map(|s| Listed {
                name: s.spec.name.clone(),
                ip: s.spec.ip.clone(),
                state: s.state,
                created_at: s.created_at.clone(),
                cpu: s.spec.cpu,
                memory: s.spec.memory,
                cpu_usage: 0.0,
                memory_current: 0,
                alive: s.alive,
                sending: s.sending,
                scratch_used: s.scratch_used,
            })
            .collect()
    }

    fn running(&self, name: &str) -> bool {
        self.sandboxes
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|s| s.state == HostState::Running)
    }

    fn set(&self, f: impl FnOnce(&mut Host)) {
        f(&mut self.info.lock().unwrap());
    }

    fn release_send(&self) {
        self.hold_send.store(false, Ordering::SeqCst);
        self.send_gate.notify_waiters();
    }
}

fn iso_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

impl Server for HostSim {
    async fn host(&self) -> Answer<Host> {
        let mut h = self.info.lock().unwrap().clone();
        h.sandboxes = self.listed();
        h.images.clone_from(&self.images.lock().unwrap());
        h.egress_targets = self.egress.lock().unwrap().clone().unwrap_or_default();
        Ok(h)
    }
    async fn set_peers(&self, req: SetPeers) -> Answer<Peers> {
        self.saw(format!("peers {}", req.peers.len()));
        *self.peers.lock().unwrap() = Some(req.peers);
        Ok(Peers { peers: vec![] })
    }
    async fn set_egress(&self, req: EgressTargets) -> Answer<EgressTargets> {
        self.saw("egress".into());
        *self.egress.lock().unwrap() = Some(req.targets.clone());
        Ok(req)
    }
    async fn ensure_image(&self, req: EnsureImage) -> Answer<ImageDigest> {
        let d = req.digest.unwrap_or_default();
        match req.reference {
            Some(r) => self.saw(format!("image {d} ref {r}")),
            None => self.saw(format!("image {d}")),
        }
        self.images.lock().unwrap().push(d.clone());
        Ok(ImageDigest { digest: d })
    }
    async fn list(&self) -> Answer<Sandboxes> {
        Ok(Sandboxes {
            sandboxes: self.listed(),
        })
    }
    async fn create(&self, spec: Spec) -> Answer<Listed> {
        self.saw(format!("create {}", spec.name));
        if self.info.lock().unwrap().jfs_mounted == Some(false) {
            return Err(ApiError::new(503, "the shared filesystem is not mounted"));
        }
        {
            let s = self.sandboxes.lock().unwrap();
            if let Some(o) = s.values().find(|o| o.spec.ip == spec.ip && o.spec.name != spec.name) {
                return Err(ApiError::new(409, format!("ip {} is used by {}", spec.ip, o.spec.name)));
            }
        }
        let fleet = self.fleet.upgrade().unwrap();
        fleet.supervisor_for(&spec).await;
        self.sandboxes.lock().unwrap().insert(
            spec.name.clone(),
            OnHost {
                spec: spec.clone(),
                state: HostState::Running,
                alive: Some(true),
                sending: false,
                scratch_used: Some(0),
                created_at: iso_now(),
            },
        );
        Ok(self.listed().into_iter().find(|l| l.name == spec.name).unwrap())
    }
    async fn delete(&self, name: String) -> Answer<Deleted> {
        self.saw(format!("delete {name}"));
        if self.fail_deletes.load(Ordering::SeqCst) > 0 {
            self.fail_deletes.fetch_sub(1, Ordering::SeqCst);
            return Err(ApiError::new(500, "the delete did not answer"));
        }
        self.sandboxes.lock().unwrap().remove(&name);
        Ok(Deleted { deleted: name })
    }
    async fn spec(&self, name: String) -> Answer<Spec> {
        self.sandboxes
            .lock()
            .unwrap()
            .get(&name)
            .map(|s| s.spec.clone())
            .ok_or_else(|| ApiError::new(404, format!("{name} is not here")))
    }
    async fn receive(&self, spec: Spec) -> Answer<Receiving> {
        self.saw(format!("receive {}", spec.name));
        self.sandboxes.lock().unwrap().insert(
            spec.name.clone(),
            OnHost {
                spec: spec.clone(),
                state: HostState::Receiving,
                alive: Some(true),
                sending: false,
                scratch_used: None,
                created_at: iso_now(),
            },
        );
        Ok(Receiving {
            port: 7100,
            address: self.info.lock().unwrap().wg_address.clone(),
        })
    }
    async fn receive_result(&self, name: String, wait_ms: u64) -> Answer<ReceiveResult> {
        self.saw(format!("result {name} {wait_ms}"));
        let end = tokio::time::Instant::now() + Duration::from_millis(wait_ms.min(3000));
        loop {
            let st = self.sandboxes.lock().unwrap().get(&name).map(|s| s.state);
            match st {
                Some(HostState::Running) => {
                    return Ok(ReceiveResult::Running {
                        stats: Some(json!({"final_ms": 40, "restore_ms": 60, "rounds": 3, "bytes": 1024})),
                    });
                }
                Some(_) if tokio::time::Instant::now() >= end => return Ok(ReceiveResult::Receiving),
                Some(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                None if wait_ms == 0 => return Ok(ReceiveResult::None),
                None => {
                    return Ok(ReceiveResult::Failed {
                        error: "not here".into(),
                    });
                }
            }
        }
    }
    async fn send(&self, name: String, req: SendRequest) -> Answer<SendResult> {
        self.saw(format!("send {name} {}", req.to));
        // sandboxd sends on its own: a client that goes away does not stop it
        let fleet = self.fleet.upgrade().unwrap();
        let me = fleet.sim(&self.name);
        let work = tokio::spawn(async move {
            me.sends_waiting.fetch_add(1, Ordering::SeqCst);
            while me.hold_send.load(Ordering::SeqCst) {
                let n = me.send_gate.notified();
                let _ = tokio::time::timeout(Duration::from_millis(50), n).await;
            }
            me.sends_waiting.fetch_sub(1, Ordering::SeqCst);
            if me.refuse_send.swap(false, Ordering::SeqCst) {
                return Err(ApiError::new(409, "not converging; the sandbox still runs here"));
            }
            if me.lose_answer_keep_sending.swap(false, Ordering::SeqCst) {
                if let Some(s) = me.sandboxes.lock().unwrap().get_mut(&name) {
                    s.sending = true;
                }
                return Err(ApiError::new(500, "connection reset"));
            }
            if me.stop_and_lose_answer.swap(false, Ordering::SeqCst) {
                if let Some(s) = me.sandboxes.lock().unwrap().get_mut(&name) {
                    s.alive = Some(false);
                }
                return Err(ApiError::new(500, "connection reset"));
            }
            let addr = req.to.split(':').next().unwrap_or_default().to_owned();
            let target = fleet
                .by_address(&addr)
                .ok_or_else(|| ApiError::new(502, "no such target"))?;
            let moved = me.sandboxes.lock().unwrap().remove(&name);
            let Some(mut s) = moved else {
                return Err(ApiError::new(404, "not here"));
            };
            s.state = HostState::Running;
            target.sandboxes.lock().unwrap().insert(name, s);
            Ok(())
        });
        work.await.map_err(|e| ApiError::new(500, e.to_string()))??;
        Ok(SendResult {
            ms: 5,
            scratch: ScratchStats {
                live_rounds: 1,
                live_files: 0,
                live_bytes: 0,
                live_ms: 0,
                final_files: 0,
                final_bytes: 0,
                final_deleted: 0,
                final_ms: 1,
                stopped_ms: 1,
            },
        })
    }
    async fn log(&self, name: String, _tail: usize) -> Answer<Vec<u8>> {
        Ok(format!("console of {name}\n").into_bytes())
    }
}

/// A fake host served on a local port; `down` closes its listener and its connections.
struct FakeHost {
    sim: Arc<HostSim>,
    addr: Mutex<Option<SocketAddr>>,
    served: Mutex<Option<aviary_fakes::Served>>,
}

impl FakeHost {
    fn up(&self) {
        let listener = match *self.addr.lock().unwrap() {
            Some(a) => std::net::TcpListener::bind(a).unwrap(),
            None => std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        };
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        *self.addr.lock().unwrap() = Some(listener.local_addr().unwrap());
        let served = aviary_fakes::serve::serve_on(listener, sandboxd_api::router(self.sim.clone())).unwrap();
        *self.served.lock().unwrap() = Some(served);
    }

    fn down(&self) {
        self.served.lock().unwrap().take();
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr.lock().unwrap().unwrap())
    }
}

/// Every fake host, the fake supervisors of their sandboxes, and the connector hermit uses to reach them.
struct Fleet {
    me: std::sync::Weak<Fleet>,
    hosts: Mutex<HashMap<String, Arc<FakeHost>>>,
    /// The fake supervisor of each sandbox address.
    supervisors: Mutex<HashMap<IpAddr, (Arc<FakeSupervisor>, Running)>>,
    keys: Arc<::hermit::tokens::Keys>,
    pool: aviary_pg::Pool,
}

impl Fleet {
    fn new(h: &Harness) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            hosts: Mutex::default(),
            supervisors: Mutex::default(),
            keys: h.keys.clone(),
            pool: h.pool(),
        })
    }

    fn add(&self, name: &str, wg: &str, gateway: bool, mem: u64) -> Arc<HostSim> {
        let mut info: Host = serde_json::from_str(include_str!("../../sandboxd-api/fixtures/host.json")).unwrap();
        info.name = name.into();
        info.wg_address = wg.into();
        info.wg_public_key = ::hermit::fabric::ec2::wg_keys().unwrap().public_key;
        info.gateway = gateway;
        info.role = if gateway {
            sandboxd_api::Role::Hub
        } else {
            sandboxd_api::Role::Burst
        };
        info.public_ipv6 = (!gateway).then(|| "2001:db8::10".into());
        info.mem_total = mem;
        info.mem_available = mem;
        info.reserve_memory = if gateway { mem } else { GIB };
        info.cpus = 4;
        info.cpu_features = if gateway {
            String::new()
        } else {
            "fpu,sse4_2,avx2".into()
        };
        info.gvisor_release = "r1".into();
        info.jfs_mounted = if gateway { None } else { Some(true) };
        info.sandboxes = vec![];
        info.images = vec![];
        let sim = Arc::new(HostSim {
            name: name.into(),
            fleet: self.me.clone(),
            info: Mutex::new(info),
            sandboxes: Mutex::default(),
            peers: Mutex::default(),
            egress: Mutex::default(),
            images: Mutex::default(),
            calls: Mutex::default(),
            refuse_send: AtomicBool::new(false),
            hold_send: AtomicBool::new(false),
            send_gate: Notify::new(),
            sends_waiting: AtomicU32::new(0),
            stop_and_lose_answer: AtomicBool::new(false),
            lose_answer_keep_sending: AtomicBool::new(false),
            fail_deletes: AtomicU32::new(0),
        });
        let host = Arc::new(FakeHost {
            sim: sim.clone(),
            addr: Mutex::new(None),
            served: Mutex::new(None),
        });
        host.up();
        self.hosts.lock().unwrap().insert(name.into(), host);
        sim
    }

    fn host(&self, name: &str) -> Arc<FakeHost> {
        self.hosts.lock().unwrap().get(name).cloned().unwrap()
    }

    fn sim(&self, name: &str) -> Arc<HostSim> {
        self.host(name).sim.clone()
    }

    fn by_address(&self, addr: &str) -> Option<Arc<HostSim>> {
        self.hosts
            .lock()
            .unwrap()
            .values()
            .find(|h| h.sim.info.lock().unwrap().wg_address == addr)
            .map(|h| h.sim.clone())
    }

    /// A fake supervisor for a created sandbox (its control token is derived from the row's id).
    async fn supervisor_for(&self, spec: &Spec) {
        let ip: IpAddr = spec.ip.parse().unwrap();
        if self.supervisors.lock().unwrap().contains_key(&ip) {
            return;
        }
        let id: Option<Uuid> = self
            .pool
            .get()
            .await
            .unwrap()
            .query_opt("select id from sandboxes where name = $1", &[&spec.name])
            .await
            .unwrap()
            .map(|r| r.get(0));
        let Some(id) = id else { return };
        let token = self.keys.control_token(self.keys.newest(), id).unwrap();
        let sup = FakeSupervisor::new(&token);
        let running = sup.serve().await.unwrap();
        self.supervisors.lock().unwrap().insert(ip, (sup, running));
    }
}

impl Connect for Fleet {
    fn connect(&self, name: &str, _wg_ip: IpAddr) -> Result<Client, sandboxd_api::Error> {
        let url = self
            .hosts
            .lock()
            .unwrap()
            .get(name)
            .map_or_else(|| "http://127.0.0.1:9".to_owned(), |h| h.url());
        Client::plain(&url)
    }
}

impl ControlUrl for Fleet {
    fn url(&self, ip: IpAddr) -> String {
        self.supervisors
            .lock()
            .unwrap()
            .get(&ip)
            .map_or_else(|| "http://127.0.0.1:9".to_owned(), |(_, r)| r.url())
    }
}

/// One `RunInstances`: the instance id, the user data, the tags.
type Launched = (String, serde_json::Value, Vec<(String, String)>);

/// EC2 in memory: a launched instance becomes a fake host named and addressed by its user data.
struct FakeEc2 {
    fleet: Arc<Fleet>,
    launched: Mutex<Vec<Launched>>,
    terminated: Mutex<Vec<String>>,
    /// Extra instances `DescribeInstances` lists (orphans).
    extra: Mutex<Vec<Instance>>,
    /// The next `RunInstances` fails with this code.
    fail: Mutex<Option<String>>,
    /// Hosts come up with their `JuiceFS` mount missing.
    unmounted: AtomicBool,
    n: AtomicU32,
}

impl FakeEc2 {
    fn new(fleet: Arc<Fleet>) -> Arc<Self> {
        Arc::new(Self {
            fleet,
            launched: Mutex::default(),
            terminated: Mutex::default(),
            extra: Mutex::default(),
            fail: Mutex::default(),
            unmounted: AtomicBool::new(false),
            n: AtomicU32::new(0),
        })
    }

    fn launched(&self) -> usize {
        self.launched.lock().unwrap().len()
    }

    fn user_data(&self, i: usize) -> serde_json::Value {
        self.launched.lock().unwrap()[i].1.clone()
    }

    fn terminated(&self) -> Vec<String> {
        self.terminated.lock().unwrap().clone()
    }
}

impl Ec2 for FakeEc2 {
    fn run_instance<'a>(
        &'a self,
        _template: &'a str,
        user_data: &'a str,
        tags: &'a [(String, String)],
    ) -> Fut<'a, Result<Instance, Ec2Error>> {
        Box::pin(async move {
            if let Some(code) = self.fail.lock().unwrap().take() {
                return Err(Ec2Error {
                    code,
                    message: "You have requested more vCPU capacity than your current vCPU limit".into(),
                });
            }
            let ud: serde_json::Value = serde_json::from_str(user_data).unwrap();
            let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
            let id = format!("i-{n:04}");
            let name = ud["host"]["name"].as_str().unwrap().to_owned();
            let wg = ud["host"]["wgAddress"].as_str().unwrap().to_owned();
            let sim = self.fleet.add(&name, &wg, false, 16 * GIB);
            if self.unmounted.load(Ordering::SeqCst) {
                sim.set(|h| h.jfs_mounted = Some(false));
            }
            self.launched.lock().unwrap().push((id.clone(), ud, tags.to_vec()));
            Ok(Instance {
                id,
                state: "pending".into(),
                private_ip: Some(format!("172.31.0.{}", 10 + n).parse().unwrap()),
                launched_ms: now_ms(),
            })
        })
    }

    fn terminate<'a>(&'a self, ids: &'a [String]) -> Fut<'a, Result<(), Ec2Error>> {
        Box::pin(async move {
            self.terminated.lock().unwrap().extend(ids.iter().cloned());
            Ok(())
        })
    }

    fn describe<'a>(&'a self, _tags: &'a [(String, String)]) -> Fut<'a, Result<Vec<Instance>, Ec2Error>> {
        Box::pin(async move {
            let gone = self.terminated();
            let mut out: Vec<Instance> = self
                .launched
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, ..)| !gone.contains(id))
                .map(|(id, ..)| Instance {
                    id: id.clone(),
                    state: "running".into(),
                    private_ip: None,
                    launched_ms: now_ms(),
                })
                .collect();
            out.extend(
                self.extra
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|i| !gone.contains(&i.id))
                    .cloned(),
            );
            Ok(out)
        })
    }
}

fn now_ms() -> i64 {
    i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap()
}

// ---- the setup -----------------------------------------------------------------------------------------------

/// A test: its harness, fleet (with the gateway), EC2, egress source and the fabric of each replica.
struct Rig {
    h: Harness,
    fleet: Arc<Fleet>,
    ec2: Arc<FakeEc2>,
    egress: Arc<Fixed>,
}

impl Rig {
    async fn new(test: &str, edit: impl FnOnce(&mut serde_json::Value), timings: Timings) -> Self {
        let config = common::config(|v| {
            v["sandbox"]["memory"] = json!("2Gi");
            v["fabric"]["migration"]["minAgeSeconds"] = json!(0);
            v["fabric"]["cpuFeatures"] = json!("sse4_2,avx2");
            edit(v);
        });
        let h = Harness::with(test, config, timings).await;
        let fleet = Fleet::new(&h);
        fleet.add(GATEWAY, "198.19.0.1", true, 8 * GIB);
        let ec2 = FakeEc2::new(fleet.clone());
        Self {
            h,
            fleet,
            ec2,
            egress: Arc::new(Fixed::new(Some(vec!["10.244.1.5".into()]))),
        }
    }

    fn fabric(&self) -> Arc<Gvisor> {
        Arc::new(Gvisor::new(Deps {
            connect: self.fleet.clone(),
            ec2: Some(self.ec2.clone()),
            egress: self.egress.clone(),
            host_secrets: Some(HostSecrets {
                ca: "CA".into(),
                cert: "CERT".into(),
                key: "KEY".into(),
                juicefs_meta_url: "postgres://juicefs@192.0.2.2:30432/jfs".into(),
                juicefs_meta_password: "pw".into(),
            }),
            control: self.fleet.clone(),
        }))
    }

    async fn replica(&self, pod: &str) -> (Replica, Arc<Gvisor>) {
        let g = self.fabric();
        let r = self.h.replica_on(pod, g.clone()).await;
        (r, g)
    }

    async fn sql(&self, sql: &str) -> u64 {
        self.h.sql(sql).await
    }

    async fn host_status(&self, name: &str) -> String {
        self.h
            .pool()
            .get()
            .await
            .unwrap()
            .query_opt("select status from hosts where name = $1", &[&name])
            .await
            .unwrap()
            .map_or_else(|| "none".to_owned(), |r| r.get(0))
    }

    async fn host_of(&self, id: Uuid) -> Option<String> {
        db::load(&self.h.pool(), id).await.unwrap().unwrap().host
    }

    async fn events(&self, kind: &str) -> Vec<serde_json::Value> {
        self.h
            .pool()
            .get()
            .await
            .unwrap()
            .query("select data from host_events where kind = $1 order by id", &[&kind])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect()
    }

    /// A ready sandbox on `host` (row and fake host both), as jarvis3's `newRow` + `ready`.
    async fn ready_on(&self, host: &str, ip: &str) -> (Uuid, String) {
        let id = Uuid::now_v7();
        let name = format!("sbx-{}", &id.simple().to_string()[..16]);
        let ipa: IpAddr = ip.parse().unwrap();
        let image = self.h.config.image_digest.clone();
        // the host lists it before the row exists, so no heartbeat can miss it
        let spec = Spec {
            name: name.clone(),
            ip: ip.into(),
            image: image.trim_start_matches("sha256:").into(),
            image_ref: None,
            env: sandboxd_api::Env::default(),
            cpu: 1.0,
            memory: 2 * GIB,
            cpu_features: String::new(),
            workspace: None,
            pids: None,
            scratch_bytes: None,
        };
        self.fleet.sim(host).sandboxes.lock().unwrap().insert(
            name.clone(),
            OnHost {
                spec,
                state: HostState::Running,
                alive: Some(true),
                sending: false,
                scratch_used: Some(0),
                created_at: iso_now(),
            },
        );
        self.h
            .pool()
            .get()
            .await
            .unwrap()
            .execute(
                "insert into sandboxes (id, conversation_id, person, status, name, token_kid, host, ip, image, cpu,
                   memory_bytes, start_deadline, expires_at, hard_expires_at, created_at, ready_at, last_heartbeat_at)
                 values ($1, $2, $3, 'ready', $4, 1, $5, $6, $7, 1, $8, now() + interval '1 hour',
                   now() + interval '8 hours', now() + interval '9 hours', now() - interval '1 minute',
                   now() - interval '1 minute', now())",
                &[
                    &id,
                    &Uuid::now_v7(),
                    &STAFF,
                    &name,
                    &host,
                    &ipa,
                    &image,
                    &i64::try_from(2 * GIB).unwrap(),
                ],
            )
            .await
            .unwrap();
        (id, name)
    }

    /// Completes a move [`HostSim::stop_and_lose_answer`] left half done: the target's receive restores it.
    fn finish_move(&self, name: &str, from: &str, to: &str) {
        let mut s = self.fleet.sim(from).sandboxes.lock().unwrap().remove(name).unwrap();
        s.state = HostState::Running;
        s.alive = Some(true);
        s.sending = false;
        s.scratch_used = Some(0);
        self.fleet.sim(to).sandboxes.lock().unwrap().insert(name.into(), s);
    }

    /// The AWS host names in launch order.
    fn aws(&self, i: usize) -> String {
        self.ec2.user_data(i)["host"]["name"].as_str().unwrap().to_owned()
    }
}

/// Timings for tests that run the scaler's passes themselves.
fn manual() -> Timings {
    Timings {
        scaler: Duration::from_secs(3600),
        reconcile: Duration::ZERO,
        ..common::timings()
    }
}

async fn pass(g: &Gvisor, r: &Replica) -> Report {
    scaler::pass(g, r.core()).await.unwrap()
}

async fn demand(rig: &Rig, person: &str) {
    rig.h
        .pool()
        .get()
        .await
        .unwrap()
        .execute(
            "insert into demand_hints (person, at) values ($1, now()) on conflict (person) do update set at = now()",
            &[&person],
        )
        .await
        .unwrap();
}

/// A starting row (as a `PUT` inserts it), for placement.
async fn starting(rig: &Rig) -> Uuid {
    let id = Uuid::now_v7();
    let name = format!("sbx-{}", &id.simple().to_string()[..16]);
    rig.h
        .pool()
        .get()
        .await
        .unwrap()
        .execute(
            "insert into sandboxes (id, conversation_id, person, status, phase, name, token_kid, cpu, memory_bytes,
               start_deadline, expires_at, hard_expires_at)
             values ($1, $2, $3, 'starting', 'placing', $4, 1, 1, $5, now() + interval '1 hour',
               now() + interval '8 hours', now() + interval '9 hours')",
            &[&id, &Uuid::now_v7(), &STAFF, &name, &i64::try_from(2 * GIB).unwrap()],
        )
        .await
        .unwrap();
    id
}

async fn eventually<F, Fu>(f: F) -> bool
where
    F: FnMut() -> Fu,
    Fu: std::future::Future<Output = bool>,
{
    common::eventually(WAIT, f).await
}

// ---- the tests -----------------------------------------------------------------------------------------------

/// `vm.test.ts` "placement, burst to AWS, drain-back by live migration, termination", for a fabric whose gateway
/// runs no sandboxes: demand launches an AWS host with the right user data, it becomes active once mounted, takes
/// the placements, packing moves a sandbox onto the fuller host (not across gVisor releases, not when the send would
/// not converge), and the emptied host is terminated once past its minimum lifetime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn burst_placement_packing_and_release() {
    let rig = Rig::new("fabric_burst", |_| {}, manual()).await;
    let (r, g) = rig.replica("hermit-a").await;
    let first = pass(&g, &r).await;
    assert!(first.gateway_up);
    assert_eq!(rig.host_status(GATEWAY).await, "active");
    // no demand, no spare slots: nothing launched; a placement finds no slot
    assert_eq!(rig.ec2.launched(), 0);
    let s0 = starting(&rig).await;
    assert_eq!(
        g.place(s0, 2 * GIB).await.unwrap(),
        Place::NoSlot,
        "the gateway runs no sandboxes"
    );

    // someone types: one host
    demand(&rig, "a@example.org").await;
    let p = pass(&g, &r).await;
    assert_eq!(p.launched, vec!["scaler".to_owned()]);
    assert_eq!(rig.ec2.launched(), 1);
    let ud = rig.ec2.user_data(0);
    let a = rig.aws(0);
    assert_eq!(ud["host"]["role"], "burst");
    assert_eq!(ud["host"]["wgAddress"], "198.19.0.10");
    assert_eq!(ud["host"]["sandboxCidr"], "198.18.0.0/16");
    assert_eq!(ud["host"]["imageSource"], "http://198.19.0.1:7444");
    assert_eq!(ud["host"]["egressPorts"], json!([[3128, 3128]]));
    let gw_key = rig.fleet.sim(GATEWAY).info.lock().unwrap().wg_public_key.clone();
    assert_eq!(ud["host"]["hub"]["publicKey"], json!(gw_key));
    assert_eq!(ud["host"]["hub"]["endpoint"], "[2001:db8::1]:51821");
    assert_eq!(ud["tls"], json!({"ca": "CA", "cert": "CERT", "key": "KEY"}));
    assert_eq!(ud["juicefs"]["rootSquash"], "1000:1000");
    assert!(ud.to_string().len() < 16 << 10);
    let tags = rig.ec2.launched.lock().unwrap()[0].2.clone();
    assert!(tags.contains(&("app".into(), "hermit-host".into())));
    assert!(tags.contains(&("Name".into(), format!("hermit-host-{a}"))));
    assert_eq!(rig.host_status(&a).await, "booting");
    // the private key is only in the user data; the row has the public one
    let k: String = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select wg_public_key from hosts where name = $1", &[&a])
        .await
        .unwrap()
        .get(0);
    assert_ne!(json!(k), ud["wgPrivateKey"]);

    // the next pass: active (mounted), the image warmed, no second host while its slots count
    let p = pass(&g, &r).await;
    assert_eq!(p.activated, vec![a.clone()]);
    assert_eq!(rig.ec2.launched(), 1);
    assert_eq!(rig.host_status(&a).await, "active");
    let digest = rig.h.config.image_digest.trim_start_matches("sha256:").to_owned();
    assert!(eventually(|| async { rig.fleet.sim(&a).calls().contains(&format!("image {digest}")) }).await);
    assert_eq!(rig.events("active").await.len(), 1);
    // the gateway, the image server of hosts without registry access, pulls the configured image (by its ref), once
    let pulled = format!("image {digest} ref {}", rig.h.config.file.sandbox.image);
    assert!(eventually(|| async { rig.fleet.sim(GATEWAY).calls().contains(&pulled) }).await);
    let _ = pass(&g, &r).await;
    let asked = rig
        .fleet
        .sim(GATEWAY)
        .calls()
        .iter()
        .filter(|c| c.starts_with("image "))
        .count();
    assert_eq!(asked, 1, "the gateway is asked once per digest");

    // placement: the AWS host, the first sandbox address; the gateway routes its /32 there
    let Place::Placed(pl) = g.place(s0, 2 * GIB).await.unwrap() else {
        panic!("not placed")
    };
    assert_eq!(
        (pl.host.as_str(), pl.ip.to_string().as_str()),
        (a.as_str(), "198.18.0.10")
    );
    assert_eq!(
        g.place(s0, 2 * GIB).await.unwrap(),
        Place::Placed(pl.clone()),
        "a placed row keeps its place"
    );
    rig.sql(&format!("update sandboxes set status = 'gone' where id = '{s0}'"))
        .await;
    pass(&g, &r).await;
    let peers = rig.fleet.sim(GATEWAY).peers.lock().unwrap().clone().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].endpoint.as_deref(), Some("[2001:db8::10]:51820"));
    assert_eq!(peers[0].allowed_ips, vec!["198.19.0.10/32".to_owned()]);
    assert_eq!(
        *rig.fleet.sim(GATEWAY).egress.lock().unwrap(),
        Some(vec!["10.244.1.5".to_owned()])
    );

    // a second host (the cap of the first is 1), one sandbox on each
    rig.sql(&format!("update hosts set max_sandboxes = 1 where name = '{a}'"))
        .await;
    let (x, _) = rig.ready_on(&a, "198.18.0.20").await;
    demand(&rig, "b@example.org").await;
    pass(&g, &r).await;
    assert_eq!(rig.ec2.launched(), 2);
    let b = rig.aws(1);
    pass(&g, &r).await;
    assert_eq!(rig.host_status(&b).await, "active");
    let (y, y_name) = rig.ready_on(&b, "198.18.0.21").await;
    rig.sql("delete from demand_hints").await;
    rig.sql(&format!("update hosts set max_sandboxes = null where name = '{a}'"))
        .await;
    // hosts on different gVisor releases: no move (runsc restore refuses another release's checkpoint)
    rig.fleet.sim(&b).set(|h| h.gvisor_release = "r2".into());
    let p = pass(&g, &r).await;
    assert_eq!(p.migration, None);
    let p = pass(&g, &r).await;
    assert_eq!(p.migration, None);
    let peers = rig.fleet.sim(GATEWAY).peers.lock().unwrap().clone().unwrap();
    let all: Vec<String> = peers.iter().flat_map(|p| p.allowed_ips.clone()).collect();
    assert!(all.contains(&"198.18.0.20/32".to_owned()) && all.contains(&"198.18.0.21/32".to_owned()));
    rig.fleet.sim(&b).set(|h| h.gvisor_release = "r1".into());
    // a send that would not converge leaves it where it was, the target's half-received copy removed
    let (from_x, from_y) = (rig.fleet.sim(&a), rig.fleet.sim(&b));
    from_x.refuse_send.store(true, Ordering::SeqCst);
    from_y.refuse_send.store(true, Ordering::SeqCst);
    let p = pass(&g, &r).await;
    let (moved, from, to) = p.migration.clone().expect("a packing move");
    assert!(eventually(|| async { !rig.events("migration-skipped").await.is_empty() }).await);
    assert_eq!(rig.host_of(moved).await.as_deref(), Some(from.as_str()));
    assert!(eventually(|| async { db::load(&rig.h.pool(), moved).await.unwrap().unwrap().task.is_none() }).await);
    assert!(
        !rig.fleet
            .sim(&to)
            .sandboxes
            .lock()
            .unwrap()
            .values()
            .any(|s| s.state == HostState::Receiving)
    );
    from_x.refuse_send.store(false, Ordering::SeqCst);
    from_y.refuse_send.store(false, Ordering::SeqCst);
    // now it moves: the row follows, the gateway's peers with it, the event has the pause
    let p = pass(&g, &r).await;
    let (moved, from, to) = p.migration.expect("a packing move");
    assert!(eventually(|| async { rig.host_of(moved).await.as_deref() == Some(to.as_str()) }).await);
    let row = db::load(&rig.h.pool(), moved).await.unwrap().unwrap();
    assert_eq!((row.migrating_to, row.task), (None, None));
    let m = rig.events("migration").await;
    assert_eq!(m.last().unwrap()["pause_ms"], json!(100.0));
    assert_eq!(m.last().unwrap()["reason"], "packing");
    assert_ne!(rig.events("pack").await.len(), 0);
    let moved_name = row.name.clone();
    assert!(moved == x || moved == y);
    let _ = y_name;
    assert!(rig.fleet.sim(&to).running(&moved_name));
    let peers = rig.fleet.sim(GATEWAY).peers.lock().unwrap().clone().unwrap();
    let ip = row.ip.unwrap().to_string();
    let holder = peers
        .iter()
        .find(|p| p.allowed_ips.contains(&format!("{ip}/32")))
        .unwrap();
    assert!(
        holder
            .allowed_ips
            .contains(&format!("{}/32", if to == a { "198.19.0.10" } else { "198.19.0.11" }))
    );
    // the emptied host: kept within its minimum lifetime, then terminated
    pass(&g, &r).await;
    assert_eq!(rig.host_status(&from).await, "active");
    rig.sql(&format!(
        "update hosts set created_at = now() - interval '16 minutes' where name = '{from}'"
    ))
    .await;
    let p = pass(&g, &r).await;
    assert_eq!(p.terminated, vec![from.clone()]);
    assert_eq!(rig.host_status(&from).await, "gone");
    assert_eq!(rig.ec2.terminated().len(), 1);
    assert_eq!(rig.events("terminate").await.last().unwrap()["reason"], "empty");
    // destroy follows the sandbox to where it is now
    g.destroy(Some(&to), None, &moved_name).await.unwrap();
    assert!(!rig.fleet.sim(&to).running(&moved_name));
}

/// `vm.test.ts` "never more than maxHosts AWS hosts; a full pool is reported, not waited out", and capacity
/// planning across replicas: two people typing at once start one host (pending slots count), a burst starts
/// enough at once, and the next launch finds it planned.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn launches_are_capped_and_planned_once_across_replicas() {
    let rig = Rig::new("fabric_cap", |v| v["fabric"]["aws"]["maxHosts"] = json!(2), manual()).await;
    let (r1, g1) = rig.replica("hermit-a").await;
    let (_r2, g2) = rig.replica("hermit-b").await;
    pass(&g1, &r1).await;
    demand(&rig, "a@example.org").await;
    demand(&rig, "b@example.org").await;
    let (l1, l2) = tokio::join!(g1.request_host(), g2.request_host());
    assert_eq!(
        rig.ec2.launched(),
        1,
        "the second signal fits into the booting host's pending slots"
    );
    assert!(matches!(
        (l1, l2),
        (Launch::Launched, Launch::Planned) | (Launch::Planned, Launch::Launched)
    ));
    let b: i64 = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from hosts where kind = 'aws' and status = 'booting'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(b, 1);
    // nothing is placed on a booting host
    assert_eq!(g2.place(starting(&rig).await, 2 * GIB).await.unwrap(), Place::NoSlot);
    // a burst bigger than one host's slots (7): the second host at once, then the cap
    rig.sql("update sandboxes set status = 'gone'").await;
    for i in 0..9 {
        demand(&rig, &format!("u{i}@example.org")).await;
    }
    assert_eq!(g2.request_host().await, Launch::Launched);
    assert_eq!(
        rig.ec2.launched(),
        2,
        "9 people typing, 7 pending: one more host (the cap is 2)"
    );
    assert_eq!(
        g1.request_host().await,
        Launch::Planned,
        "14 pending slots cover 11 people"
    );
    for i in 9..20 {
        demand(&rig, &format!("u{i}@example.org")).await;
    }
    assert_eq!(
        g1.request_host().await,
        Launch::Full,
        "at the cap: reported, not waited out"
    );
    assert_eq!(rig.ec2.launched(), 2);
    // off: the operator's switch and maxHosts 0
    rig.sql("update controls set sandboxes_enabled = false").await;
    assert_eq!(g1.request_host().await, Launch::Off);
}

/// `vm.test.ts` "typing starts an AWS host only when no slot is free; an empty host goes once past its minimum
/// lifetime", with the demand window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn demand_counts_for_its_window_and_a_host_with_a_sandbox_is_kept() {
    let rig = Rig::new("fabric_demand", |_| {}, manual()).await;
    let (r, g) = rig.replica("hermit-a").await;
    pass(&g, &r).await;
    assert_eq!(rig.ec2.launched(), 0);
    // a hint older than 120 s does not count
    demand(&rig, "old@example.org").await;
    rig.sql("update demand_hints set at = now() - interval '125 seconds'")
        .await;
    pass(&g, &r).await;
    assert_eq!(rig.ec2.launched(), 0);
    demand(&rig, "a@example.org").await;
    pass(&g, &r).await;
    assert_eq!(rig.ec2.launched(), 1);
    pass(&g, &r).await;
    let a = rig.aws(0);
    assert_eq!(rig.host_status(&a).await, "active");
    // a free slot now: typing again starts nothing
    demand(&rig, "b@example.org").await;
    pass(&g, &r).await;
    assert_eq!(rig.ec2.launched(), 1);
    rig.sql("delete from demand_hints").await;
    // a sandbox keeps it past the minimum lifetime
    let (s, _) = rig.ready_on(&a, "198.18.0.30").await;
    rig.sql(&format!(
        "update hosts set created_at = now() - interval '16 minutes' where name = '{a}'"
    ))
    .await;
    pass(&g, &r).await;
    assert_eq!(rig.host_status(&a).await, "active");
    let empty: Option<time::OffsetDateTime> = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select empty_since from hosts where name = $1", &[&a])
        .await
        .unwrap()
        .get(0);
    assert!(empty.is_none());
    // no sandbox and past 15 minutes: terminated
    rig.sql(&format!("update sandboxes set status = 'gone' where id = '{s}'"))
        .await;
    let p = pass(&g, &r).await;
    assert_eq!(p.terminated, vec![a.clone()]);
    // hints older than an hour are pruned
    demand(&rig, "c@example.org").await;
    rig.sql("update demand_hints set at = now() - interval '2 hours'").await;
    pass(&g, &r).await;
    let n: i64 = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from demand_hints", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
}

/// `vm.test.ts` "a hub renamed in config (the same overlay address) replaces the old row instead of failing".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renamed_gateway_replaces_the_old_row() {
    let rig = Rig::new("fabric_rename", |_| {}, manual()).await;
    let (r, g) = rig.replica("hermit-a").await;
    rig.sql("insert into hosts (name, kind, url, wg_ip, status) values ('old-gw', 'gateway', 'https://198.19.0.1:7443', '198.19.0.1', 'active')").await;
    pass(&g, &r).await;
    assert_eq!(rig.host_status("old-gw").await, "gone");
    assert_eq!(rig.host_status(GATEWAY).await, "active");
    let why: String = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select gone_reason from hosts where name = 'old-gw'", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(why, format!("replaced by {GATEWAY}"));
}

/// Brings one AWS host up (launch, active) and returns its name.
async fn one_host(rig: &Rig, g: &Gvisor, r: &Replica) -> String {
    pass(g, r).await;
    demand(rig, "a@example.org").await;
    pass(g, r).await;
    pass(g, r).await;
    rig.sql("delete from demand_hints").await;
    let a = rig.aws(rig.ec2.launched() - 1);
    assert_eq!(rig.host_status(&a).await, "active");
    a
}

/// hermit 13.7 and `vm.test.ts` "a host that stops answering loses its sandboxes and is terminated": not while the
/// gateway has only just come back, then `gone`/`sandbox_lost` within 10 s of the host's death with the scaler
/// running on its own; a gateway that stops answering judges nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_loss_ends_its_sandboxes_within_ten_seconds() {
    let rig = Rig::new(
        "fabric_loss",
        |v| v["fabric"]["hostDeadSeconds"] = json!(2),
        Timings {
            scaler: Duration::from_millis(200),
            ..manual()
        },
    )
    .await;
    let (r, g) = rig.replica("hermit-a").await;
    // the replica's own scaler runs; wait for the gateway, then launch
    assert!(eventually(|| async { rig.host_status(GATEWAY).await == "active" }).await);
    demand(&rig, "a@example.org").await;
    assert!(eventually(|| async { rig.ec2.launched() == 1 }).await);
    let a = rig.aws(0);
    assert!(eventually(|| async { rig.host_status(&a).await == "active" }).await);
    rig.sql("delete from demand_hints").await;
    let (s, _) = rig.ready_on(&a, "198.18.0.40").await;
    // the gateway goes away first: nothing is judged while it is away
    rig.fleet.host(GATEWAY).down();
    rig.fleet.host(&a).down();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        rig.host_status(&a).await,
        "active",
        "AWS hosts are not judged while the gateway is away"
    );
    assert!(!g.gateway_settled());
    // the gateway returns: the host is judged only once it has answered for hostDeadSeconds
    rig.fleet.host(GATEWAY).up();
    let back = tokio::time::Instant::now();
    assert!(
        eventually(|| async { db::load(&rig.h.pool(), s).await.unwrap().unwrap().status == SandboxState::Gone }).await
    );
    assert!(
        back.elapsed() >= Duration::from_secs(2),
        "not judged before the gateway settled"
    );
    assert!(back.elapsed() < Duration::from_secs(10), "{:?}", back.elapsed());
    let row = db::load(&rig.h.pool(), s).await.unwrap().unwrap();
    assert_eq!(row.release_reason.as_deref(), Some("host_lost"));
    assert_eq!(row.release_code.as_deref(), Some("sandbox_lost"));
    assert_eq!(rig.host_status(&a).await, "gone");
    assert_eq!(rig.ec2.terminated(), vec!["i-0001".to_owned()]);
    let lost = rig.events("host-lost").await;
    assert_eq!(lost.last().unwrap()["sandboxes"], json!(1));
    // platform 15.4: nothing could be fetched from the lost host, and the row says so
    let (bytes, reason): (Option<Vec<u8>>, String) = {
        let r = rig
            .h
            .pool()
            .get()
            .await
            .unwrap()
            .query_one("select bytes, reason from sandbox_logs where sandbox_id = $1", &[&s])
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!((bytes, reason.as_str()), (None, "host_lost"));

    // a host that dies with the gateway answering: within 10 s of its death
    demand(&rig, "b@example.org").await;
    assert!(eventually(|| async { rig.ec2.launched() == 2 }).await);
    let b = rig.aws(1);
    assert!(eventually(|| async { rig.host_status(&b).await == "active" }).await);
    rig.sql("delete from demand_hints").await;
    let (t, _) = rig.ready_on(&b, "198.18.0.41").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    rig.fleet.host(&b).down();
    let died = tokio::time::Instant::now();
    assert!(
        eventually(|| async { db::load(&rig.h.pool(), t).await.unwrap().unwrap().status == SandboxState::Gone }).await
    );
    assert!(died.elapsed() < Duration::from_secs(10), "{:?}", died.elapsed());
    drop(r);
}

/// hermit 13.4 step 3: a sandbox its host no longer lists, or lists with `alive: false` (an OOM-killed sentry),
/// is `gone`/`sandbox_lost` (`vanished`) within 10 s; a row being migrated is never judged from its source alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vanished_sandboxes_are_lost_within_ten_seconds() {
    let rig = Rig::new(
        "fabric_vanished",
        |_| {},
        Timings {
            scaler: Duration::from_millis(200),
            ..manual()
        },
    )
    .await;
    let (r, g) = rig.replica("hermit-a").await;
    assert!(eventually(|| async { rig.host_status(GATEWAY).await == "active" }).await);
    demand(&rig, "a@example.org").await;
    assert!(eventually(|| async { rig.ec2.launched() == 1 }).await);
    let a = rig.aws(0);
    assert!(eventually(|| async { rig.host_status(&a).await == "active" }).await);
    rig.sql("delete from demand_hints").await;
    let (oom, oom_name) = rig.ready_on(&a, "198.18.0.50").await;
    let (gone, gone_name) = rig.ready_on(&a, "198.18.0.51").await;
    let (moving, moving_name) = rig.ready_on(&a, "198.18.0.52").await;
    rig.sql(&format!(
        "update sandboxes set migrating_to = 'aws-elsewhere' where id = '{moving}'"
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let t0 = tokio::time::Instant::now();
    rig.fleet
        .sim(&a)
        .sandboxes
        .lock()
        .unwrap()
        .get_mut(&oom_name)
        .unwrap()
        .alive = Some(false);
    rig.fleet.sim(&a).sandboxes.lock().unwrap().remove(&gone_name);
    rig.fleet.sim(&a).sandboxes.lock().unwrap().remove(&moving_name);
    for id in [oom, gone] {
        assert!(
            eventually(|| async { db::load(&rig.h.pool(), id).await.unwrap().unwrap().status == SandboxState::Gone })
                .await
        );
        let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
        assert_eq!(
            (row.release_reason.as_deref(), row.release_code.as_deref()),
            (Some("vanished"), Some("sandbox_lost"))
        );
    }
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        db::load(&rig.h.pool(), moving).await.unwrap().unwrap().status,
        SandboxState::Ready
    );
    drop((r, g));
}

/// `vm.test.ts` "a new AWS host stays booting until its `JuiceFS` mount is there; an active host without it takes no
/// sandbox and retires if it stays so", and the boot timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn juicefs_gates_hosts() {
    let rig = Rig::new(
        "fabric_jfs",
        |v| {
            v["fabric"]["jfsRetireSeconds"] = json!(1);
        },
        manual(),
    )
    .await;
    let (r, g) = rig.replica("hermit-a").await;
    pass(&g, &r).await;
    rig.ec2.unmounted.store(true, Ordering::SeqCst);
    demand(&rig, "a@example.org").await;
    pass(&g, &r).await;
    let a = rig.aws(0);
    // sandboxd answers but the mount is not there: still booting, its slots pending, no second host
    pass(&g, &r).await;
    assert_eq!(rig.host_status(&a).await, "booting");
    assert_eq!(rig.ec2.launched(), 1);
    assert_eq!(rig.events("active").await.len(), 0);
    // mounted: active once
    rig.fleet.sim(&a).set(|h| h.jfs_mounted = Some(true));
    pass(&g, &r).await;
    pass(&g, &r).await;
    assert_eq!(rig.host_status(&a).await, "active");
    assert_eq!(rig.events("active").await.len(), 1);
    rig.sql("delete from demand_hints").await;
    let s = starting(&rig).await;
    assert!(matches!(g.place(s, 2 * GIB).await.unwrap(), Place::Placed(p) if p.host == a));
    // the mount went away: active, keeps its sandbox, takes no new one; recorded once
    rig.fleet.sim(&a).set(|h| h.jfs_mounted = Some(false));
    pass(&g, &r).await;
    pass(&g, &r).await;
    assert_eq!(rig.host_status(&a).await, "active");
    assert_eq!(rig.events("jfs-unmounted").await.len(), 1);
    assert_eq!(g.place(starting(&rig).await, 2 * GIB).await.unwrap(), Place::NoSlot);
    // back: placeable again
    rig.fleet.sim(&a).set(|h| h.jfs_mounted = Some(true));
    pass(&g, &r).await;
    assert_eq!(rig.events("jfs-mounted").await.len(), 1);
    assert!(matches!(
        g.place(starting(&rig).await, 2 * GIB).await.unwrap(),
        Place::Placed(_)
    ));
    // unmounted for longer than jfsRetireSeconds: retiring (system), its sandboxes move off
    rig.fleet.sim(&a).set(|h| h.jfs_mounted = Some(false));
    pass(&g, &r).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let p = pass(&g, &r).await;
    assert_eq!(p.retired, vec![a.clone()]);
    assert_eq!(rig.host_status(&a).await, "retiring");
    assert_eq!(rig.events("jfs-retire").await.len(), 1);
    // a retiring host with sandboxes gets a replacement (one over the cap if need be)
    assert_eq!(rig.ec2.launched(), 2);
    // a booting host that never mounts is lost after the boot timeout
    let b = rig.aws(1);
    rig.sql(&format!(
        "update hosts set created_at = now() - interval '9 minutes' where name = '{b}'"
    ))
    .await;
    let p = pass(&g, &r).await;
    assert!(p.lost_hosts.contains(&b), "{p:?}");
    assert_eq!(rig.host_status(&b).await, "gone");
}

/// hermit 13.4 step 2: a host without a CPU feature of the pool retires (`retire_by` system).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_missing_a_cpu_feature_retires() {
    let rig = Rig::new("fabric_cpu", |_| {}, manual()).await;
    let (r, g) = rig.replica("hermit-a").await;
    let a = one_host(&rig, &g, &r).await;
    rig.fleet.sim(&a).set(|h| h.cpu_features = "fpu,sse4_2".into());
    pass(&g, &r).await;
    assert_eq!(rig.host_status(&a).await, "retiring");
    let by: Option<String> = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select retire_by from hosts where name = $1", &[&a])
        .await
        .unwrap()
        .get(0);
    assert_eq!(by.as_deref(), Some("system"));
    assert_eq!(rig.events("retire").await[0]["missing"], json!(["avx2"]));
}

/// hermit 13.9: an instance hermit does not know is terminated once older than `orphanInstanceSeconds`; a live
/// row whose instance is gone is lost; a failed `RunInstances` leaves the row `gone` with the AWS error code; a
/// sandbox name a host lists with no row is destroyed there (janitor step 8).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ec2_reconcile_and_orphans() {
    let rig = Rig::new(
        "fabric_ec2",
        |v| v["fabric"]["orphanInstanceSeconds"] = json!(1),
        manual(),
    )
    .await;
    let (r, g) = rig.replica("hermit-a").await;
    let a = one_host(&rig, &g, &r).await;
    rig.ec2.extra.lock().unwrap().push(Instance {
        id: "i-orphan".into(),
        state: "running".into(),
        private_ip: None,
        launched_ms: now_ms() - 5000,
    });
    rig.ec2.extra.lock().unwrap().push(Instance {
        id: "i-young".into(),
        state: "running".into(),
        private_ip: None,
        launched_ms: now_ms() + 60_000,
    });
    pass(&g, &r).await;
    assert_eq!(rig.ec2.terminated(), vec!["i-orphan".to_owned()]);
    // the private address is filled in
    let ip: Option<IpAddr> = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select private_ip from hosts where name = $1", &[&a])
        .await
        .unwrap()
        .get(0);
    assert!(ip.is_some());
    // an orphan sandbox name (no row), older than 60 s
    rig.fleet.sim(&a).sandboxes.lock().unwrap().insert(
        "sbx-0rphan".into(),
        OnHost {
            spec: Spec {
                name: "sbx-0rphan".into(),
                ip: "198.18.9.9".into(),
                image: "x".into(),
                image_ref: None,
                env: sandboxd_api::Env::default(),
                cpu: 1.0,
                memory: GIB,
                cpu_features: String::new(),
                workspace: None,
                pids: None,
                scratch_bytes: None,
            },
            state: HostState::Running,
            alive: Some(true),
            sending: false,
            scratch_used: Some(0),
            created_at: "2026-01-01T00:00:00.000Z".into(),
        },
    );
    let p = pass(&g, &r).await;
    assert_eq!(p.orphans, vec!["sbx-0rphan".to_owned()]);
    assert!(rig.fleet.sim(&a).calls().contains(&"delete sbx-0rphan".to_owned()));
    // the instance disappears: lost (after 2 min of the row's life)
    rig.ec2.terminated.lock().unwrap().push("i-0001".into());
    rig.sql(&format!(
        "update hosts set created_at = now() - interval '3 minutes' where name = '{a}'"
    ))
    .await;
    let p = pass(&g, &r).await;
    assert!(p.lost_hosts.contains(&a), "{p:?}");
    // a launch that fails: the row gone with the code, event launch-failed
    *rig.ec2.fail.lock().unwrap() = Some("VcpuLimitExceeded".into());
    demand(&rig, "a@example.org").await;
    pass(&g, &r).await;
    let failed = rig.events("launch-failed").await;
    assert_eq!(failed[0]["code"], "VcpuLimitExceeded");
    let n: i64 = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from hosts where kind = 'aws' and status = 'gone' and gone_reason like 'launch failed: VcpuLimitExceeded%'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}

/// hermit 13.8: no ready egress endpoint keeps the gateway's last targets; a new list replaces them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn egress_targets_follow_the_ready_endpoints() {
    let rig = Rig::new("fabric_egress", |_| {}, manual()).await;
    let (r, g) = rig.replica("hermit-a").await;
    pass(&g, &r).await;
    let gw = rig.fleet.sim(GATEWAY);
    assert_eq!(*gw.egress.lock().unwrap(), Some(vec!["10.244.1.5".to_owned()]));
    rig.egress.set(Some(vec![]));
    pass(&g, &r).await;
    assert_eq!(
        *gw.egress.lock().unwrap(),
        Some(vec!["10.244.1.5".to_owned()]),
        "the last list is kept"
    );
    rig.egress.set(Some(vec!["10.244.2.6".into(), "10.244.3.7".into()]));
    pass(&g, &r).await;
    assert_eq!(
        *gw.egress.lock().unwrap(),
        Some(vec!["10.244.2.6".to_owned(), "10.244.3.7".to_owned()])
    );
    // the ops page's part (18.3, 18.6): the plan, the last pass, the gateway's egress targets, slots per host
    let a = one_host(&rig, &g, &r).await;
    let ops = g.ops().await.unwrap();
    let plan = ops.plan.unwrap();
    assert_eq!((plan.per_host, plan.available, plan.need), (7, 7, 0));
    assert!(ops.last_pass_at.is_some());
    assert_eq!(ops.egress_targets, Some(2));
    assert_eq!(ops.slots.get(&a), Some(&7));
    assert_eq!(ops.slots.get(GATEWAY), Some(&0));
}

/// hermit 13.10 "Takeover": a replica that dies mid-migration leaves its claim to lapse; the replica that takes it
/// over asks the target where the receive stands and finishes the move (the sandbox is adopted on the target,
/// never declared vanished from its source, and the target's copy is never swept as an orphan).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_migration_resumes_after_a_replica_dies() {
    let rig = Rig::new("fabric_takeover", |_| {}, manual()).await;
    let (r1, g1) = rig.replica("hermit-a").await;
    // two hosts with one sandbox each
    let a = one_host(&rig, &g1, &r1).await;
    rig.sql(&format!("update hosts set max_sandboxes = 1 where name = '{a}'"))
        .await;
    let (_, _) = rig.ready_on(&a, "198.18.0.60").await;
    demand(&rig, "b@example.org").await;
    pass(&g1, &r1).await;
    pass(&g1, &r1).await;
    rig.sql("delete from demand_hints").await;
    let b = rig.aws(1);
    let (_, _) = rig.ready_on(&b, "198.18.0.61").await;
    rig.sql(&format!("update hosts set max_sandboxes = null where name = '{a}'"))
        .await;
    // the send is held on whichever host packing picks as the source
    for h in [&a, &b] {
        rig.fleet.sim(h).hold_send.store(true, Ordering::SeqCst);
    }
    let p = pass(&g1, &r1).await;
    let (id, from, to) = p.migration.expect("a packing move");
    let src = rig.fleet.sim(&from);
    assert!(eventually(|| async { src.sends_waiting.load(Ordering::SeqCst) == 1 }).await);
    // the replica dies while sandboxd sends
    r1.kill();
    let r2 = rig.replica("hermit-b").await;
    // sandboxd finishes on its own while nobody watches
    src.release_send();
    // the new owner's sweep takes the lapsed claim and adopts the sandbox on the target
    assert!(eventually(|| async { rig.host_of(id).await.as_deref() == Some(to.as_str()) }).await);
    let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
    assert_eq!(
        (row.status, row.migrating_to, row.task),
        (SandboxState::Ready, None, None)
    );
    assert!(
        rig.fleet
            .sim(&to)
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("result {} 0", row.name))),
        "asked the target without a wait"
    );
    let kinds = [rig.events("migration").await, rig.events("migration-adopted").await].concat();
    assert_eq!(kinds.len(), 1);
    // the scaler on the new replica keeps it (listed by its new host) and does not sweep it
    let p = pass(&r2.1, &r2.0).await;
    assert!(p.vanished.is_empty() && p.orphans.is_empty(), "{p:?}");
    assert_eq!(
        db::load(&rig.h.pool(), id).await.unwrap().unwrap().status,
        SandboxState::Ready
    );
    let _ = from;
}

/// WP-19 review: a send whose answer is lost after the source's checkpoint stopped the sandbox, with the target's
/// receive still in progress when hermit's wait ends. The source lists it `alive: false` (sandboxd marks a source
/// stopped at once, not at its next 10 s sample), so the settle neither keeps it on the source (which would delete
/// the target's copy, the only one) nor declares it lost while the receive runs: it waits, then adopts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_source_never_settles_as_kept() {
    let rig = Rig::new("fabric_stopped_source", |_| {}, manual()).await;
    let (r1, g1) = rig.replica("hermit-a").await;
    let a = one_host(&rig, &g1, &r1).await;
    rig.sql(&format!("update hosts set max_sandboxes = 1 where name = '{a}'"))
        .await;
    let (_, _) = rig.ready_on(&a, "198.18.0.70").await;
    demand(&rig, "b@example.org").await;
    pass(&g1, &r1).await;
    pass(&g1, &r1).await;
    rig.sql("delete from demand_hints").await;
    let b = rig.aws(1);
    let (_, _) = rig.ready_on(&b, "198.18.0.71").await;
    rig.sql(&format!("update hosts set max_sandboxes = null where name = '{a}'"))
        .await;
    for h in [&a, &b] {
        rig.fleet.sim(h).stop_and_lose_answer.store(true, Ordering::SeqCst);
    }
    let p = pass(&g1, &r1).await;
    let (id, from, to) = p.migration.expect("a packing move");
    let name = db::load(&rig.h.pool(), id).await.unwrap().unwrap().name;
    let (src, dst) = (rig.fleet.sim(&from), rig.fleet.sim(&to));
    // hermit's wait on the receive (3 s in the fake) ends, then the settle asks both hosts for a while
    assert!(eventually(|| async { src.calls().iter().any(|c| c.starts_with("send ")) }).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
    assert_eq!(
        (row.status, row.host.as_deref(), row.migrating_to.as_deref()),
        (SandboxState::Ready, Some(from.as_str()), Some(to.as_str())),
        "undecided while the receive runs"
    );
    assert!(
        dst.sandboxes.lock().unwrap().get(&name).map(|s| s.state) == Some(HostState::Receiving),
        "the target's copy is kept"
    );
    // the receive finishes: adopted on the target
    rig.finish_move(&name, &from, &to);
    assert!(eventually(|| async { rig.host_of(id).await.as_deref() == Some(to.as_str()) }).await);
    let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
    assert_eq!((row.status, row.migrating_to), (SandboxState::Ready, None));
    assert!(
        !dst.calls().iter().any(|c| c == &format!("delete {name}")),
        "{:?}",
        dst.calls()
    );
    assert_eq!(rig.events("migration-adopted").await.len(), 1);
    assert_eq!(rig.events("migration-failed").await, Vec::<serde_json::Value>::new());
}

/// Two sandboxes on host a, one on a fuller host b with room for a's: the setup of a packing move a -> b (the
/// stopped-source test's), with every send of either host set up by `arm`. Returns the rig's replica and gvisor.
async fn packing_pair(rig: &Rig, arm: impl Fn(&HostSim)) -> (Replica, Arc<Gvisor>) {
    let (r1, g1) = rig.replica("hermit-a").await;
    let a = one_host(rig, &g1, &r1).await;
    rig.sql(&format!("update hosts set max_sandboxes = 1 where name = '{a}'"))
        .await;
    let (_, _) = rig.ready_on(&a, "198.18.0.80").await;
    demand(rig, "b@example.org").await;
    pass(&g1, &r1).await;
    pass(&g1, &r1).await;
    rig.sql("delete from demand_hints").await;
    let b = rig.aws(1);
    let (_, _) = rig.ready_on(&b, "198.18.0.81").await;
    rig.sql(&format!("update hosts set max_sandboxes = null where name = '{a}'"))
        .await;
    for h in [&a, &b] {
        arm(&rig.fleet.sim(h));
    }
    (r1, g1)
}

/// WP-19 review: a send that keeps running on the source after hermit's call failed (its answer lost) and its wait
/// on the receive ended. The source lists the sandbox alive but `sending`, so the settle neither keeps it there
/// (which would delete the target's receive while the send may still switch to it) nor declares it lost: it waits.
/// The send then ends with the sandbox still on its source (not converged): kept, and the receive deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_source_still_sending_is_not_kept_until_its_send_ends() {
    let rig = Rig::new("fabric_still_sending", |_| {}, manual()).await;
    let (r1, g1) = packing_pair(&rig, |h| h.lose_answer_keep_sending.store(true, Ordering::SeqCst)).await;
    let p = pass(&g1, &r1).await;
    let (id, from, to) = p.migration.expect("a packing move");
    let name = db::load(&rig.h.pool(), id).await.unwrap().unwrap().name;
    let (src, dst) = (rig.fleet.sim(&from), rig.fleet.sim(&to));
    assert!(eventually(|| async { src.calls().iter().any(|c| c.starts_with("send ")) }).await);
    // hermit's wait on the receive (3 s in the fake) ends; the settle asks both hosts for a while
    tokio::time::sleep(Duration::from_secs(6)).await;
    let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
    assert_eq!(
        (row.status, row.host.as_deref(), row.migrating_to.as_deref()),
        (SandboxState::Ready, Some(from.as_str()), Some(to.as_str())),
        "undecided while the send runs"
    );
    assert!(
        !dst.calls().iter().any(|c| c == &format!("delete {name}")),
        "the target's receive is kept while the source sends: {:?}",
        dst.calls()
    );
    // the send ends without the switch: the sandbox still runs on its source
    src.sandboxes.lock().unwrap().get_mut(&name).unwrap().sending = false;
    assert!(
        eventually(|| async {
            db::load(&rig.h.pool(), id)
                .await
                .unwrap()
                .unwrap()
                .migrating_to
                .is_none()
        })
        .await
    );
    let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
    assert_eq!(
        (row.status, row.host.as_deref()),
        (SandboxState::Ready, Some(from.as_str()))
    );
    assert!(dst.calls().iter().any(|c| c == &format!("delete {name}")));
    let failed = rig.events("migration-failed").await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["kept"], json!(true));
}

/// WP-19 review: the same lost answer, but the send then switches and hands over: adopted on the target, whose
/// receive was never deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_source_still_sending_is_adopted_when_its_send_switches() {
    let rig = Rig::new("fabric_sending_adopt", |_| {}, manual()).await;
    let (r1, g1) = packing_pair(&rig, |h| h.lose_answer_keep_sending.store(true, Ordering::SeqCst)).await;
    let p = pass(&g1, &r1).await;
    let (id, from, to) = p.migration.expect("a packing move");
    let name = db::load(&rig.h.pool(), id).await.unwrap().unwrap().name;
    let (src, dst) = (rig.fleet.sim(&from), rig.fleet.sim(&to));
    assert!(eventually(|| async { src.calls().iter().any(|c| c.starts_with("send ")) }).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    // the checkpoint stops it (alive:false, still sending), then the hand-over
    src.sandboxes.lock().unwrap().get_mut(&name).unwrap().alive = Some(false);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(rig.host_of(id).await.as_deref(), Some(from.as_str()));
    rig.finish_move(&name, &from, &to);
    assert!(eventually(|| async { rig.host_of(id).await.as_deref() == Some(to.as_str()) }).await);
    assert!(
        !dst.calls().iter().any(|c| c == &format!("delete {name}")),
        "{:?}",
        dst.calls()
    );
    assert_eq!(rig.events("migration-adopted").await.len(), 1);
    assert_eq!(rig.events("migration-failed").await, Vec::<serde_json::Value>::new());
}

/// WP-19 review: a target that lists the receive `receiving` forever (a sandboxd that does not bound it) and a
/// source its checkpoint stopped. Past `migration_limit` from `migration_started_at` the settle deletes the receive
/// and ends the sandbox lost (`migration_failed` / `sandbox_lost`), so the conversation is not left on a row whose
/// sandbox no longer runs anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receive_that_never_ends_is_ended_at_the_migration_limit() {
    let rig = Rig::new(
        "fabric_receive_forever",
        |_| {},
        Timings {
            migration_limit: Duration::from_secs(8),
            ..manual()
        },
    )
    .await;
    let (r1, g1) = packing_pair(&rig, |h| h.stop_and_lose_answer.store(true, Ordering::SeqCst)).await;
    let p = pass(&g1, &r1).await;
    let (id, _from, to) = p.migration.expect("a packing move");
    let name = db::load(&rig.h.pool(), id).await.unwrap().unwrap().name;
    let dst = rig.fleet.sim(&to);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        db::load(&rig.h.pool(), id).await.unwrap().unwrap().status,
        SandboxState::Ready,
        "waits while the receive runs, within the limit"
    );
    assert!(
        eventually(|| async { db::load(&rig.h.pool(), id).await.unwrap().unwrap().status == SandboxState::Gone }).await
    );
    let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
    assert_eq!(row.release_code.as_deref(), Some("sandbox_lost"), "{row:?}");
    assert!(
        dst.calls().iter().any(|c| c == &format!("delete {name}")),
        "{:?}",
        dst.calls()
    );
    let failed = rig.events("migration-failed").await;
    assert_eq!(failed.len(), 1);
    assert_eq!(
        (&failed[0]["kept"], &failed[0]["overdue"]),
        (&json!(false), &json!(true))
    );
}

/// Past the migration limit, a target whose delete of the receive does not land (500) is asked to delete it again
/// while it still lists the receive; the sandbox then ends lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receive_whose_delete_fails_is_deleted_again() {
    let rig = Rig::new(
        "fabric_receive_delete_fails",
        |_| {},
        Timings {
            migration_limit: Duration::from_secs(4),
            ..manual()
        },
    )
    .await;
    let (r1, g1) = packing_pair(&rig, |h| h.stop_and_lose_answer.store(true, Ordering::SeqCst)).await;
    let p = pass(&g1, &r1).await;
    let (id, _from, to) = p.migration.expect("a packing move");
    let name = db::load(&rig.h.pool(), id).await.unwrap().unwrap().name;
    let dst = rig.fleet.sim(&to);
    // the migration started under a second ago; its limit is 4 s
    dst.fail_deletes.store(1, Ordering::SeqCst);
    assert!(
        eventually(|| async { db::load(&rig.h.pool(), id).await.unwrap().unwrap().status == SandboxState::Gone }).await
    );
    let deletes = dst.calls().iter().filter(|c| *c == &format!("delete {name}")).count();
    assert!(deletes >= 2, "{:?}", dst.calls());
    let failed = rig.events("migration-failed").await;
    assert_eq!(failed.len(), 1);
    assert_eq!(
        (&failed[0]["kept"], &failed[0]["overdue"]),
        (&json!(false), &json!(true))
    );
}

/// WP-19 review: past the migration limit with the source still sending (alive), a target whose delete of the
/// receive does not land keeps the migration open: the receive is deleted again until the target lists none, and only
/// then is the sandbox kept on its source (the send can then no longer switch to a receive).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn past_the_limit_a_sending_source_is_kept_only_once_the_receive_is_gone() {
    let rig = Rig::new(
        "fabric_sending_overdue",
        |_| {},
        Timings {
            migration_limit: Duration::from_secs(4),
            ..manual()
        },
    )
    .await;
    let (r1, g1) = packing_pair(&rig, |h| h.lose_answer_keep_sending.store(true, Ordering::SeqCst)).await;
    let p = pass(&g1, &r1).await;
    let (id, from, to) = p.migration.expect("a packing move");
    let name = db::load(&rig.h.pool(), id).await.unwrap().unwrap().name;
    let dst = rig.fleet.sim(&to);
    // two deletes fail: the one at the limit and the first of the kept branch
    dst.fail_deletes.store(2, Ordering::SeqCst);
    assert!(
        eventually(|| async {
            db::load(&rig.h.pool(), id)
                .await
                .unwrap()
                .unwrap()
                .migrating_to
                .is_none()
        })
        .await
    );
    let row = db::load(&rig.h.pool(), id).await.unwrap().unwrap();
    assert_eq!(
        (row.status, row.host.as_deref()),
        (SandboxState::Ready, Some(from.as_str()))
    );
    let deletes = dst.calls().iter().filter(|c| *c == &format!("delete {name}")).count();
    assert!(deletes >= 3, "{:?}", dst.calls());
    assert!(
        !dst.sandboxes.lock().unwrap().contains_key(&name),
        "no receive left on the target when the sandbox is kept"
    );
    let failed = rig.events("migration-failed").await;
    assert_eq!(failed.len(), 1);
    assert_eq!(
        (&failed[0]["kept"], &failed[0]["overdue"]),
        (&json!(true), &json!(true))
    );
}

/// hermit 13.10 `movable`: a sandbox with more than `maxScratchMib` in use on its scratch, or whose figure its source
/// does not report, is not moved (its checksummed final delta would stop it for seconds); under the bound it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_scratch_is_not_moved() {
    let rig = Rig::new("fabric_full_scratch", |_| {}, manual()).await;
    let (r1, g1) = packing_pair(&rig, |_| {}).await;
    let set = |mib: u64| {
        for h in [rig.aws(0), rig.aws(1)] {
            for s in rig.fleet.sim(&h).sandboxes.lock().unwrap().values_mut() {
                s.scratch_used = Some(mib << 20);
            }
        }
    };
    set(600);
    let p = pass(&g1, &r1).await;
    assert!(p.migration.is_none(), "{p:?}");
    // a figure the source does not report (an older sandboxd, a scratch not mounted) is not under the bound
    for h in [rig.aws(0), rig.aws(1)] {
        for s in rig.fleet.sim(&h).sandboxes.lock().unwrap().values_mut() {
            s.scratch_used = None;
        }
    }
    let p = pass(&g1, &r1).await;
    assert!(p.migration.is_none(), "{p:?}");
    set(100);
    let p = pass(&g1, &r1).await;
    assert!(p.migration.is_some(), "{p:?}");
}

/// A start through the whole core on this fabric: `PUT` -> placed on the AWS host, created with the spec hermit
/// 13.2 asks for (the image's digest bare, `pids`, the workspace, no `user`), the supervisor's control address, the
/// gateway peers; a release takes the console tail and deletes it on its host.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_and_release_through_the_core() {
    let rig = Rig::new(
        "fabric_start",
        |_| {},
        Timings {
            scaler: Duration::from_millis(200),
            ..manual()
        },
    )
    .await;
    let (r, _g) = rig.replica("hermit-a").await;
    assert!(eventually(|| async { rig.host_status(GATEWAY).await == "active" }).await);
    let cid = Uuid::now_v7();
    let path = routes::fill(routes::SANDBOX, &[&cid.to_string()]);
    let a = rig
        .h
        .client(Caller::Magpie)
        .call(&r.url, "PUT", &path, Some(json!({"person": STAFF})), &[])
        .await;
    assert!(matches!(a.status, 200 | 202), "{}: {}", a.status, a.body);
    // no host: the start asks for one (phase host), the launch comes up, the start lands there
    assert!(
        eventually(|| async {
            db::latest_of(&rig.h.pool(), cid)
                .await
                .unwrap()
                .is_some_and(|r| r.status == SandboxState::Ready)
        })
        .await,
        "{:?}",
        db::latest_of(&rig.h.pool(), cid).await.unwrap()
    );
    let row = db::latest_of(&rig.h.pool(), cid).await.unwrap().unwrap();
    let host = row.host.clone().unwrap();
    assert_eq!(host, rig.aws(0));
    let spec = rig
        .fleet
        .sim(&host)
        .sandboxes
        .lock()
        .unwrap()
        .get(&row.name)
        .unwrap()
        .spec
        .clone();
    assert_eq!(spec.image, rig.h.config.image_digest.trim_start_matches("sha256:"));
    assert_eq!(spec.pids, Some(4096));
    // `sandbox.tmpGiB` 5 is sandboxd's default: not named, so an older sandboxd takes the spec
    assert_eq!(spec.scratch_bytes, None);
    assert_eq!(spec.workspace, Some(cid.to_string()));
    assert_eq!(spec.cpu_features, "sse4_2,avx2");
    assert!(
        spec.env
            .0
            .iter()
            .any(|(k, v)| k == "HTTPS_PROXY" && v.contains("@198.19.1.1:3128"))
    );
    let peers = rig.fleet.sim(GATEWAY).peers.lock().unwrap().clone().unwrap();
    assert!(
        peers
            .iter()
            .any(|p| p.allowed_ips.contains(&format!("{}/32", row.ip.unwrap())))
    );
    // an earlier destroy whose host did not answer for the tail left a row without bytes: the next capture
    // fetches again (only a stored tail or a lost host is final)
    rig.h
        .pool()
        .get()
        .await
        .unwrap()
        .execute(
            "insert into sandbox_logs (sandbox_id, bytes, reason) values ($1, null, 'tail_unavailable')",
            &[&row.id],
        )
        .await
        .unwrap();
    // the conversation's delete: forced release, console tail kept, deleted on its host
    let rel = routes::fill(routes::SANDBOX_RELEASE, &[&cid.to_string()]);
    let a = rig
        .h
        .client(Caller::Magpie)
        .call(&r.url, "POST", &rel, Some(json!({"reason": "deleted"})), &[])
        .await;
    assert!(a.status < 300, "{}: {}", a.status, a.body);
    assert!(
        eventually(|| async { db::load(&rig.h.pool(), row.id).await.unwrap().unwrap().status == SandboxState::Gone })
            .await
    );
    assert!(!rig.fleet.sim(&host).running(&row.name));
    let tail: Vec<u8> = rig
        .h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select bytes from sandbox_logs where sandbox_id = $1", &[&row.id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        tail,
        format!("console of {}\n", row.name).into_bytes(),
        "a tail_unavailable row is fetched again"
    );
}

/// `sandbox.tmpGiB` other than sandboxd's 5 GiB reaches the host as the spec's `scratchBytes`: the sandbox's scratch
/// filesystem is that size, not the text of the rules only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_scratch_size_reaches_the_host() {
    let rig = Rig::new(
        "fabric_scratch",
        |v| v["sandbox"]["tmpGiB"] = json!(100),
        Timings {
            scaler: Duration::from_millis(200),
            ..manual()
        },
    )
    .await;
    let (r, _g) = rig.replica("hermit-a").await;
    assert!(eventually(|| async { rig.host_status(GATEWAY).await == "active" }).await);
    let cid = Uuid::now_v7();
    let path = routes::fill(routes::SANDBOX, &[&cid.to_string()]);
    let a = rig
        .h
        .client(Caller::Magpie)
        .call(&r.url, "PUT", &path, Some(json!({"person": STAFF})), &[])
        .await;
    assert!(matches!(a.status, 200 | 202), "{}: {}", a.status, a.body);
    assert!(
        eventually(|| async {
            db::latest_of(&rig.h.pool(), cid)
                .await
                .unwrap()
                .is_some_and(|r| r.status == SandboxState::Ready)
        })
        .await,
        "{:?}",
        db::latest_of(&rig.h.pool(), cid).await.unwrap()
    );
    let row = db::latest_of(&rig.h.pool(), cid).await.unwrap().unwrap();
    let spec = rig
        .fleet
        .sim(&row.host.clone().unwrap())
        .sandboxes
        .lock()
        .unwrap()
        .get(&row.name)
        .unwrap()
        .spec
        .clone();
    assert_eq!(spec.scratch_bytes, Some(100 << 30));
}
