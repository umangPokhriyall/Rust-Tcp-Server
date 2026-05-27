//! Signal handlers for the process models.
//!
//! Two installers: a `SIGCHLD` reaper (so forked children never become zombies)
//! and a `SIGINT`/`SIGTERM` shutdown flag. Each handler body is
//! async-signal-safe — it touches only `waitpid` and lock-free atomics, never
//! allocates, never takes a lock. The caller's `&'static` target is stashed in
//! an `AtomicPtr` so the bare `extern "C"` handler (which cannot capture) can
//! reach it.

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

/// Live-child counter the `SIGCHLD` handler decrements as it reaps.
static LIVE_CHILDREN: AtomicPtr<AtomicUsize> = AtomicPtr::new(ptr::null_mut());
/// Shutdown flag the `SIGINT`/`SIGTERM` handler raises.
static SHUTDOWN_FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(ptr::null_mut());

/// Install a `SIGCHLD` handler that reaps every exited child with
/// `waitpid(WNOHANG)` in a loop, decrementing `live_children` once per reap.
/// `SA_NOCLDSTOP` suppresses notifications for merely stopped children;
/// `SA_RESTART` keeps a blocking `accept` from failing with `EINTR`.
pub fn install_sigchld_reaper(live_children: &'static AtomicUsize) {
    LIVE_CHILDREN.store(
        live_children as *const AtomicUsize as *mut AtomicUsize,
        Ordering::SeqCst,
    );
    install(libc::SIGCHLD, sigchld_handler, libc::SA_RESTART | libc::SA_NOCLDSTOP);
}

/// Install a `SIGINT`+`SIGTERM` handler that flips `flag` to `true`. No
/// `SA_RESTART`: a blocking `accept` is interrupted with `EINTR` so the loop
/// wakes, sees the flag, and shuts down promptly.
pub fn install_shutdown_flag(flag: &'static AtomicBool) {
    SHUTDOWN_FLAG.store(
        flag as *const AtomicBool as *mut AtomicBool,
        Ordering::SeqCst,
    );
    install(libc::SIGINT, shutdown_handler, 0);
    install(libc::SIGTERM, shutdown_handler, 0);
}

extern "C" fn sigchld_handler(_sig: libc::c_int) {
    let counter = LIVE_CHILDREN.load(Ordering::SeqCst);
    if counter.is_null() {
        return;
    }
    // Reap every child that is ready; one SIGCHLD can cover several exits.
    loop {
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        unsafe { &*counter }.fetch_sub(1, Ordering::SeqCst);
    }
}

extern "C" fn shutdown_handler(_sig: libc::c_int) {
    let flag = SHUTDOWN_FLAG.load(Ordering::SeqCst);
    if !flag.is_null() {
        unsafe { &*flag }.store(true, Ordering::SeqCst);
    }
}

/// Install `handler` for `signum` with the given `sa_flags`.
fn install(signum: libc::c_int, handler: extern "C" fn(libc::c_int), flags: libc::c_int) {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as usize;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = flags;
        // Only fails on an invalid signal number, which these are not.
        let _ = libc::sigaction(signum, &action, ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `raise` in a multithreaded program targets the calling thread, and the
    // handler runs synchronously before `raise` returns (the signal is not
    // blocked) — so the flag is observable immediately, no spin needed.
    #[test]
    fn shutdown_flag_set_by_sigterm() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        install_shutdown_flag(&FLAG);
        assert!(!FLAG.load(Ordering::SeqCst));
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        assert!(FLAG.load(Ordering::SeqCst));
    }
}
