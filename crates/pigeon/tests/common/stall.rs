//! A terminating TCP proxy between pigeon and the fake old router, as bridge B1 is (DESIGN 15.5: the Hetzner load
//! balancer of the old bridge, then the old router's own relay to the child), whose far leg stalls like a partition:
//! while a connection is stalled the proxy keeps reading (and so acknowledging) what pigeon sends and holds it. When
//! it heals, every held byte is delivered to the old side, whether or not pigeon reset its connection meanwhile: a
//! proxy has already acknowledged those bytes, and pigeon's reset only ends pigeon's own leg. So a write that reaches
//! the old side late is refused there by its own not-after instant, or it is applied twice.
#![allow(dead_code, clippy::unwrap_used, clippy::missing_panics_doc)] // tests

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

#[derive(Default)]
struct Link {
    stalled: AtomicBool,
    healed: Notify,
}

impl Link {
    async fn wait_healed(&self) {
        loop {
            let n = self.healed.notified();
            if !self.stalled.load(Ordering::SeqCst) {
                return;
            }
            n.await;
        }
    }
}

/// The relay. Cheap to clone.
#[derive(Clone, Default)]
pub struct Stall {
    pub url: String,
    links: Arc<Mutex<Vec<Arc<Link>>>>,
    /// Connections pigeon reset.
    pub resets: Arc<AtomicUsize>,
    /// Bytes delivered to the old router after a heal (held while stalled).
    pub late: Arc<AtomicUsize>,
}

impl Stall {
    /// Relays to `target` (`http://host:port`).
    pub async fn start(target: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let me = Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            ..Self::default()
        };
        let to = target.trim_start_matches("http://").to_owned();
        let r = me.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let Ok(up) = TcpStream::connect(&to).await else {
                    continue;
                };
                let link = Arc::new(Link::default());
                r.links.lock().unwrap().push(link.clone());
                tokio::spawn(relay(r.clone(), link, client, up));
            }
        });
        me
    }

    /// Every connection open now stops delivering (new ones are not affected).
    pub fn stall(&self) {
        for l in self.links.lock().unwrap().iter() {
            l.stalled.store(true, Ordering::SeqCst);
        }
    }

    /// The partition heals.
    pub fn heal(&self) {
        for l in self.links.lock().unwrap().iter() {
            l.stalled.store(false, Ordering::SeqCst);
            l.healed.notify_waiters();
        }
    }
}

async fn relay(r: Stall, link: Arc<Link>, client: TcpStream, up: TcpStream) {
    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = up.into_split();
    let l2 = link.clone();
    // the old side's answers: held while stalled, then delivered unless pigeon has gone
    let back = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 << 10];
        loop {
            let n = match ur.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            l2.wait_healed().await;
            if cw.write_all(&buf[..n]).await.is_err() {
                return;
            }
        }
    });
    let mut held: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 16 << 10];
    let reset = loop {
        match cr.read(&mut buf).await {
            Ok(0) => break false,
            Ok(n) => {
                if link.stalled.load(Ordering::SeqCst) {
                    held.extend_from_slice(&buf[..n]);
                    continue;
                }
                if !held.is_empty() {
                    r.late.fetch_add(held.len(), Ordering::SeqCst);
                    if uw.write_all(&std::mem::take(&mut held)).await.is_err() {
                        break false;
                    }
                }
                if uw.write_all(&buf[..n]).await.is_err() {
                    break false;
                }
            }
            Err(e) => break e.kind() == std::io::ErrorKind::ConnectionReset,
        }
    };
    if reset {
        r.resets.fetch_add(1, Ordering::SeqCst);
    }
    // the proxy acknowledged what it holds: delivered once the far leg is back, a reset of pigeon's leg or not
    if !held.is_empty() {
        link.wait_healed().await;
        r.late.fetch_add(held.len(), Ordering::SeqCst);
        let _ = uw.write_all(&held).await;
    }
    let _ = uw.shutdown().await;
    let _ = back.await;
}
