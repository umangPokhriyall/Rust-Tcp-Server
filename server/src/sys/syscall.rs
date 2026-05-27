//! The one place a raw `libc` return value becomes an `io::Result`.
//!
//! Every `sys` syscall goes through `cvt` (or the `syscall!` macro that wraps
//! it), which maps a `-1` return to `io::Error::last_os_error()`. This is the
//! only errno-handling code in the layer — nothing below re-implements it.

use std::io;
use std::time::Duration;

/// Convert a wait timeout to the millisecond `c_int` that `epoll_wait`/`poll`
/// expect: `None` blocks forever (`-1`); a duration is rounded down to whole
/// milliseconds and saturated at `c_int::MAX`.
pub(crate) fn timeout_to_millis(timeout: Option<Duration>) -> libc::c_int {
    match timeout {
        None => -1,
        Some(d) => {
            let ms = d.as_millis();
            if ms > libc::c_int::MAX as u128 {
                libc::c_int::MAX
            } else {
                ms as libc::c_int
            }
        }
    }
}

/// Integer return types whose error sentinel is `-1` (the libc convention).
pub trait IsMinusOne {
    fn is_minus_one(&self) -> bool;
}

macro_rules! impl_is_minus_one {
    ($($t:ident)*) => {$(
        impl IsMinusOne for $t {
            fn is_minus_one(&self) -> bool {
                *self == -1
            }
        }
    )*};
}

impl_is_minus_one! { i8 i16 i32 i64 isize }

/// Map a libc return value to an `io::Result`: `-1` becomes the current errno,
/// anything else passes through unchanged.
pub(crate) fn cvt<T: IsMinusOne>(t: T) -> io::Result<T> {
    if t.is_minus_one() {
        Err(io::Error::last_os_error())
    } else {
        Ok(t)
    }
}

/// `syscall!(fcntl(fd, F_GETFL))` => `cvt(unsafe { libc::fcntl(fd, F_GETFL) })`.
/// A thin convenience over [`cvt`] so call sites read like the syscall itself.
macro_rules! syscall {
    ($fn:ident ( $($arg:expr),* $(,)? )) => {
        $crate::sys::syscall::cvt(unsafe { libc::$fn($($arg),*) })
    };
}

pub(crate) use syscall;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvt_passes_through_non_negative() {
        assert_eq!(cvt(0_i32).unwrap(), 0);
        assert_eq!(cvt(7_i64).unwrap(), 7);
    }

    #[test]
    fn cvt_maps_minus_one_to_last_os_error() {
        // Force a known errno, then confirm `cvt` surfaces it.
        unsafe { *libc::__errno_location() = libc::EBADF };
        let err = cvt(-1_i32).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EBADF));
    }
}
