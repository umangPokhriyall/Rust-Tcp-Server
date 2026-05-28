//! `io-uring` — completion-based, purpose-built (Phase 2 §5).
//!
//! Single ring, single thread. Three primitives carry the model:
//!
//!   * **multishot accept** (`IORING_OP_ACCEPT | IORING_ACCEPT_MULTISHOT`)
//!     — one SQE, every incoming connection delivered as a CQE. No per-accept
//!     submission overhead.
//!   * **provided buffer ring** (`IORING_REGISTER_PBUF_RING`) — the kernel
//!     selects a buffer from a userspace-managed ring for each `recv`; the CQE
//!     reports which buffer holds the data. Removes per-read allocation and
//!     the user-side `buf` argument from the hot path.
//!   * **batched submission** — every SQE produced in one wake-up is pushed to
//!     the ring and flushed by a single `io_uring_enter`. We block on at least
//!     one completion with a 100 ms `Timespec` for periodic shutdown polling.
//!
//! The flow is purely completion-based: an accept CQE produces a
//! `core::Connection`; a recv CQE feeds bytes into `on_bytes` and the returned
//! `ConnAction` decides whether the next SQE is `Recv` (re-arm), `Send` (the
//! response from `pending_write`), or `Close` (the conn is finished). The same
//! sans-IO state machine that drove blocking `read`/`write` (Phase 0) and epoll
//! readiness (Phase 1) drives io_uring's completions — Phase 0 validated three
//! ways. `core` is unchanged.
//!
//! **Kernel gate (§5):** parsed from `uname.release` at startup. Multishot
//! accept and `register_buf_ring_with_flags` require ≥ 5.19; we refuse to run
//! on anything older and surface the kernel version in the error so the bench
//! script can record N/A and proceed.
//!
//! **Scope:** single ring, single thread — the fair-axis comparison against
//! single-thread `epoll-et`. Multi-ring thread-per-core is the production form
//! and the path to competing with `multireactor` on absolute throughput; it is
//! deliberately out of scope here (§5).

use std::collections::HashMap;
use std::io;
use std::mem;
use std::net::TcpListener;
use std::os::unix::io::{AsRawFd, RawFd};
use std::ptr;
use std::sync::atomic::{self, AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use io_uring::types::{BufRingEntry, Fd, SubmitArgs, Timespec};
use io_uring::{cqueue, opcode, squeue, IoUring as Ring};

use core::{bind_listener, App, ConnAction, Connection, Server, ServerConfig};

use crate::sys::signal;

/// Shared shutdown flag, raised by SIGINT/SIGTERM. The loop polls it once per
/// `submit_with_args` wake-up (every `TICK` at the latest), so shutdown latency
/// is bounded by `TICK`.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// SQ + CQ ring depth. Power of two. 1024 fits a typical Send/Recv pipeline
/// for 256 connections without ever blocking on a full SQ.
const RING_ENTRIES: u32 = 1024;
/// Provided-buffer-ring depth. Power of two. Each buffer is `BUF_SIZE` bytes —
/// total pool = 4 MiB at the defaults.
const NUM_BUFS: u16 = 256;
/// Per-buffer size — matches `core::limits::READ_CHUNK` so one CQE per request
/// covers the worst-case header block.
const BUF_SIZE: u32 = 16 * 1024;
/// Buffer-group id. We use exactly one group; the recv SQEs reference it via
/// `buf_group(BG_ID)` and `BUFFER_SELECT`.
const BG_ID: u16 = 1;
/// `submit_with_args` timeout — caps shutdown latency and stays well under the
/// default 30 s read timeout (the `core::Connection` deadline check is implicit
/// in `is_expired`, which we do not need here: a tcp peer that idles forever
/// stays parked in a recv CQE, and SIGINT will drop it via ring teardown).
const TICK_NS: u32 = 100_000_000;
/// Minimum kernel version (multishot accept + provided buffer ring registration).
const MIN_KERNEL: (u32, u32) = (5, 19);

// `user_data` layout: low 8 bits = op kind, upper 56 bits = the i32 fd
// sign-extended to i64. Lets a single u64 carry both the "which kind of CQE is
// this?" and "which connection?" dispatch keys with no slab indirection.
const OP_ACCEPT: u64 = 1;
const OP_RECV: u64 = 2;
const OP_SEND: u64 = 3;
const OP_CLOSE: u64 = 4;

#[inline]
fn pack(op: u64, fd: RawFd) -> u64 {
    (op & 0xff) | (((fd as i64) as u64) << 8)
}

#[inline]
fn unpack(ud: u64) -> (u64, RawFd) {
    let op = ud & 0xff;
    let fd = ((ud as i64) >> 8) as i32;
    (op, fd)
}

pub struct IoUring {
    verbose: bool,
}

impl IoUring {
    pub fn new(verbose: bool) -> Self {
        IoUring { verbose }
    }
}

impl Server for IoUring {
    fn name(&self) -> &'static str {
        "io-uring"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> io::Result<()> {
        check_kernel()?;
        signal::install_shutdown_flag(&SHUTDOWN);
        SHUTDOWN.store(false, Ordering::SeqCst);

        let listener = bind_listener(cfg.addr, false)?;
        eprintln!("io-uring: listening on http://{}", cfg.addr);

        let owned_cfg = crate::models::shared_config(cfg);
        let runtime_cfg = ServerConfig {
            addr: owned_cfg.addr,
            workers: owned_cfg.workers,
            read_timeout: owned_cfg.read_timeout,
            write_timeout: owned_cfg.write_timeout,
            max_connections: owned_cfg.max_connections,
            assets_dir: owned_cfg.assets_dir.clone(),
        };

        let mut rt = Runtime::new(listener, runtime_cfg, self.verbose)?;
        rt.run(&app)
    }
}

// ---- kernel gate ------------------------------------------------------------

/// Parse `uname -r` and refuse to run if older than `MIN_KERNEL`. The error
/// message names the observed and required versions verbatim so `bench/run.sh`
/// can grep it and record io_uring N/A (§5, §6).
fn check_kernel() -> io::Result<()> {
    let mut un: libc::utsname = unsafe { mem::zeroed() };
    if unsafe { libc::uname(&mut un) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // `un.release` is a fixed-length NUL-terminated C string. Walk to the NUL.
    let raw = un.release.as_ptr();
    let mut len = 0usize;
    while len < un.release.len() && unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    let bytes = unsafe { std::slice::from_raw_parts(raw as *const u8, len) };
    let s = std::str::from_utf8(bytes).unwrap_or("");
    let (major, minor) = parse_release(s);
    if (major, minor) < MIN_KERNEL {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "io_uring unavailable: kernel {major}.{minor} < {}.{}",
                MIN_KERNEL.0, MIN_KERNEL.1
            ),
        ));
    }
    Ok(())
}

