//! Hard limits. They exist so that one buggy or hostile app cannot freeze the tray or
//! exhaust the host's memory; the core enforces them, shells may rely on them.

/// Largest frame, in bytes, including the trailing newline. Longer lines are discarded
/// without being buffered (see `FrameDecoder`).
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// Most items (list rows plus menu entries, counted recursively) one app may publish.
pub const MAX_ITEMS: usize = 200;

/// Most blocks in one app's tile.
pub const MAX_BLOCKS: usize = 32;

/// Deepest menu nesting accepted.
pub const MAX_MENU_DEPTH: usize = 4;

/// Character caps for rendered strings, after sanitising.
pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_TEXT_CHARS: usize = 2000;
pub const MAX_LABEL_CHARS: usize = 80;
pub const MAX_ID_CHARS: usize = 128;

/// Largest reply a shell may send, in characters.
pub const MAX_REPLY_CHARS: usize = 8000;

/// Event rate limit per app: sustained rate and burst size (token bucket).
pub const EVENTS_PER_SEC: u32 = 10;
pub const EVENT_BURST: u32 = 50;

/// State documents are coalesced per app; shells receive at most this many per second.
pub const STATE_FANOUT_PER_SEC: u32 = 4;

/// Heartbeat interval. A connection that misses two is treated as offline.
pub const HEARTBEAT_SECS: u64 = 10;
pub const MISSED_HEARTBEATS_OFFLINE: u64 = 2;

/// Feed retention per app.
pub const FEED_EVENTS_PER_APP: usize = 200;
