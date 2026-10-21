//! Stack high-water marks: each thread paints the unused part of its (fixed-size) stack at start; the deepest
//! overwritten byte is how much stack it has ever used. GET /v1/host reports the maximum per kind, the soak test
//! checks it against the stack sizes in the budget.
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

const PAINT: u8 = 0xA5;
const MAX: usize = 300;

struct T {
    lo: AtomicUsize,
    hi: AtomicUsize,
    kind: AtomicUsize,
}
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: T = T { lo: AtomicUsize::new(0), hi: AtomicUsize::new(0), kind: AtomicUsize::new(0) };
static THREADS: [T; MAX] = [ZERO; MAX];
static N: AtomicUsize = AtomicUsize::new(0);

pub const API: usize = 1;
pub const OTHER: usize = 2;

/// Paints this thread's stack below the current frame (call first thing in a thread).
#[inline(never)]
pub fn paint(kind: usize) {
    unsafe {
        let mut attr: libc::pthread_attr_t = core::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return;
        }
        let mut addr: *mut libc::c_void = core::ptr::null_mut();
        let mut size: usize = 0;
        libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        let lo = addr as usize + 8192; // above the guard page(s)
        let hi = addr as usize + size;
        let here = &size as *const usize as usize;
        let mut p = lo;
        while p < here - 4096 {
            core::ptr::write_volatile(p as *mut u8, PAINT);
            p += 1;
        }
        let i = N.fetch_add(1, Relaxed);
        if i < MAX {
            THREADS[i].lo.store(lo, Relaxed);
            THREADS[i].hi.store(hi, Relaxed);
            THREADS[i].kind.store(kind, Relaxed);
        }
    }
}

/// The most stack any thread of `kind` has used so far (bytes).
pub fn peak(kind: usize) -> usize {
    let mut best = 0;
    for t in THREADS.iter().take(N.load(Relaxed).min(MAX)) {
        if t.kind.load(Relaxed) != kind {
            continue;
        }
        let (lo, hi) = (t.lo.load(Relaxed), t.hi.load(Relaxed));
        let mut p = lo;
        // SAFETY: [lo, hi) is a live thread's stack (threads never exit); bytes are read, racily but harmlessly.
        while p < hi && unsafe { core::ptr::read_volatile(p as *const u8) } == PAINT {
            p += 1;
        }
        best = best.max(hi - p);
    }
    best
}
