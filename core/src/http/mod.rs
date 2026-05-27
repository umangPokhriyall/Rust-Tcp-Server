//! HTTP primitives: method/version, headers, the incremental request parser,
//! and the response encoder. All sans-IO — these operate only on byte buffers.

pub mod headers;
pub mod method;
pub mod request;
pub mod response;
