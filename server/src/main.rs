//! Phase 0 server binary skeleton.
//!
//! The hand-rolled CLI (`--model`, `--port`, `--assets-dir`), `App`
//! construction, and the `iterative` model are wired in Session E. For now this
//! only confirms the `server` binary links against the `core` crate.

mod models;

fn main() {
    // Touch a `core` constant so the dependency link is exercised at build time.
    let _ = core::limits::READ_CHUNK;
    eprintln!("server: not implemented yet (Phase 0, Session A skeleton)");
    std::process::exit(1);
}
