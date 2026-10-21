//! The global allocator: fixed arenas, never the OS (docs/SANDBOXD-MEMORY.md, "The heap").
//!
//! - Arena 0 ("boot") is a static array in .bss. Everything sandboxd allocates while it starts (the TLS
//!   configuration, thread handles) comes from here.
//! - Arenas 1..=n are slices of ONE region mapped at start (size from host.json `limits`), one per API worker
//!   thread. After start, the only code that allocates is rustls (inside `zone::tls`), on the worker's own arena; a
//!   connection's TLS state lives and dies on that thread, so the arena is empty again between connections (checked
//!   and reported in GET /v1/host `memory`).
//! - Nothing ever calls mmap/brk for the heap after start: an exhausted worker arena falls back to what is left of
//!   the boot arena (counted as `overflow`), and only if that is exhausted too does an allocation fail (null). The
//!   TLS layer checks the arena's headroom before each call into rustls and closes the connection instead
//!   (tls.rs `guard`), so this is not reached in practice; the soak test checks `overflow` stays 0.
//! - sandboxd's own code (HTTP, JSON, commands, state) never allocates after start: it works in fixed buffers.
//!   Allocations outside the TLS zone after start are counted (`stray`; the soak test requires 0).
use core::alloc::{GlobalAlloc, Layout};
use core::cell::{Cell, UnsafeCell};
use core::ptr::{NonNull, null_mut};
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use linked_list_allocator::Heap;
use std::sync::Mutex;

/// Upper limit for API workers (each has an arena); host.json `limits.maxConnections` must not exceed it.
pub const MAX_ARENAS: usize = 256;
/// The boot arena (static, .bss).
pub const BOOT_SIZE: usize = if cfg!(test) { 16 << 20 } else { 512 << 10 };

#[repr(C, align(4096))]
struct Boot(UnsafeCell<[u8; BOOT_SIZE]>);
unsafe impl Sync for Boot {}
static BOOT: Boot = Boot(UnsafeCell::new([0; BOOT_SIZE]));

struct Arena {
    heap: Mutex<Heap>,
    peak: AtomicUsize,
}

#[allow(clippy::declare_interior_mutable_const)]
const ARENA: Arena = Arena { heap: Mutex::new(Heap::empty()), peak: AtomicUsize::new(0) };
static ARENAS: [Arena; MAX_ARENAS + 1] = [ARENA; MAX_ARENAS + 1];

static REGION: AtomicUsize = AtomicUsize::new(0);
static ARENA_SIZE: AtomicUsize = AtomicUsize::new(0);
static ARENA_COUNT: AtomicUsize = AtomicUsize::new(0);
static STRAY: AtomicUsize = AtomicUsize::new(0);
static OVERFLOW: AtomicUsize = AtomicUsize::new(0);
static FAILED: AtomicUsize = AtomicUsize::new(0);

/// What a thread is doing, for the allocation audit.
pub mod zone {
    /// starting up: allocations are expected
    pub const INIT: u8 = 0;
    /// sandboxd's own code after start: no allocations expected (counted as stray)
    pub const APP: u8 = 1;
    /// inside rustls: allocations expected, on the thread's arena
    pub const TLS: u8 = 2;
}

thread_local! {
    static CUR: Cell<usize> = const { Cell::new(0) };
    static ZONE: Cell<u8> = const { Cell::new(zone::INIT) };
}

/// This thread allocates from arena `i` (0 = boot) from now on.
pub fn use_arena(i: usize) {
    CUR.with(|c| c.set(i));
}

pub fn set_zone(z: u8) -> u8 {
    ZONE.with(|c| c.replace(z))
}

/// Runs `f` in the TLS zone (rustls may allocate on this thread's arena).
#[inline]
pub fn in_tls<T>(f: impl FnOnce() -> T) -> T {
    let prev = set_zone(zone::TLS);
    let r = f();
    set_zone(prev);
    r
}

fn lock(i: usize) -> std::sync::MutexGuard<'static, Heap> {
    // nothing panics while holding an arena lock, so it is never poisoned; tolerate it anyway
    match ARENAS[i].heap.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn boot_heap() -> std::sync::MutexGuard<'static, Heap> {
    let mut h = lock(0);
    if h.size() == 0 {
        // SAFETY: BOOT is only ever handed to this heap, once (size() == 0 only before init).
        unsafe { h.init(BOOT.0.get() as *mut u8, BOOT_SIZE) };
    }
    h
}