fn parse_release(s: &str) -> (u32, u32) {
    let mut parts = s.split(|c: char| !c.is_ascii_digit());
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor)
}

// ---- provided buffer ring ---------------------------------------------------

/// Userspace-managed buffer ring registered with the kernel via
/// `IORING_REGISTER_PBUF_RING`. The kernel reads `bufs[(tail-1) & mask]` to
/// pick a buffer for each completion-based recv that sets `BUFFER_SELECT`; the
/// chosen buffer id comes back in the CQE flags. We recycle by writing the
/// buffer descriptor back to `bufs[tail & mask]` and bumping the tail (release).
///
/// Layout: the kernel aliases `bufs[0].resv` with the ring's `tail` field, so
/// the same 16 bytes hold either the head-of-buffer metadata or the tail
/// (different offsets within the struct). [`BufRingEntry::tail`] returns the
/// canonical tail pointer.
struct BufRing {
    /// Page-aligned mmap-backed ring memory of `NUM_BUFS` `BufRingEntry`s.
    ring: *mut BufRingEntry,
    /// Total size of the mmap region; needed for `munmap`.
    map_size: usize,
    /// The buffer pool. Stable address (`Box<[u8]>` doesn't reallocate).
    pool: Box<[u8]>,
    mask: u16,
    /// Local copy of the tail. Synced to the kernel via `commit()`.
    local_tail: u16,
}

// SAFETY: `BufRing` only exposes `&mut self` methods on its hot path; the
// pointers are private and never aliased.
unsafe impl Send for BufRing {}

