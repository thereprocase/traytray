//! Frame types. Every frame is one JSON object with a `type` tag, on one line.
//!
//! Direction matters for trust: `ClientFrame` is what the host *receives*, and the core
//! decides per connection role which variants are allowed (see `ClientFrame::allowed_for`).

use serde::{Deserialize, Serialize};

/// Bumped on any incompatible change. `hello` with a different value is refused, and the
/// app keeps its own UI.
pub const PROTO_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Quiet,
    Notice,
    NeedsYou,
    Alert,
}

impl Default for Urgency {
    fn default() -> Self {
        Urgency::Quiet
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Publishes its own state and events; receives actions and replies for itself.
    App,
    /// Draws the tray. Receives everything; may send actions, replies and dismisses.
    Shell,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Menu,
    Widgets,
    Panel,
}

// ---------------------------------------------------------------------------------------
// App state document
// ---------------------------------------------------------------------------------------

/// The whole published state of one app. Sent complete every time; the host keeps only the
/// latest, in memory.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct AppState {
    /// Freedesktop icon name or app-relative icon id; shells resolve it, never fetch it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_mark: Option<IconMark>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<MenuItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Block>,
    /// Tier 2 (beta). Ignored by alpha hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_url: Option<String>,
}

/// A small mark the host draws in a corner of its own icon on the app's behalf.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconMark {
    pub shape: MarkShape,
    pub tone: MarkTone,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarkShape {
    Dot,
    Ring,
    Bar,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarkTone {
    Neutral,
    Accent,
    Warn,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label: String,
    /// None for a submenu parent or a non-clickable line (e.g. a status line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<Confirm>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submenu: Vec<MenuItem>,
}

/// A confirmation the host draws before delivering the action. Only drawn when the app asks.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Confirm {
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// Label of the confirming button, e.g. "Exit".
    pub verb: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ActionDef {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<Confirm>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

/// Per-item privacy. Either flag set on the item, the connection, or (for events) the event
/// makes it apply.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Privacy {
    /// Never written to disk by the host: not to the feed, not as a dismiss record.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ephemeral: bool,
    /// Never shown in toasts beyond a generic line.
    #[serde(default, skip_serializing_if = "is_false")]
    pub private: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Status {
        id: String,
        text: String,
        #[serde(default)]
        urgency: Urgency,
        #[serde(default, flatten)]
        privacy: Privacy,
    },
    Text {
        text: String,
    },
    Progress {
        id: String,
        label: String,
        /// 0.0..=1.0, or None for indeterminate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        eta_secs: Option<u64>,
        #[serde(default)]
        urgency: Urgency,
        #[serde(default, flatten)]
        privacy: Privacy,
    },
    List {
        id: String,
        rows: Vec<Row>,
    },
    Buttons {
        actions: Vec<ActionDef>,
    },
    Reply {
        /// The item the reply is about (e.g. a session id); echoed back with the text.
        item_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub badge: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default)]
    pub urgency: Urgency,
    #[serde(default, flatten)]
    pub privacy: Privacy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionDef>,
}

// ---------------------------------------------------------------------------------------
// Frames the host receives
// ---------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Hello(Hello),
    Heartbeat,

    // App role.
    State(StateFrame),
    Event(EventFrame),

    // Shell role.
    Action(ShellAction),
    Reply(ShellReply),
    Dismiss(ShellDismiss),
    Revoke { app: String },
    PairOpen,
    PairAccept(PairAccept),
    PairReject { request_id: String },

    // Unauthenticated remote connections, while pairing.
    PairRequest(PairRequest),
    PairCode { request_id: String, code: String },
}

