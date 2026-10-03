//! traytrayd internals.
//!
//! `store`, `pairing` and `feed` are pure or file-only logic with time and randomness passed
//! in, so they are testable without sockets. `server` (added later) wires them to transports.

pub mod feed;
pub mod pairing;
pub mod store;

/// Milliseconds on a monotonic clock owned by the caller. Never wall-clock time: the host
/// orders and ages things by its own clock, never by what apps claim.
pub type Millis = u64;

/// Milliseconds since the Unix epoch, used only for display (feed timestamps, pairing date).
pub type UnixMillis = u64;

/// Identifies one live connection. Frames are always attributed to the connection they
/// arrived on, which is how late frames from a dead connection are told apart.
pub type ConnId = u64;
