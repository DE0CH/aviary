//! fuse-gate: the FUSE gate spike (crates/supervisor/GATE.md). One static binary, four roles:
//!   fuse-gate keeper SOCK
//!   fuse-gate magpie LISTEN DATA_DIR CTL_DIR
//!   fuse-gate model LISTEN LOG
//!   fuse-gate serve MOUNT MAGPIE STATE_DIR JOURNAL KEEPER_SOCK [--workers N] [--die-at POINT:OPCODE:NTH]
//!     POINT = read | sent | answered | replied; OPCODE = WRITE, READ, ...

mod fuse;
mod http;
mod keeper;
mod magpie;
mod model;

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).cloned().unwrap_or_else(|| usage());
    let r = match a.get(1).map(String::as_str) {
        Some("keeper") => keeper::run(&arg(2)),
        Some("magpie") => magpie::run(&arg(2), PathBuf::from(arg(3)), PathBuf::from(arg(4))),
        Some("model") => model::run(&arg(2), &arg(3)),
        Some("serve") => {
            let mut workers = 16;
            let mut die = None;
            let mut i = 7;
            while i < a.len() {
                match a[i].as_str() {
                    "--workers" => workers = arg(i + 1).parse().unwrap_or(16),
                    "--die-at" => {
                        let s = arg(i + 1);
                        let p: Vec<&str> = s.split(':').collect();
                        die = Some(fuse::Die {
                            point: p[0].to_string(),
                            opcode: fuse::opcode_of(p.get(1).copied().unwrap_or("WRITE")),
                            nth: p.get(2).and_then(|n| n.parse().ok()).unwrap_or(1),
                            seen: AtomicU64::new(0),
                        });
                    }
                    _ => usage(),
                }
                i += 2;
            }
            fuse::serve(fuse::Opts {
                mount: arg(2),
                magpie: arg(3),
                state: PathBuf::from(arg(4)),
                journal: PathBuf::from(arg(5)),
                keeper: arg(6),
                workers,
                die,
            })
        }
        _ => usage(),
    };
    if let Err(e) = r {
        eprintln!("fuse-gate: {e}");
        std::process::exit(1);
    }
}

fn usage() -> ! {
    eprintln!("usage: see src/main.rs");
    std::process::exit(2)
}