impl ClientFrame {
    pub fn kind(&self) -> &'static str {
        match self {
            ClientFrame::Hello(_) => "hello",
            ClientFrame::Heartbeat => "heartbeat",
            ClientFrame::State(_) => "state",
            ClientFrame::Event(_) => "event",
            ClientFrame::Action(_) => "action",
            ClientFrame::Reply(_) => "reply",
            ClientFrame::Dismiss(_) => "dismiss",
            ClientFrame::Revoke { .. } => "revoke",
            ClientFrame::PairOpen => "pair_open",
            ClientFrame::PairAccept(_) => "pair_accept",
            ClientFrame::PairReject { .. } => "pair_reject",
            ClientFrame::PairRequest(_) => "pair_request",
            ClientFrame::PairCode { .. } => "pair_code",
        }
    }

    /// Which frames each role may send after `hello`. This is the whole of invariant 4:
    /// only shells drive apps; apps only describe themselves.
    pub fn allowed_for(&self, role: Role) -> bool {
        use ClientFrame::*;
        match (self, role) {
            (Heartbeat, _) => true,
            (State(_) | Event(_), Role::App) => true,
            (
                Action(_) | Reply(_) | Dismiss(_) | Revoke { .. } | PairOpen | PairAccept(_)
                | PairReject { .. },
                Role::Shell,
            ) => true,
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Hello {
    pub proto_version: u32,
    pub role: Role,
    /// Required for apps. Remote apps are namespaced by the host as `<app_id>@<node>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Display name for local apps. Remote apps are shown under the name confirmed at pairing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Bearer token; required on remote connections, ignored locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Connection-wide privacy: applies to every item and event on this connection.
    #[serde(default, flatten)]
    pub privacy: Privacy,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StateFrame {
    /// Strictly increasing per app. The host drops any frame with rev <= the current one.
    pub rev: u64,
    pub doc: AppState,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EventFrame {
    /// App-generated, unique per app; the host dedupes on it.
    pub event_id: String,
    pub urgency: Urgency,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// Ask for a toast at `notice` urgency (needs_you and alert always toast).
    #[serde(default, skip_serializing_if = "is_false")]
    pub toast: bool,
    #[serde(default, flatten)]
    pub privacy: Privacy,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ShellAction {
    pub app: String,
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// The app state rev the click was made on.
    pub rev: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ShellReply {
    pub app: String,
    pub item_id: String,
    pub text: String,
    pub rev: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ShellDismiss {
    pub app: String,
    pub item_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PairRequest {
    pub app_id: String,
    pub name: String,
    #[serde(default)]
    pub requested_tiers: Vec<Tier>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PairAccept {
    pub request_id: String,
    pub display_name: String,
    pub tiers: Vec<Tier>,
}

// ---------------------------------------------------------------------------------------
// Frames the host sends
// ---------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostFrame {
    Welcome {
        proto_version: u32,
        heartbeat_secs: u64,
        /// The id the host files this app under (namespaced for remote apps).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_id: Option<String>,
    },
    Heartbeat,
    /// State accepted (or ignored as stale; `current` tells the app where the host is).
    Ack { rev: u64, current: u64 },

    // To apps.
    Action { action_id: String, item_id: Option<String>, rev: u64, shell: String },
    Reply { item_id: String, text: String, rev: u64, shell: String },
    Dismiss { item_id: String, shell: String },

    // To shells.
    Snapshot(Snapshot),
    Toast(Toast),
    PairPending(PairPending),
    PairShowCode { request_id: String, code: String, expires_secs: u64 },
    PairDone { request_id: String, outcome: PairOutcome },

    // To an app being paired.
    PairWaiting { request_id: String },
    PairGranted { app_id: String, token: String },

    Error { code: ErrorCode, message: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub apps: Vec<AppView>,
    /// Worst urgency over online apps; drives the host icon.
    pub icon_urgency: Urgency,
    /// Badge count for the host icon.
    pub badge: u32,
    pub muted: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AppView {
    pub app_id: String,
    pub name: String,
    /// "local" or the paired node's name. Drawn by the host; apps cannot set it.
    pub origin: String,
    pub online: bool,
    /// Seconds since last contact (host clock). 0 while online.
    pub last_seen_secs: u64,
    pub rev: u64,
    pub state: AppState,
    pub tiers: Vec<Tier>,
    pub volume: Volume,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Volume {
    #[default]
    All,
    BadgeOnly,
    Off,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Toast {
    pub app_id: String,
    pub event_id: String,
    pub urgency: Urgency,
    /// Already reduced to a generic line when the event or item is private.
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PairPending {
    pub request_id: String,
    /// What the app calls itself. Unverified.
    pub claimed_name: String,
    pub claimed_app_id: String,
    /// From Tailscale whois of the TCP peer. Verified.
    pub node_name: String,
    pub login: String,
    pub requested_tiers: Vec<Tier>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PairOutcome {
    Paired,
    Rejected,
    Expired,
    LockedOut,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ProtoMismatch,
    BadFrame,
    Oversized,
    HelloRequired,
    NotPermitted,
    AppIdTaken,
    UnknownApp,
    LimitExceeded,
    RateLimited,
    PairingClosed,
    PairingBusy,
    BadCode,
    CodeExpired,
    TokenRevoked,
    NodeMismatch,
}

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_orders_quiet_to_alert() {
        assert!(Urgency::Quiet < Urgency::Notice);
        assert!(Urgency::Notice < Urgency::NeedsYou);
        assert!(Urgency::NeedsYou < Urgency::Alert);
    }

    #[test]
    fn frames_round_trip_with_type_tags() {
        let f = ClientFrame::Event(EventFrame {
            event_id: "e1".into(),
            urgency: Urgency::NeedsYou,
            title: "Permission needed".into(),
            body: String::new(),
            item_id: Some("s1".into()),
            toast: false,
            privacy: Privacy { ephemeral: true, private: false },
        });
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""type":"event""#));
        assert!(json.contains(r#""urgency":"needs_you""#));
        assert!(json.contains(r#""ephemeral":true"#));
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), f);
    }

    #[test]
    fn block_kinds_are_tagged() {
        assert!(serde_json::from_str::<Block>(r#"{"kind":"row_typo","id":"x"}"#).is_err());
        let p: Block =
            serde_json::from_str(r#"{"kind":"progress","id":"j1","label":"Copy","value":0.5}"#)
                .unwrap();
        assert!(matches!(p, Block::Progress { value: Some(v), .. } if v == 0.5));
    }

    #[test]
    fn apps_cannot_send_shell_frames_and_vice_versa() {
        let action = ClientFrame::Action(ShellAction {
            app: "a".into(),
            action_id: "x".into(),
            item_id: None,
            rev: 1,
        });
        assert!(!action.allowed_for(Role::App));
        assert!(action.allowed_for(Role::Shell));
        let state = ClientFrame::State(StateFrame { rev: 1, doc: AppState::default() });
        assert!(state.allowed_for(Role::App));
        assert!(!state.allowed_for(Role::Shell));
        assert!(!ClientFrame::PairOpen.allowed_for(Role::App));
        // Hello and pairing frames are handled before a role exists.
        let hello = ClientFrame::Hello(Hello {
            proto_version: PROTO_VERSION,
            role: Role::App,
            app_id: None,
            name: None,
            token: None,
            privacy: Privacy::default(),
        });
        assert!(!hello.allowed_for(Role::App) && !hello.allowed_for(Role::Shell));
    }

    #[test]
    fn menu_item_enabled_defaults_true() {
        let m: MenuItem = serde_json::from_str(r#"{"label":"Exit","action_id":"exit"}"#).unwrap();
        assert!(m.enabled);
    }
}
