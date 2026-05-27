//! Bounded-input constants (DoS defense).
//!
//! Every constant here is a hard ceiling the parser enforces; exceeding one is
//! a fatal parse error. This is what makes the server slow-loris- and
//! memory-DoS-resistant.

/// Maximum size of the HTTP request line.
pub const MAX_REQUEST_LINE: usize = 8 * 1024;
/// Maximum size of the entire header block.
pub const MAX_HEADER_BYTES: usize = 32 * 1024;
/// Maximum number of header fields.
pub const MAX_HEADER_COUNT: usize = 100;
/// Maximum request body size (1 MiB).
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Suggested per-read chunk size for models.
pub const READ_CHUNK: usize = 16 * 1024;
