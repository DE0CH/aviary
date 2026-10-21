//! sandboxd's memory budget, computed from host.json `limits` (docs/SANDBOXD-MEMORY.md). The same numbers size the
//! region at start, the cgroup's memory.max/memory.min, and `sandboxd budget` prints them.
use crate::alloc::BOOT_SIZE;
use crate::config::Limits;
use crate::fixed::region_bytes;
use crate::sandboxes::{Mig, Sandboxes, Slot};

/// Request head buffer per API worker.
pub const HEAD: usize = 16 << 10;
/// Command output / file scratch per worker (runsc list, image config, bundle, state files).
pub const BIG: usize = 256 << 10;
/// Second scratch per worker (wg dump, runsc state, command output).
pub const SMALL: usize = 64 << 10;
/// The image server's copy buffer per connection.
pub const IMAGE_BUF: usize = 64 << 10;
/// Migration slot threads: scratch for the restore's stats and the state file.
pub const MIG_BIG: usize = 64 << 10;
pub const MIG_SMALL: usize = 16 << 10;

/// Thread stacks (fixed at spawn; mlockall makes them resident, so they count in full).
pub const API_STACK: usize = (512 << 10) * DEBUG;
pub const OTHER_STACK: usize = (256 << 10) * DEBUG;
/// debug builds have much bigger frames
const DEBUG: usize = if cfg!(debug_assertions) { 4 } else { 1 };
/// The start-up thread, which then runs the background jobs.
pub const START_STACK: usize = (512 << 10) * DEBUG;
/// The main thread only spawns the others and waits (its stack never grows past what start-up touched).
pub const MAIN_STACK: usize = 256 << 10;
/// The static binary (text + data), measured: `size` of the release build, rounded up.
pub const BINARY: usize = 4 << 20;
/// Kernel memory charged to sandboxd's cgroup besides socket buffers: page tables, kernel stacks (16 KiB per
/// thread), file and pipe objects, the page cache of the small files it writes (reclaimable).
pub const KERNEL_FIXED: usize = 8 << 20;

#[derive(Clone, Copy, Debug, Default)]
pub struct Budget {
    pub binary: usize,
    pub boot_arena: usize,
    pub tls_arenas: usize,
    pub worker_buffers: usize,
    pub image_buffers: usize,
    pub background_buffers: usize,
    pub tables: usize,
    pub stacks: usize,
    pub sockets: usize,
    pub kernel: usize,
    pub threads: usize,
}

pub fn out_size(l: &Limits) -> usize {
    (64 << 10) + l.max_sandboxes * 512
}

pub fn body_size(l: &Limits) -> usize {
    l.max_body + 2048
}

impl Budget {
    pub fn of(l: &Limits, hub: bool) -> Budget {
        let images = if hub { l.image_connections } else { 0 };
        // threads: API workers, image workers, migration slots, start-up/background, main
        let threads = l.max_connections + images + l.max_migrations + 2;
        Budget {
            binary: BINARY,
            boot_arena: BOOT_SIZE,
            tls_arenas: l.max_connections * l.tls_arena,
            worker_buffers: l.max_connections * (HEAD + body_size(l) + out_size(l) + BIG + SMALL),
            image_buffers: images * (IMAGE_BUF + HEAD),
            background_buffers: (BIG + SMALL) + l.max_migrations * (MIG_BIG + MIG_SMALL),
            tables: region_bytes::<Slot>(Sandboxes::slot_count(l)) + region_bytes::<Mig>(l.max_migrations),
            stacks: l.max_connections * API_STACK
                + (images + l.max_migrations) * OTHER_STACK
                + START_STACK
                + MAIN_STACK,
            sockets: (l.max_connections + images + 2) * crate::sock::SOCK_KERNEL_MAX,
            kernel: KERNEL_FIXED,
            threads,
        }
    }

    /// The region mapped at start (arenas + buffers + tables, page-aligned pieces).
    pub fn region(&self) -> usize {
        self.tls_arenas + self.worker_buffers + self.image_buffers + self.background_buffers + self.tables + 64 * 4096
    }

    /// Resident memory of the process can't exceed this (everything is mapped at start and locked).
    pub fn rss(&self) -> usize {
        self.binary + self.boot_arena + self.region() + self.stacks
    }

    /// What the cgroup may charge: RSS + kernel memory.
    pub fn total(&self) -> u64 {
        (self.rss() + self.sockets + self.kernel) as u64
    }

    pub fn write_table(&self, out: &mut impl core::fmt::Write) {
        let mib = |b: usize| b as f64 / (1 << 20) as f64;
        let _ = writeln!(out, "binary (text+data, static)     {:8.2} MiB", mib(self.binary));
        let _ = writeln!(out, "boot arena (.bss, start-up heap) {:6.2} MiB", mib(self.boot_arena));
        let _ = writeln!(out, "TLS arenas (per API worker)    {:8.2} MiB", mib(self.tls_arenas));
        let _ = writeln!(out, "API worker buffers             {:8.2} MiB", mib(self.worker_buffers));
        let _ = writeln!(out, "image server buffers           {:8.2} MiB", mib(self.image_buffers));
        let _ = writeln!(out, "background/migration buffers   {:8.2} MiB", mib(self.background_buffers));
        let _ = writeln!(out, "tables (sandboxes, locks, migs){:8.2} MiB", mib(self.tables));
        let _ = writeln!(out, "thread stacks ({} threads)     {:8.2} MiB", self.threads, mib(self.stacks));
        let _ = writeln!(out, "= resident (RSS) bound         {:8.2} MiB", mib(self.rss()));
        let _ = writeln!(out, "socket buffers (kernel)        {:8.2} MiB", mib(self.sockets));
        let _ = writeln!(out, "other kernel memory            {:8.2} MiB", mib(self.kernel));
        let _ = writeln!(out, "= cgroup bound (memory.max)    {:8.2} MiB", mib(self.total() as usize));
    }
}