impl BufRing {
    /// Allocate the ring memory + buffer pool, zero-initialize, fill every
    /// slot, and publish the tail. The caller is responsible for registering
    /// the ring with the kernel (`register_buf_ring_with_flags`) AFTER the
    /// initial fill — the kernel reads the entries lazily, but the protocol
    /// is to populate first.
    fn new(num_bufs: u16, buf_size: u32) -> io::Result<Self> {
        assert!(num_bufs.is_power_of_two());
        let entry_size = mem::size_of::<BufRingEntry>();
        let ring_bytes = num_bufs as usize * entry_size;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let map_size = ring_bytes.next_multiple_of(page);

        let ring = unsafe {
            libc::mmap(
                ptr::null_mut(),
                map_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if ring == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // Zero-init so `bufs[0].resv` (the tail) starts at 0.
        unsafe { ptr::write_bytes(ring as *mut u8, 0, map_size) };

        let pool: Box<[u8]> = vec![0u8; num_bufs as usize * buf_size as usize].into_boxed_slice();

        let mut br = BufRing {
            ring: ring as *mut BufRingEntry,
            map_size,
            pool,
            mask: num_bufs - 1,
            local_tail: 0,
        };
        // Stage every buffer; the tail is published once at the end.
        for bid in 0..num_bufs {
            unsafe { br.stage(bid) };
        }
        unsafe { br.commit() };
        Ok(br)
    }

    /// Address the kernel uses to find the buffer ring. Passed to
    /// `register_buf_ring_with_flags`.
    fn ring_addr(&self) -> u64 {
        self.ring as u64
    }

    /// Total slot count (== capacity).
    fn entries(&self) -> u16 {
        self.mask + 1
    }

    /// Borrow the `len` bytes that the kernel deposited into `bid`'s slot. The
    /// caller MUST recycle (`recycle(bid)`) before re-arming the recv on the
    /// same group, or the kernel will reuse the buffer mid-read.
    ///
    /// SAFETY: `bid` must be in range; `len` must be ≤ `BUF_SIZE`.
    unsafe fn bytes_ptr(&self, bid: u16) -> *const u8 {
        let off = bid as usize * BUF_SIZE as usize;
        self.pool.as_ptr().add(off)
    }

    /// Return `bid` to the ring and publish.
    unsafe fn recycle(&mut self, bid: u16) {
        self.stage(bid);
        self.commit();
    }

    /// Write the buffer descriptor at the current tail. Does NOT publish.
    unsafe fn stage(&mut self, bid: u16) {
        let idx = (self.local_tail & self.mask) as usize;
        let entry = &mut *self.ring.add(idx);
        let addr = self.pool.as_ptr().add(bid as usize * BUF_SIZE as usize) as u64;
        entry.set_addr(addr);
        entry.set_len(BUF_SIZE);
        entry.set_bid(bid);
        self.local_tail = self.local_tail.wrapping_add(1);
    }

    /// Publish the staged entries — release-store the tail so the kernel sees
    /// every prior write to the ring slots before the new tail.
    unsafe fn commit(&self) {
        let tail_ptr = BufRingEntry::tail(self.ring) as *mut AtomicU16;
        // `Release` pairs with the kernel's `Acquire` on the tail.
        (*tail_ptr).store(self.local_tail, Ordering::Release);
        // Belt-and-braces: a SeqCst fence guarantees the tail store is visible
        // before any subsequent submit-side write to the SQ tail.
        atomic::fence(Ordering::SeqCst);
    }
}

impl Drop for BufRing {
    fn drop(&mut self) {
        // The owning `Ring` is dropped FIRST (struct-field declaration order in
        // `Runtime`), which closes the io_uring fd — the kernel cancels every
        // in-flight op and stops reading the buf_ring before we unmap.
        unsafe {
            libc::munmap(self.ring as *mut libc::c_void, self.map_size);
        }
    }
}

// ---- per-connection state ---------------------------------------------------

/// Wrapped `core::Connection` plus the raw accepted fd. The fd is closed via an
/// `IORING_OP_CLOSE` SQE when the connection finishes — never via `libc::close`
/// directly, so the close is also batched into the next `io_uring_enter`.
struct ConnState {
    inner: Connection,
}

// ---- the runtime ------------------------------------------------------------

struct Runtime {
    // Drop order matters: `ring` first → kernel cancels and releases the
    // buf_ring before `br` munmaps it.
    ring: Ring,
    br: BufRing,
    conns: HashMap<RawFd, ConnState>,
    listener: TcpListener,
    cfg: ServerConfig,
    verbose: bool,
    /// True once the multishot accept SQE is armed. Multishot accept terminates
    /// the CQE chain on error (flagged by absence of `IORING_CQE_F_MORE`); we
    /// re-arm at the top of the next loop iteration.
    accept_armed: bool,
}

impl Runtime {
    fn new(listener: TcpListener, cfg: ServerConfig, verbose: bool) -> io::Result<Self> {
        let ring = Ring::builder()
            .setup_coop_taskrun()
            .build(RING_ENTRIES)?;

        let br = BufRing::new(NUM_BUFS, BUF_SIZE)?;
        unsafe {
            ring.submitter()
                .register_buf_ring_with_flags(br.ring_addr(), br.entries(), BG_ID, 0)?;
        }

        Ok(Runtime {
            ring,
            br,
            conns: HashMap::new(),
            listener,
            cfg,
            verbose,
            accept_armed: false,
        })
    }

    fn run(&mut self, app: &App) -> io::Result<()> {
        self.arm_accept();

        let ts = Timespec::new().nsec(TICK_NS);
        let args = SubmitArgs::new().timespec(&ts);

        while !SHUTDOWN.load(Ordering::SeqCst) {
            if !self.accept_armed {
                self.arm_accept();
            }
            match self.ring.submitter().submit_with_args(1, &args) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(libc::ETIME) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if self.verbose {
                        eprintln!("io-uring: submit_with_args: {e}");
                    }
                    continue;
                }
            }
            self.drain_cqes(app);
        }
        Ok(())
    }