/// Maps the worker arenas: `count` arenas of `size` bytes in one region (called once, at start).
/// # Safety
/// `region` must point to `count * size` bytes that stay mapped and unused by anything else forever.
pub unsafe fn init_arenas(region: *mut u8, count: usize, size: usize) {
    assert!(count <= MAX_ARENAS && ARENA_COUNT.load(Relaxed) == 0);
    for i in 0..count {
        let mut h = lock(i + 1);
        unsafe { h.init(region.add(i * size), size) };
    }
    REGION.store(region as usize, Relaxed);
    ARENA_SIZE.store(size, Relaxed);
    ARENA_COUNT.store(count, Relaxed);
}

fn owner(ptr: *mut u8) -> usize {
    let p = ptr as usize;
    let boot = BOOT.0.get() as usize;
    if p >= boot && p < boot + BOOT_SIZE {
        return 0;
    }
    let r = REGION.load(Relaxed);
    let sz = ARENA_SIZE.load(Relaxed);
    if sz == 0 || p < r {
        return usize::MAX;
    }
    let i = (p - r) / sz;
    if i < ARENA_COUNT.load(Relaxed) { i + 1 } else { usize::MAX }
}

fn try_alloc(i: usize, layout: Layout) -> *mut u8 {
    let mut h = if i == 0 { boot_heap() } else { lock(i) };
    match h.allocate_first_fit(layout) {
        Ok(p) => {
            let used = h.used();
            drop(h);
            ARENAS[i].peak.fetch_max(used, Relaxed);
            p.as_ptr()
        }
        Err(()) => null_mut(),
    }
}

pub struct ArenaAlloc;

unsafe impl GlobalAlloc for ArenaAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let i = CUR.with(|c| c.get());
        if ZONE.with(|z| z.get()) == zone::APP {
            STRAY.fetch_add(1, Relaxed);
            // `--features audit`: stop right there, for a debugger's backtrace (tests/soak.py --audit)
            #[cfg(feature = "audit")]
            unsafe {
                libc::abort()
            };
        }
        let p = try_alloc(i, layout);
        if !p.is_null() || i == 0 {
            if p.is_null() {
                FAILED.fetch_add(1, Relaxed);
            }
            return p;
        }
        OVERFLOW.fetch_add(1, Relaxed);
        let p = try_alloc(0, layout);
        if p.is_null() {
            FAILED.fetch_add(1, Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let i = owner(ptr);
        if i == usize::MAX {
            return; // not ours (cannot happen: every allocation comes from an arena)
        }
        let Some(p) = NonNull::new(ptr) else { return };
        unsafe { lock(i).deallocate(p, layout) };
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub boot_size: usize,
    pub boot_used: usize,
    pub boot_peak: usize,
    pub arenas: usize,
    pub arena_size: usize,
    /// sum of the worker arenas' bytes in use now
    pub arena_used: usize,
    /// the largest any single worker arena has been
    pub arena_peak: usize,
    pub stray: usize,
    pub overflow: usize,
    pub failed: usize,
}

pub fn stats() -> Stats {
    let n = ARENA_COUNT.load(Relaxed);
    let mut s = Stats {
        boot_size: BOOT_SIZE,
        boot_used: boot_heap().used(),
        boot_peak: ARENAS[0].peak.load(Relaxed),
        arenas: n,
        arena_size: ARENA_SIZE.load(Relaxed),
        stray: STRAY.load(Relaxed),
        overflow: OVERFLOW.load(Relaxed),
        failed: FAILED.load(Relaxed),
        ..Default::default()
    };
    for (i, a) in ARENAS.iter().enumerate().take(n + 1).skip(1) {
        s.arena_used += lock(i).used();
        s.arena_peak = s.arena_peak.max(a.peak.load(Relaxed));
    }
    s
}

/// Bytes in use on the current thread's arena, and its size.
pub fn current_usage() -> (usize, usize) {
    let i = CUR.with(|c| c.get());
    let h = if i == 0 { boot_heap() } else { lock(i) };
    (h.used(), h.size())
}
