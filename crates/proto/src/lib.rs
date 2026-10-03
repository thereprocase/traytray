//! Traytray protocol: frame types, limits, sanitising, framing and validation.
//!
//! This crate is pure (no I/O, no clock) so every rule in it can be tested on any machine.
//! The wire format is newline-delimited JSON; see docs/decisions/0001-ndjson-framing.md.

pub mod framing;
pub mod frames;
pub mod limits;
pub mod sanitize;
pub mod validate;

pub use frames::*;