    fn arm_accept(&mut self) {
        let fd = self.listener.as_raw_fd();
        let sqe = opcode::AcceptMulti::new(Fd(fd))
            .build()
            .user_data(pack(OP_ACCEPT, fd));
        if self.push(sqe).is_ok() {
            self.accept_armed = true;
        }
    }

    /// Push one SQE, flushing the queue with a single submit if it is full.
    /// Submission is otherwise batched — the outer loop calls a single
    /// `submit_with_args` per wake-up.
    fn push(&mut self, entry: squeue::Entry) -> io::Result<()> {
        // SAFETY: every SQE we push owns its referenced memory for the lifetime
        // of the operation: recv (no user buffer, kernel uses the provided
        // buffer ring), send (pending_write — Vec heap pointer stable while the
        // owning Connection is in `conns`), accept/close (no user pointer).
        {
            let mut sq = unsafe { self.ring.submission_shared() };
            if unsafe { sq.push(&entry) }.is_ok() {
                return Ok(());
            }
        }
        // SQ full — flush and retry once.
        let _ = self.ring.submit();
        let mut sq = unsafe { self.ring.submission_shared() };
        unsafe { sq.push(&entry) }
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))
    }

    fn drain_cqes(&mut self, app: &App) {
        // Snapshot the CQEs so the borrow on `self.ring` is released before we
        // push new SQEs from the per-op handlers.
        let cqes: Vec<cqueue::Entry> = unsafe { self.ring.completion_shared() }.collect();
        for cqe in cqes {
            let (op, fd) = unpack(cqe.user_data());
            let result = cqe.result();
            let flags = cqe.flags();
            match op {
                OP_ACCEPT => self.on_accept(result, flags, app),
                OP_RECV => self.on_recv(fd, result, flags, app),
                OP_SEND => self.on_send(fd, result, app),
                OP_CLOSE => { /* fire-and-forget: kernel closed the fd */ }
                _ => {
                    if self.verbose {
                        eprintln!("io-uring: unknown op {op} in CQE");
                    }
                }
            }
        }
    }

    // ---- accept CQE ----------------------------------------------------

    fn on_accept(&mut self, result: i32, flags: u32, app: &App) {
        if !cqueue::more(flags) {
            // Multishot terminated; we'll re-arm next loop iteration.
            self.accept_armed = false;
        }
        if result < 0 {
            if self.verbose {
                eprintln!("io-uring: accept errno {}", -result);
            }
            return;
        }
        let fd = result;
        if self.conns.len() >= self.cfg.max_connections {
            // Backpressure: shed the connection by closing it. The kernel
            // accept backlog continues to absorb further incoming connects.
            self.submit_close(fd);
            return;
        }
        self.conns.insert(
            fd,
            ConnState {
                inner: Connection::new(self.cfg.read_timeout),
            },
        );
        app.metrics().inc_connections();
        self.submit_recv(fd);
    }

    // ---- recv CQE ------------------------------------------------------

    fn on_recv(&mut self, fd: RawFd, result: i32, flags: u32, app: &App) {
        if result <= 0 {
            // EOF (0) or hard error (<0). Drop the connection.
            self.drop_conn(fd);
            return;
        }
        let Some(bid) = cqueue::buffer_select(flags) else {
            // The kernel said success but reported no buffer — should not
            // happen with BUFFER_SELECT; treat as a protocol error and bail.
            self.drop_conn(fd);
            return;
        };
        let n = result as usize;

        // Split-borrow `self`: `br.bytes_ptr` and `conns.get_mut` touch
        // different fields. We pull the raw pointer out (no borrow of `br`)
        // before reaching into `conns`.
        let bytes_ptr = unsafe { self.br.bytes_ptr(bid) };
        let action = if let Some(c) = self.conns.get_mut(&fd) {
            // SAFETY: the kernel wrote exactly `n` bytes into the provided
            // buffer; the pool memory is stable for the lifetime of `br`.
            let bytes = unsafe { std::slice::from_raw_parts(bytes_ptr, n) };
            c.inner.on_bytes(bytes, app)
        } else {
            // Conn was removed while the recv was in flight (e.g. shutdown).
            unsafe { self.br.recycle(bid) };
            return;
        };
        unsafe { self.br.recycle(bid) };
        self.apply(fd, action);
    }

    // ---- send CQE ------------------------------------------------------

    fn on_send(&mut self, fd: RawFd, result: i32, _app: &App) {
        if result < 0 {
            self.drop_conn(fd);
            return;
        }
        let action = match self.conns.get_mut(&fd) {
            Some(c) => c.inner.on_written(result as usize),
            None => return,
        };
        self.apply(fd, action);
    }

    // ---- shared action dispatch ---------------------------------------

    fn apply(&mut self, fd: RawFd, action: ConnAction) {
        match action {
            ConnAction::WantRead => self.submit_recv(fd),
            ConnAction::WantWrite => self.submit_send(fd),
            ConnAction::Close => self.drop_conn(fd),
        }
    }

    fn submit_recv(&mut self, fd: RawFd) {
        // BUFFER_SELECT + buf_group makes the kernel ignore the user-supplied
        // (buf, len) — we pass nulls; the actual receive lands in a provided
        // buffer of size BUF_SIZE.
        let sqe = opcode::Recv::new(Fd(fd), ptr::null_mut(), BUF_SIZE)
            .buf_group(BG_ID)
            .build()
            .flags(squeue::Flags::BUFFER_SELECT)
            .user_data(pack(OP_RECV, fd));
        let _ = self.push(sqe);
    }

    fn submit_send(&mut self, fd: RawFd) {
        // Grab the pending pointer + len, then drop the borrow before pushing.
        let (ptr, len) = match self.conns.get(&fd) {
            Some(c) => {
                let p = c.inner.pending_write();
                (p.as_ptr(), p.len() as u32)
            }
            None => return,
        };
        if len == 0 {
            // No bytes pending — drive the FSM forward synthetically.
            let action = match self.conns.get_mut(&fd) {
                Some(c) => c.inner.on_written(0),
                None => return,
            };
            self.apply(fd, action);
            return;
        }
        let sqe = opcode::Send::new(Fd(fd), ptr, len)
            .build()
            .user_data(pack(OP_SEND, fd));
        let _ = self.push(sqe);
    }

    fn submit_close(&mut self, fd: RawFd) {
        let sqe = opcode::Close::new(Fd(fd))
            .build()
            .user_data(pack(OP_CLOSE, fd));
        let _ = self.push(sqe);
    }

    fn drop_conn(&mut self, fd: RawFd) {
        if self.conns.remove(&fd).is_some() {
            self.submit_close(fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_data_roundtrip() {
        for &fd in &[3i32, 4, 1024, i32::MAX] {
            for op in [OP_ACCEPT, OP_RECV, OP_SEND, OP_CLOSE] {
                let (got_op, got_fd) = unpack(pack(op, fd));
                assert_eq!(got_op, op);
                assert_eq!(got_fd, fd);
            }
        }
    }

    #[test]
    fn parse_release_handles_dotted_and_dashed_strings() {
        assert_eq!(parse_release("6.1.0"), (6, 1));
        assert_eq!(parse_release("5.19.17-arch1-1"), (5, 19));
        assert_eq!(parse_release("7.0.0-15-generic"), (7, 0));
        assert_eq!(parse_release(""), (0, 0));
    }

    #[test]
    fn buf_ring_alloc_and_stage_fills_pool() {
        let br = BufRing::new(8, 64).expect("buf ring");
        assert_eq!(br.entries(), 8);
        // Pool sized for 8 * 64.
        assert_eq!(br.pool.len(), 8 * 64);
        // After init, the tail should equal NUM_BUFS (one entry per buffer).
        let tail = unsafe { *BufRingEntry::tail(br.ring) };
        assert_eq!(tail, 8);
    }
}
