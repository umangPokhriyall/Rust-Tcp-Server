//! CPU affinity primitives for `multireactor` (Phase 2 §3).
//!
//! Two functions only:
//!
//! * [`pin_to_core`] — pin the calling thread to one logical core via
//!   `sched_setaffinity`. Used by each `multireactor` worker so the kernel
//!   scheduler cannot migrate that thread between cores; combined with one
//!   `SO_REUSEPORT` listener per worker this gives shared-nothing parallelism
//!   with hot caches.
//! * [`num_cores`] — the logical CPU count from `sysconf(_SC_NPROCESSORS_ONLN)`,
//!   the same number `multireactor` uses to bound its worker pool when
//!   `cfg.workers` is unset.
//!
//! Out-of-range core IDs are not an error: `pin_to_core` warns and continues
//! with the kernel default affinity (every logical core), so a configuration
//! mismatch produces a measurable degradation rather than a crash. Linux-only —
//! the syscalls below are Linux-specific and `multireactor` is gated to Linux
//! through the same `libc::sched_setaffinity` surface every other model uses.

use std::io;
use std::mem;

/// Pin the calling thread to one logical core via `sched_setaffinity(0, …)`.
///
/// A `core_id` greater than or equal to [`num_cores`] is warned on stderr and
/// the call returns `Ok(())` — leaving the kernel's default per-thread affinity
/// (all cores) in place. The rationale: pinning is a *performance hint*, not a
/// correctness requirement; a misconfigured `--workers` flag should not abort
/// the server. Any other failure (`EPERM`, etc.) bubbles up as `io::Error`.
pub fn pin_to_core(core_id: usize) -> io::Result<()> {
    let cores = num_cores();
    if core_id >= cores {
        eprintln!(
            "affinity: core_id {core_id} out of range (have {cores}) — leaving thread unpinned"
        );
        return Ok(());
    }
    // CPU_ZERO/CPU_SET on a zeroed cpu_set_t — touching only `core_id`'s bit.
    let mut set: libc::cpu_set_t = unsafe { mem::zeroed() };
    unsafe {
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(core_id, &mut set);
    }
    // `pid = 0` means the calling thread (Linux glibc's
    // sched_setaffinity targets the LWP, not the process).
    let res = unsafe {
        libc::sched_setaffinity(0, mem::size_of::<libc::cpu_set_t>(), &set)
    };
    if res == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Logical core count from `sysconf(_SC_NPROCESSORS_ONLN)`. Saturates to 1 if
/// the syscall returns 0 or `-1` — `multireactor` derives a thread count from
/// this number and 0 would be a footgun.
pub fn num_cores() -> usize {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n < 1 {
        1
    } else {
        n as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_cores_is_at_least_one() {
        assert!(num_cores() >= 1);
    }

    #[test]
    fn pin_to_core_zero_succeeds() {
        // Core 0 exists on every Linux box that runs the test suite.
        pin_to_core(0).expect("pin to core 0");
    }

    #[test]
    fn pin_to_core_out_of_range_warns_and_continues() {
        // Picking a wildly out-of-range id exercises the warn-and-continue
        // branch without depending on the host's actual core count.
        pin_to_core(usize::MAX).expect("out-of-range id should not error");
    }
}
