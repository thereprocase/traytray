//! In-memory host state: one entry per registered app.
//!
//! Nothing in this module touches disk (invariant 2). State documents, toasts and host-local
//! dismissals live only here. The one thing that may be persisted leaves as a `FeedEntry`
//! value, already filtered for `ephemeral`, for the feed module to write. Time and connection
//! ids are passed in, so every rule is testable without sockets or clocks.

use std::collections::{BTreeMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use traytray_proto::limits::{
    EVENT_BURST, EVENTS_PER_SEC, HEARTBEAT_SECS, MAX_NAME_CHARS, MISSED_HEARTBEATS_OFFLINE,
    STATE_FANOUT_PER_SEC,
};
use traytray_proto::sanitize::{clean, valid_id};
use traytray_proto::validate::{self, Invalid};
use traytray_proto::{
    AppState, AppView, Block, ErrorCode, EventFrame, Privacy, Snapshot, StateFrame, Tier, Toast,
    Urgency, Volume,
};

use crate::{ConnId, Millis, UnixMillis};

/// How many recent event ids each app's dedupe window holds.
pub const RECENT_EVENT_IDS: usize = 1024;

/// A connection that has been silent this long has missed `MISSED_HEARTBEATS_OFFLINE`
/// heartbeats.
const OFFLINE_AFTER_MS: Millis = HEARTBEAT_SECS * 1000 * MISSED_HEARTBEATS_OFFLINE;

/// Minimum spacing between snapshots sent to shells.
const FANOUT_INTERVAL_MS: Millis = 1000 / STATE_FANOUT_PER_SEC as Millis;

/// Generic toast lines for private events. The shell adds the confirmed app name.
const PRIVATE_TOAST_NEEDS_YOU: &str = "1 item needs you";
const PRIVATE_TOAST_NOTICE: &str = "1 new update";

/// Result of a state frame that came from the bound connection. A stale rev is not an error:
/// the app is told where the host is and can resynchronise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    pub accepted: bool,
    pub current_rev: u64,
}

/// Something the feed module may write. Built only for non-ephemeral events and dismisses.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FeedEntry {
    pub app_id: String,
    /// Host wall clock at the moment the host saw it; app timestamps are never used.
    pub wall: UnixMillis,
    #[serde(flatten)]
    pub kind: FeedKind,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeedKind {
    /// The cleaned event, with `privacy` set to the effective (OR-ed) flags.
    Event { event: EventFrame },
    /// A dismiss of a non-ephemeral item, attributed to the shell that sent it.
    Dismiss { item_id: String, shell: String },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventOutcome {
    pub feed: Option<FeedEntry>,
    pub toast: Option<Toast>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DismissOutcome {
    /// The app's live connection, if any. A dismiss is never queued for an offline app.
    pub deliver_to: Option<ConnId>,
    pub feed: Option<FeedEntry>,
}

/// Bounded set of recently seen event ids. Oldest ids fall out first, so memory per app is
/// capped regardless of how many events an app sends.
#[derive(Debug, Default)]
struct RecentIds {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl RecentIds {
    /// Returns false if the id was already present.
    fn insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        if self.order.len() >= RECENT_EVENT_IDS
            && let Some(oldest) = self.order.pop_front()
        {
            self.set.remove(&oldest);
        }
        self.order.push_back(id.to_owned());
        self.set.insert(id.to_owned());
        true
    }
}

/// Event token bucket, counted in thousandths of a token so refill is exact integer
/// arithmetic: each elapsed millisecond adds exactly `EVENTS_PER_SEC` milli-tokens.
#[derive(Debug)]
struct TokenBucket {
    milli_tokens: u64,
    last: Millis,
}

impl TokenBucket {
    const ONE: u64 = 1000;
    const CAPACITY: u64 = EVENT_BURST as u64 * Self::ONE;

    fn full(now: Millis) -> Self {
        Self { milli_tokens: Self::CAPACITY, last: now }
    }

    fn try_take(&mut self, now: Millis) -> bool {
        // A clock that steps backwards must not mint tokens.
        let elapsed = now.saturating_sub(self.last);
        self.last = self.last.max(now);
        self.milli_tokens = self
            .milli_tokens
            .saturating_add(elapsed.saturating_mul(u64::from(EVENTS_PER_SEC)))
            .min(Self::CAPACITY);
        if self.milli_tokens >= Self::ONE {
            self.milli_tokens -= Self::ONE;
            true
        } else {
            false
        }
    }
}

/// An item that carries urgency and privacy: a status, a progress block or a list row.
struct Item<'a> {
    id: &'a str,
    urgency: Urgency,
    privacy: Privacy,
}

fn items(state: &AppState) -> Vec<Item<'_>> {
    let mut out = Vec::new();
    for block in &state.blocks {
        match block {
            Block::Status { id, urgency, privacy, .. }
            | Block::Progress { id, urgency, privacy, .. } => {
                out.push(Item { id, urgency: *urgency, privacy: *privacy });
            }
            Block::List { rows, .. } => out.extend(rows.iter().map(|r| Item {
                id: &r.id,
                urgency: r.urgency,
                privacy: r.privacy,
            })),
            Block::Text { .. } | Block::Buttons { .. } | Block::Reply { .. } => {}
        }
    }
    out
}

fn either(a: Privacy, b: Privacy) -> Privacy {
    Privacy { ephemeral: a.ephemeral || b.ephemeral, private: a.private || b.private }
}

fn invalid_code(invalid: &Invalid) -> ErrorCode {
    match invalid {
        Invalid::TooManyItems(_) | Invalid::TooManyBlocks(_) | Invalid::MenuTooDeep => {
            ErrorCode::LimitExceeded
        }
        Invalid::TierNotGranted(_) => ErrorCode::NotPermitted,
        Invalid::BadId(_) | Invalid::DuplicateId(_) | Invalid::BadNumber(_) => ErrorCode::BadFrame,
    }
}

fn wants_toast(event: &EventFrame) -> bool {
    event.urgency >= Urgency::NeedsYou || (event.urgency == Urgency::Notice && event.toast)
}

/// `event` must already carry its effective privacy.
fn toast_for(app_id: &str, event: &EventFrame) -> Toast {
    let (title, body) = if event.privacy.private {
        let generic = if event.urgency >= Urgency::NeedsYou {
            PRIVATE_TOAST_NEEDS_YOU
        } else {
            PRIVATE_TOAST_NOTICE
        };
        (generic.to_owned(), String::new())
    } else {
        (event.title.clone(), event.body.clone())
    };
    Toast {
        app_id: app_id.to_owned(),
        event_id: event.event_id.clone(),
        urgency: event.urgency,
        title,
        body,
    }
}

#[derive(Debug)]
struct Entry {
    app_id: String,
    name: String,
    origin: String,
    tiers: Vec<Tier>,
    /// The live connection. `None` means offline; there is no separate online flag, so an
    /// app can never be online without a connection to deliver to.
    conn: Option<ConnId>,
    /// Privacy of the bound (or most recently bound) connection.
    conn_privacy: Privacy,
    /// Connection privacy in force when `state` was accepted. After a rebind the previous
    /// state stays visible under a connection whose flags may be weaker, and those items
    /// must keep the flags they were published with.
    state_privacy: Privacy,
    rev: u64,
    state: AppState,
    last_seen: Millis,
    volume: Volume,
    recent: RecentIds,
    bucket: TokenBucket,
    /// Host-local acknowledgements: item id to the level it was dismissed at.
    dismissed: BTreeMap<String, Urgency>,
}

impl Entry {
    fn online(&self) -> bool {
        self.conn.is_some()
    }

    fn touch(&mut self, now: Millis) {
        self.last_seen = self.last_seen.max(now);
    }

    /// Urgency and privacy of an item in the current state, including the privacy the state
    /// was published under.
    fn item(&self, id: &str) -> Option<(Urgency, Privacy)> {
        items(&self.state)
            .into_iter()
            .find(|i| i.id == id)
            .map(|i| (i.urgency, either(i.privacy, self.state_privacy)))
    }

    fn is_dismissed(&self, id: &str, urgency: Urgency) -> bool {
        self.dismissed.get(id).is_some_and(|&at| urgency <= at)
    }

    /// Re-evaluate dismissals against the current state. A raise above the dismissed level
    /// or the item disappearing ends the dismissal. A lowered level lowers the record too,
    /// so that a later raise back up (a new waiting episode) counts as a raise.
    fn prune_dismissals(&mut self) {
        let current: BTreeMap<&str, Urgency> =
            items(&self.state).into_iter().map(|i| (i.id, i.urgency)).collect();
        self.dismissed.retain(|id, at| match current.get(id.as_str()) {
            None => false,
            Some(&now) if now > *at => false,
            Some(&now) => {
                *at = now;
                true
            }
        });
    }

    fn view(&self, now: Millis) -> AppView {
        AppView {
            app_id: self.app_id.clone(),
            name: self.name.clone(),
            origin: self.origin.clone(),
            online: self.online(),
            last_seen_secs: if self.online() {
                0
            } else {
                now.saturating_sub(self.last_seen) / 1000
            },
            rev: self.rev,
            state: self.state.clone(),
            tiers: self.tiers.clone(),
            volume: self.volume,
        }
    }
}

/// The host's state for every app it has seen since it started.
#[derive(Debug, Default)]
pub struct Store {
    apps: BTreeMap<String, Entry>,
    muted: bool,
    dirty: bool,
    last_fanout: Option<Millis>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    fn bound_mut(&mut self, conn: ConnId) -> Option<&mut Entry> {
        self.apps.values_mut().find(|e| e.conn == Some(conn))
    }

    /// The app a connection is bound to, if any.
    pub fn app_of(&self, conn: ConnId) -> Option<&str> {
        self.apps.values().find(|e| e.conn == Some(conn)).map(|e| e.app_id.as_str())
    }

    /// The live connection of an app, if it is online.
    pub fn conn_of(&self, app_id: &str) -> Option<ConnId> {
        self.apps.get(app_id).and_then(|e| e.conn)
    }

    /// Bind `app_id` to `conn`. `name` and `origin` come from the caller: the hello name for
    /// local apps, the pairing record for remote ones. A connection binds one app, once.
    #[allow(clippy::too_many_arguments)]
    pub fn register(
        &mut self,
        conn: ConnId,
        app_id: &str,
        name: &str,
        origin: &str,
        tiers: &[Tier],
        privacy: Privacy,
        now: Millis,
    ) -> Result<(), ErrorCode> {
        if !valid_id(app_id) {
            return Err(ErrorCode::BadFrame);
        }
        if self.app_of(conn).is_some() {
            return Err(ErrorCode::NotPermitted);
        }
        let mut name = clean(name, MAX_NAME_CHARS, false);
        if name.is_empty() {
            name = clean(app_id, MAX_NAME_CHARS, false);
        }
        let origin = clean(origin, MAX_NAME_CHARS, false);

        match self.apps.get_mut(app_id) {
            Some(e) if e.online() => return Err(ErrorCode::AppIdTaken),
            Some(e) => {
                // A new connection starts a new rev epoch. The old state stays visible until
                // the new one arrives, but only if it fits the tiers this connection holds.
                // Volume, dedupe window and rate bucket carry over, so reconnecting neither
                // forgets the user's setting nor replays or refills anything.
                e.conn = Some(conn);
                e.rev = 0;
                e.name = name;
                e.origin = origin;
                e.tiers = tiers.to_vec();
                e.conn_privacy = privacy;
                e.touch(now);
                if validate::state(&e.state, tiers).is_err() {
                    e.state = AppState::default();
                    e.dismissed.clear();
                }
            }
            None => {
                self.apps.insert(
                    app_id.to_owned(),
                    Entry {
                        app_id: app_id.to_owned(),
                        name,
                        origin,
                        tiers: tiers.to_vec(),
                        conn: Some(conn),
                        conn_privacy: privacy,
                        state_privacy: privacy,
                        rev: 0,
                        state: AppState::default(),
                        last_seen: now,
                        volume: Volume::default(),
                        recent: RecentIds::default(),
                        bucket: TokenBucket::full(now),
                        dismissed: BTreeMap::new(),
                    },
                );
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// The connection closed. Its app goes offline; its state stays in memory as a stale tile.
    /// A connection that no longer owns the app (it was rebound) changes nothing.
    pub fn disconnect(&mut self, conn: ConnId, now: Millis) {
        if let Some(e) = self.bound_mut(conn) {
            e.conn = None;
            e.touch(now);
            self.dirty = true;
        }
    }

    pub fn apply_state(
        &mut self,
        conn: ConnId,
        frame: &StateFrame,
        now: Millis,
    ) -> Result<Ack, (ErrorCode, String)> {
        let Some(e) = self.bound_mut(conn) else {
            return Err((ErrorCode::NotPermitted, "connection has no registered app".into()));
        };
        e.touch(now);
        if frame.rev <= e.rev {
            return Ok(Ack { accepted: false, current_rev: e.rev });
        }
        let doc = validate::state(&frame.doc, &e.tiers)
            .map_err(|invalid| (invalid_code(&invalid), invalid.to_string()))?;
        e.rev = frame.rev;
        e.state = doc;
        e.state_privacy = e.conn_privacy;
        e.prune_dismissals();
        self.dirty = true;
        Ok(Ack { accepted: true, current_rev: frame.rev })
    }

    pub fn apply_event(
        &mut self,
        conn: ConnId,
        frame: &EventFrame,
        now: Millis,
        wall: UnixMillis,
    ) -> Result<EventOutcome, ErrorCode> {
        let muted = self.muted;
        let e = self.bound_mut(conn).ok_or(ErrorCode::NotPermitted)?;
        e.touch(now);
        // Rate limit before any other work, so invalid or duplicate floods are limited too.
        if !e.bucket.try_take(now) {
            return Err(ErrorCode::RateLimited);
        }
        // Validate before remembering the id: an unchecked id could be arbitrarily large.
        let mut event = validate::event(frame).map_err(|invalid| invalid_code(&invalid))?;
        if !e.recent.insert(&event.event_id) {
            return Ok(EventOutcome::default());
        }

        let item_privacy = event
            .item_id
            .as_deref()
            .and_then(|id| e.item(id))
            .map(|(_, privacy)| privacy)
            .unwrap_or_default();
        event.privacy = either(either(event.privacy, e.conn_privacy), item_privacy);

        let toast = (wants_toast(&event) && !muted && e.volume == Volume::All)
            .then(|| toast_for(&e.app_id, &event));
        let feed = (!event.privacy.ephemeral).then(|| FeedEntry {
            app_id: e.app_id.clone(),
            wall,
            kind: FeedKind::Event { event },
        });
        Ok(EventOutcome { feed, toast })
    }

    pub fn heartbeat(&mut self, conn: ConnId, now: Millis) {
        if let Some(e) = self.bound_mut(conn) {
            e.touch(now);
        }
    }

    /// Take offline every app that has missed its heartbeats. Its connection is unbound, so
    /// anything that connection sends later is refused and the caller should close it.
    pub fn tick(&mut self, now: Millis) -> Vec<String> {
        let mut gone = Vec::new();
        for e in self.apps.values_mut() {
            if e.online() && now.saturating_sub(e.last_seen) > OFFLINE_AFTER_MS {
                e.conn = None;
                gone.push(e.app_id.clone());
            }
        }
        if !gone.is_empty() {
            self.dirty = true;
        }
        gone
    }

    /// A shell acknowledged an item. Host-local: it silences the badge for that item on this
    /// host and is passed to the app only if the app is connected now.
    pub fn dismiss(
        &mut self,
        app_id: &str,
        item_id: &str,
        shell: &str,
        wall: UnixMillis,
    ) -> Result<DismissOutcome, ErrorCode> {
        if !valid_id(item_id) {
            return Err(ErrorCode::BadFrame);
        }
        let e = self.apps.get_mut(app_id).ok_or(ErrorCode::UnknownApp)?;
        let deliver_to = e.conn;
        // An item the host cannot see has unknown privacy, so nothing about it is recorded
        // or written. The app still hears about it and decides whether it is stale.
        let Some((urgency, item_privacy)) = e.item(item_id) else {
            return Ok(DismissOutcome { deliver_to, feed: None });
        };
        e.dismissed.insert(item_id.to_owned(), urgency);
        let privacy = either(item_privacy, e.conn_privacy);
        let feed = (!privacy.ephemeral).then(|| FeedEntry {
            app_id: e.app_id.clone(),
            wall,
            kind: FeedKind::Dismiss { item_id: item_id.to_owned(), shell: shell.to_owned() },
        });
        self.dirty = true;
        Ok(DismissOutcome { deliver_to, feed })
    }

    pub fn snapshot(&self, now: Millis) -> Snapshot {
        let mut icon_urgency = Urgency::Quiet;
        let mut badge: u64 = 0;
        // Volume is the user's and overrides everything, offline notices included.
        for e in self.apps.values().filter(|e| e.volume != Volume::Off) {
            if !e.online() {
                icon_urgency = icon_urgency.max(Urgency::Notice);
                badge += 1;
                continue;
            }
            for item in items(&e.state) {
                icon_urgency = icon_urgency.max(item.urgency);
                if item.urgency >= Urgency::Notice && !e.is_dismissed(item.id, item.urgency) {
                    badge += 1;
                }
            }
        }

        let mut apps: Vec<&Entry> = self.apps.values().collect();
        apps.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.app_id.cmp(&b.app_id)));
        Snapshot {
            apps: apps.into_iter().map(|e| e.view(now)).collect(),
            icon_urgency,
            badge: u32::try_from(badge).unwrap_or(u32::MAX),
            muted: self.muted,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The latest snapshot, if something visible changed and shells have not had one within
    /// the fanout interval. Intermediate changes are coalesced: the latest wins.
    pub fn take_fanout(&mut self, now: Millis) -> Option<Snapshot> {
        if !self.dirty {
            return None;
        }
        if let Some(last) = self.last_fanout
            && now.saturating_sub(last) < FANOUT_INTERVAL_MS
        {
            return None;
        }
        self.dirty = false;
        self.last_fanout = Some(now);
        Some(self.snapshot(now))
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn set_mute(&mut self, muted: bool) {
        if self.muted != muted {
            self.muted = muted;
            self.dirty = true;
        }
    }

    pub fn set_volume(&mut self, app_id: &str, volume: Volume) -> Result<(), ErrorCode> {
        let e = self.apps.get_mut(app_id).ok_or(ErrorCode::UnknownApp)?;
        if e.volume != volume {
            e.volume = volume;
            self.dirty = true;
        }
        Ok(())
    }

    /// Forget an app entirely (revoke or unpair). Returns its live connection, which the
    /// caller must close; anything that connection sends afterwards is refused.
    pub fn remove_app(&mut self, app_id: &str) -> Result<Option<ConnId>, ErrorCode> {
        let e = self.apps.remove(app_id).ok_or(ErrorCode::UnknownApp)?;
        self.dirty = true;
        Ok(e.conn)
    }

    /// True while any ephemeral content is held, during which the core must not write its own
    /// crash or log files.
    pub fn holds_ephemeral(&self) -> bool {
        self.apps.values().any(|e| {
            (e.online() && e.conn_privacy.ephemeral)
                || (e.state_privacy.ephemeral && e.state != AppState::default())
                || items(&e.state).iter().any(|i| i.privacy.ephemeral)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use traytray_proto::Row;

    const ALL: &[Tier] = &[Tier::Menu, Tier::Widgets, Tier::Panel];
    const PLAIN: Privacy = Privacy { ephemeral: false, private: false };
    const PRIVATE: Privacy = Privacy { ephemeral: false, private: true };
    const EPHEMERAL: Privacy = Privacy { ephemeral: true, private: false };

    fn store_with(app: &str, conn: ConnId, privacy: Privacy) -> Store {
        let mut s = Store::new();
        s.register(conn, app, app, "local", ALL, privacy, 0).unwrap();
        s
    }

    fn row(id: &str, urgency: Urgency, privacy: Privacy) -> Row {
        Row {
            id: id.into(),
            title: format!("title {id}"),
            subtitle: String::new(),
            badge: String::new(),
            icon: None,
            progress: None,
            urgency,
            privacy,
            actions: vec![],
        }
    }

    fn rows(rev: u64, rows: Vec<Row>) -> StateFrame {
        StateFrame {
            rev,
            doc: AppState {
                blocks: vec![Block::List { id: "list".into(), rows }],
                ..Default::default()
            },
        }
    }

    fn one(rev: u64, id: &str, urgency: Urgency) -> StateFrame {
        rows(rev, vec![row(id, urgency, PLAIN)])
    }

    fn event(id: &str, urgency: Urgency) -> EventFrame {
        EventFrame {
            event_id: id.into(),
            urgency,
            title: "Permission needed".into(),
            body: "rm -rf build".into(),
            item_id: None,
            toast: false,
            privacy: PLAIN,
        }
    }

    // --- registration and binding -------------------------------------------------------

    #[test]
    fn second_live_connection_cannot_take_an_app_id() {
        let mut s = store_with("a", 1, PLAIN);
        assert_eq!(s.register(2, "a", "a", "local", ALL, PLAIN, 0), Err(ErrorCode::AppIdTaken));
        assert_eq!(s.conn_of("a"), Some(1));
    }

    #[test]
    fn a_connection_binds_one_app_once() {
        let mut s = store_with("a", 1, PLAIN);
        assert_eq!(s.register(1, "b", "b", "local", ALL, PLAIN, 0), Err(ErrorCode::NotPermitted));
        assert_eq!(s.register(1, "a", "a", "local", ALL, PLAIN, 0), Err(ErrorCode::NotPermitted));
        assert_eq!(s.register(3, "bad id", "x", "local", ALL, PLAIN, 0), Err(ErrorCode::BadFrame));
    }

    #[test]
    fn state_from_a_connection_without_an_app_is_refused() {
        let mut s = store_with("a", 1, PLAIN);
        let err = s.apply_state(9, &one(1, "r", Urgency::Quiet), 0).unwrap_err();
        assert_eq!(err.0, ErrorCode::NotPermitted);
        assert_eq!(
            s.apply_event(9, &event("e", Urgency::Alert), 0, 0),
            Err(ErrorCode::NotPermitted)
        );
    }

    #[test]
    fn stale_rev_is_dropped_and_reports_current() {
        let mut s = store_with("a", 1, PLAIN);
        assert_eq!(
            s.apply_state(1, &one(5, "r", Urgency::Notice), 0),
            Ok(Ack { accepted: true, current_rev: 5 })
        );
        for stale in [5, 4, 0] {
            assert_eq!(
                s.apply_state(1, &one(stale, "r", Urgency::Alert), 0),
                Ok(Ack { accepted: false, current_rev: 5 })
            );
        }
        assert_eq!(s.snapshot(0).icon_urgency, Urgency::Notice);
    }

    #[test]
    fn rebind_starts_a_new_rev_epoch_and_keeps_old_state_until_new_state() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(40, "old", Urgency::Notice), 0).unwrap();
        s.disconnect(1, 100);
        s.register(2, "a", "a", "local", ALL, PLAIN, 200).unwrap();

        let view = &s.snapshot(200).apps[0];
        assert_eq!(view.rev, 0);
        assert!(view.online);
        assert_eq!(items(&view.state)[0].id, "old");

        assert!(s.apply_state(2, &one(1, "new", Urgency::Quiet), 300).unwrap().accepted);
        assert_eq!(items(&s.snapshot(300).apps[0].state)[0].id, "new");
    }

    #[test]
    fn late_frames_from_the_old_connection_are_refused_after_rebind() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(1, "r", Urgency::Quiet), 0).unwrap();
        s.disconnect(1, 10);
        s.register(2, "a", "a", "local", ALL, PLAIN, 20).unwrap();

        let err = s.apply_state(1, &one(99, "r", Urgency::Alert), 30).unwrap_err();
        assert_eq!(err.0, ErrorCode::NotPermitted);
        assert_eq!(
            s.apply_event(1, &event("late", Urgency::Alert), 30, 0),
            Err(ErrorCode::NotPermitted)
        );
        assert_eq!(s.snapshot(30).apps[0].rev, 0);
        // The new connection's epoch is unaffected by the late frame.
        assert!(s.apply_state(2, &one(1, "r", Urgency::Quiet), 40).unwrap().accepted);
    }

    #[test]
    fn closing_the_old_connection_does_not_take_a_rebound_app_offline() {
        let mut s = store_with("a", 1, PLAIN);
        s.heartbeat(1, 0);
        s.disconnect(1, 10);
        s.register(2, "a", "a", "local", ALL, PLAIN, 20).unwrap();
        s.disconnect(1, 30);
        s.heartbeat(1, 30);
        assert_eq!(s.conn_of("a"), Some(2));
    }

    #[test]
    fn kept_state_is_cleared_if_the_new_connection_lacks_its_tiers() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(1, "r", Urgency::Alert), 0).unwrap();
        s.disconnect(1, 0);
        s.register(2, "a", "a", "node", &[Tier::Menu], PLAIN, 0).unwrap();
        let snap = s.snapshot(0);
        assert_eq!(snap.apps[0].state, AppState::default());
        assert_eq!(snap.icon_urgency, Urgency::Quiet);
    }

    #[test]
    fn state_using_an_ungranted_tier_is_refused() {
        let mut s = Store::new();
        s.register(1, "a", "a", "node", &[Tier::Menu], PLAIN, 0).unwrap();
        let err = s.apply_state(1, &one(1, "r", Urgency::Quiet), 0).unwrap_err();
        assert_eq!(err.0, ErrorCode::NotPermitted);
        assert_eq!(s.snapshot(0).apps[0].rev, 0);
    }

    #[test]
    fn names_are_sanitised_and_never_empty() {
        let mut s = Store::new();
        s.register(1, "a", "\u{1b}[31mEvil\u{202E}", "local", ALL, PLAIN, 0).unwrap();
        s.register(2, "b", "", "local", ALL, PLAIN, 0).unwrap();
        let names: Vec<_> = s.snapshot(0).apps.into_iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["Evil", "b"]);
    }

    // --- events -------------------------------------------------------------------------

    #[test]
    fn duplicate_event_ids_do_nothing() {
        let mut s = store_with("a", 1, PLAIN);
        let first = s.apply_event(1, &event("e1", Urgency::NeedsYou), 0, 0).unwrap();
        assert!(first.toast.is_some() && first.feed.is_some());
        let again = s.apply_event(1, &event("e1", Urgency::NeedsYou), 0, 0).unwrap();
        assert_eq!(again, EventOutcome::default());
    }

    #[test]
    fn dedupe_window_is_bounded_and_survives_reconnect() {
        let mut s = store_with("a", 1, PLAIN);
        let mut now = 0;
        s.apply_event(1, &event("first", Urgency::Quiet), now, 0).unwrap();
        s.disconnect(1, now);
        s.register(2, "a", "a", "local", ALL, PLAIN, now).unwrap();
        assert_eq!(
            s.apply_event(2, &event("first", Urgency::Quiet), now, 0),
            Ok(EventOutcome::default())
        );

        for i in 0..RECENT_EVENT_IDS {
            now += 100; // stays inside the rate limit
            s.apply_event(2, &event(&format!("e{i}"), Urgency::Quiet), now, 0).unwrap();
        }
        let entry = &s.apps["a"];
        assert_eq!(entry.recent.order.len(), RECENT_EVENT_IDS);
        assert_eq!(entry.recent.set.len(), RECENT_EVENT_IDS);
        // "first" has been evicted, so it is accepted again.
        now += 100;
        assert!(s.apply_event(2, &event("first", Urgency::Quiet), now, 0).unwrap().feed.is_some());
    }

    #[test]
    fn invalid_event_ids_are_not_remembered() {
        let mut s = store_with("a", 1, PLAIN);
        let huge = "x".repeat(10_000);
        assert_eq!(s.apply_event(1, &event(&huge, Urgency::Quiet), 0, 0), Err(ErrorCode::BadFrame));
        assert!(s.apps["a"].recent.order.is_empty());
    }

    #[test]
    fn events_are_rate_limited_by_burst_then_rate() {
        let mut s = store_with("a", 1, PLAIN);
        for i in 0..EVENT_BURST {
            assert!(s.apply_event(1, &event(&format!("b{i}"), Urgency::Quiet), 0, 0).is_ok());
        }
        assert_eq!(
            s.apply_event(1, &event("over", Urgency::Quiet), 0, 0),
            Err(ErrorCode::RateLimited)
        );
        // One token per 1000 / EVENTS_PER_SEC ms.
        let per_token = 1000 / u64::from(EVENTS_PER_SEC);
        assert_eq!(
            s.apply_event(1, &event("early", Urgency::Quiet), per_token - 1, 0),
            Err(ErrorCode::RateLimited)
        );
        assert!(s.apply_event(1, &event("ontime", Urgency::Quiet), per_token, 0).is_ok());
        assert_eq!(
            s.apply_event(1, &event("again", Urgency::Quiet), per_token, 0),
            Err(ErrorCode::RateLimited)
        );
        // A long silence refills to the burst size and no further.
        let later = 1_000_000;
        let mut ok = 0;
        for i in 0..EVENT_BURST * 2 {
            if s.apply_event(1, &event(&format!("l{i}"), Urgency::Quiet), later, 0).is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, EVENT_BURST);
    }

    #[test]
    fn reconnecting_does_not_refill_the_rate_limit() {
        let mut s = store_with("a", 1, PLAIN);
        for i in 0..EVENT_BURST {
            s.apply_event(1, &event(&format!("b{i}"), Urgency::Quiet), 0, 0).unwrap();
        }
        s.disconnect(1, 0);
        s.register(2, "a", "a", "local", ALL, PLAIN, 0).unwrap();
        assert_eq!(
            s.apply_event(2, &event("x", Urgency::Quiet), 0, 0),
            Err(ErrorCode::RateLimited)
        );
    }

    #[test]
    fn rate_limited_events_do_not_consume_their_id() {
        let mut s = store_with("a", 1, PLAIN);
        for i in 0..EVENT_BURST {
            s.apply_event(1, &event(&format!("b{i}"), Urgency::Quiet), 0, 0).unwrap();
        }
        assert!(s.apply_event(1, &event("alert", Urgency::Alert), 0, 0).is_err());
        // Refused while limited, so the id was not consumed and it goes through later.
        assert!(
            s.apply_event(1, &event("alert", Urgency::Alert), 1000, 0).unwrap().toast.is_some()
        );
    }

    fn privacy_of(s: &mut Store, conn: ConnId, e: EventFrame) -> (Option<Privacy>, Option<Toast>) {
        let out = s.apply_event(conn, &e, 0, 0).unwrap();
        let feed = out.feed.map(|f| match f.kind {
            FeedKind::Event { event } => event.privacy,
            FeedKind::Dismiss { .. } => panic!("event produced a dismiss entry"),
        });
        (feed, out.toast)
    }

    #[test]
    fn event_privacy_is_event_or_connection_or_item() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(
            1,
            &rows(
                1,
                vec![
                    row("plain", Urgency::Quiet, PLAIN),
                    row("secret", Urgency::Quiet, PRIVATE),
                    row("gone", Urgency::Quiet, EPHEMERAL),
                ],
            ),
            0,
        )
        .unwrap();

        let (feed, _) = privacy_of(&mut s, 1, event("e1", Urgency::Quiet));
        assert_eq!(feed, Some(PLAIN));

        let mut e = event("e2", Urgency::Quiet);
        e.privacy = PRIVATE;
        assert_eq!(privacy_of(&mut s, 1, e).0, Some(PRIVATE));

        let mut e = event("e3", Urgency::Quiet);
        e.item_id = Some("secret".into());
        assert_eq!(privacy_of(&mut s, 1, e).0, Some(PRIVATE));

        let mut e = event("e4", Urgency::Quiet);
        e.item_id = Some("gone".into());
        assert_eq!(privacy_of(&mut s, 1, e).0, None, "ephemeral item: no feed entry");

        let mut e = event("e5", Urgency::Quiet);
        e.privacy = EPHEMERAL;
        assert_eq!(privacy_of(&mut s, 1, e).0, None, "ephemeral event: no feed entry");

        let mut e = event("e6", Urgency::Quiet);
        e.item_id = Some("unknown".into());
        assert_eq!(privacy_of(&mut s, 1, e).0, Some(PLAIN));
    }

    #[test]
    fn connection_privacy_applies_to_every_event() {
        let mut s = store_with("p", 1, PRIVATE);
        s.register(2, "e", "e", "local", ALL, EPHEMERAL, 0).unwrap();
        assert_eq!(privacy_of(&mut s, 1, event("x", Urgency::Quiet)).0, Some(PRIVATE));
        let (feed, toast) = privacy_of(&mut s, 2, event("y", Urgency::Alert));
        assert_eq!(feed, None);
        assert!(toast.is_some(), "ephemeral governs disk, not toasts");
    }

    #[test]
    fn ephemeral_state_stays_ephemeral_after_rebinding_to_a_plain_connection() {
        let mut s = store_with("a", 1, EPHEMERAL);
        s.apply_state(1, &one(1, "job", Urgency::Notice), 0).unwrap();
        s.disconnect(1, 0);
        s.register(2, "a", "a", "local", ALL, PLAIN, 0).unwrap();

        let mut e = event("e", Urgency::Quiet);
        e.item_id = Some("job".into());
        assert_eq!(privacy_of(&mut s, 2, e).0, None);
        assert_eq!(s.dismiss("a", "job", "shell-1", 0).unwrap().feed, None);
        assert!(s.holds_ephemeral());

        // Once the plain connection publishes, its own state is no longer ephemeral.
        s.apply_state(2, &one(1, "job", Urgency::Notice), 0).unwrap();
        assert!(s.dismiss("a", "job", "shell-1", 0).unwrap().feed.is_some());
        assert!(!s.holds_ephemeral());
    }

    #[test]
    fn toast_edges_follow_urgency_and_the_toast_flag() {
        let mut s = store_with("a", 1, PLAIN);
        let cases = [
            (Urgency::Quiet, false, false),
            (Urgency::Quiet, true, false),
            (Urgency::Notice, false, false),
            (Urgency::Notice, true, true),
            (Urgency::NeedsYou, false, true),
            (Urgency::Alert, false, true),
        ];
        for (i, (urgency, flag, expect)) in cases.into_iter().enumerate() {
            let mut e = event(&format!("e{i}"), urgency);
            e.toast = flag;
            let toast = s.apply_event(1, &e, 0, 0).unwrap().toast;
            assert_eq!(toast.is_some(), expect, "{urgency:?} toast={flag}");
            if let Some(t) = toast {
                assert_eq!(
                    (t.title.as_str(), t.body.as_str()),
                    ("Permission needed", "rm -rf build")
                );
                assert_eq!(t.app_id, "a");
            }
        }
    }

    #[test]
    fn mute_and_volume_suppress_toasts() {
        let mut s = store_with("a", 1, PLAIN);
        s.set_mute(true);
        assert!(s.apply_event(1, &event("m", Urgency::Alert), 0, 0).unwrap().toast.is_none());
        s.set_mute(false);
        for (i, volume) in [Volume::BadgeOnly, Volume::Off].into_iter().enumerate() {
            s.set_volume("a", volume).unwrap();
            let out = s.apply_event(1, &event(&format!("v{i}"), Urgency::Alert), 0, 0).unwrap();
            assert!(out.toast.is_none(), "{volume:?}");
            assert!(out.feed.is_some(), "volume does not hide history");
        }
        s.set_volume("a", Volume::All).unwrap();
        assert!(s.apply_event(1, &event("on", Urgency::Alert), 0, 0).unwrap().toast.is_some());
    }

    #[test]
    fn private_toasts_are_generic() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &rows(1, vec![row("secret", Urgency::Quiet, PRIVATE)]), 0).unwrap();
        let mut e = event("n", Urgency::Notice);
        e.toast = true;
        e.privacy = PRIVATE;
        let t = s.apply_event(1, &e, 0, 0).unwrap().toast.unwrap();
        assert_eq!((t.title.as_str(), t.body.as_str()), (PRIVATE_TOAST_NOTICE, ""));

        let mut e = event("y", Urgency::Alert);
        e.item_id = Some("secret".into());
        let t = s.apply_event(1, &e, 0, 0).unwrap().toast.unwrap();
        assert_eq!((t.title.as_str(), t.body.as_str()), (PRIVATE_TOAST_NEEDS_YOU, ""));
        assert_eq!(t.app_id, "a");

        let mut s = store_with("b", 2, PRIVATE);
        let t = s.apply_event(2, &event("z", Urgency::NeedsYou), 0, 0).unwrap().toast.unwrap();
        assert_eq!((t.title.as_str(), t.body.as_str()), (PRIVATE_TOAST_NEEDS_YOU, ""));
    }

    // --- liveness -----------------------------------------------------------------------

    #[test]
    fn missing_two_heartbeats_takes_an_app_offline_once() {
        let mut s = store_with("a", 1, PLAIN);
        s.heartbeat(1, 10_000);
        assert!(s.tick(10_000 + OFFLINE_AFTER_MS).is_empty());
        assert_eq!(s.tick(10_000 + OFFLINE_AFTER_MS + 1), vec!["a".to_string()]);
        assert!(s.tick(10_000 + OFFLINE_AFTER_MS + 2).is_empty());
        assert_eq!(s.conn_of("a"), None);

        // The silent connection no longer speaks for the app, and the id can be reclaimed.
        let err = s.apply_state(1, &one(1, "r", Urgency::Quiet), 40_000).unwrap_err();
        assert_eq!(err.0, ErrorCode::NotPermitted);
        s.register(2, "a", "a", "local", ALL, PLAIN, 40_000).unwrap();
    }

    #[test]
    fn any_frame_counts_as_contact() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(1, "r", Urgency::Quiet), 15_000).unwrap();
        s.apply_event(1, &event("e", Urgency::Quiet), 30_000, 0).unwrap();
        assert!(s.tick(30_000 + OFFLINE_AFTER_MS).is_empty());
    }

    #[test]
    fn offline_apps_report_last_seen_by_host_clock() {
        let mut s = store_with("a", 1, PLAIN);
        assert_eq!(s.snapshot(5_000).apps[0].last_seen_secs, 0);
        s.disconnect(1, 7_000);
        let v = &s.snapshot(67_999).apps[0];
        assert!(!v.online);
        assert_eq!(v.last_seen_secs, 60);
    }

    // --- roll-up and dismissal ----------------------------------------------------------

    #[test]
    fn icon_and_badge_roll_up_over_online_apps_with_volume() {
        let mut s = store_with("a", 1, PLAIN);
        s.register(2, "b", "b", "local", ALL, PLAIN, 0).unwrap();
        s.apply_state(
            1,
            &rows(
                1,
                vec![
                    row("q", Urgency::Quiet, PLAIN),
                    row("n", Urgency::Notice, PLAIN),
                    row("y", Urgency::NeedsYou, PLAIN),
                ],
            ),
            0,
        )
        .unwrap();
        let progress = StateFrame {
            rev: 1,
            doc: AppState {
                blocks: vec![Block::Progress {
                    id: "p".into(),
                    label: "copy".into(),
                    value: None,
                    eta_secs: None,
                    urgency: Urgency::Alert,
                    privacy: PLAIN,
                }],
                ..Default::default()
            },
        };
        s.apply_state(2, &progress, 0).unwrap();
        let snap = s.snapshot(0);
        assert_eq!((snap.icon_urgency, snap.badge), (Urgency::Alert, 3));

        s.set_volume("b", Volume::Off).unwrap();
        let snap = s.snapshot(0);
        assert_eq!((snap.icon_urgency, snap.badge), (Urgency::NeedsYou, 2));

        s.set_volume("b", Volume::BadgeOnly).unwrap();
        assert_eq!(s.snapshot(0).badge, 3);
    }

    #[test]
    fn offline_apps_count_as_one_notice_and_their_items_do_not() {
        let mut s = store_with("a", 1, PLAIN);
        s.register(2, "b", "b", "local", ALL, PLAIN, 0).unwrap();
        s.apply_state(
            1,
            &rows(1, vec![row("x", Urgency::Alert, PLAIN), row("y", Urgency::Alert, PLAIN)]),
            0,
        )
        .unwrap();
        s.disconnect(1, 0);
        let snap = s.snapshot(0);
        assert_eq!((snap.icon_urgency, snap.badge), (Urgency::Notice, 1));
        assert_eq!(items(&snap.apps[0].state).len(), 2, "offline state stays visible");

        s.disconnect(2, 0);
        assert_eq!(s.snapshot(0).badge, 2);
        s.set_volume("b", Volume::Off).unwrap();
        assert_eq!(s.snapshot(0).badge, 1);
        s.set_volume("a", Volume::Off).unwrap();
        let snap = s.snapshot(0);
        assert_eq!((snap.icon_urgency, snap.badge), (Urgency::Quiet, 0));
    }

    #[test]
    fn dismissal_clears_the_badge_but_not_the_level() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(1, "s1", Urgency::NeedsYou), 0).unwrap();
        let out = s.dismiss("a", "s1", "shell-1", 42).unwrap();
        assert_eq!(out.deliver_to, Some(1));
        assert_eq!(
            out.feed,
            Some(FeedEntry {
                app_id: "a".into(),
                wall: 42,
                kind: FeedKind::Dismiss { item_id: "s1".into(), shell: "shell-1".into() },
            })
        );
        let snap = s.snapshot(0);
        assert_eq!((snap.icon_urgency, snap.badge), (Urgency::NeedsYou, 0));

        // Same level again: still acknowledged.
        s.apply_state(1, &one(2, "s1", Urgency::NeedsYou), 0).unwrap();
        assert_eq!(s.snapshot(0).badge, 0);
        // Raised above the acknowledged level: counts again.
        s.apply_state(1, &one(3, "s1", Urgency::Alert), 0).unwrap();
        assert_eq!(s.snapshot(0).badge, 1);
    }

    #[test]
    fn a_lowered_then_raised_item_counts_again() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(1, "s1", Urgency::NeedsYou), 0).unwrap();
        s.dismiss("a", "s1", "shell-1", 0).unwrap();
        s.apply_state(1, &one(2, "s1", Urgency::Quiet), 0).unwrap();
        s.apply_state(1, &one(3, "s1", Urgency::NeedsYou), 0).unwrap();
        assert_eq!(s.snapshot(0).badge, 1);
    }

    #[test]
    fn a_dismissal_ends_when_the_item_disappears() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(1, "s1", Urgency::NeedsYou), 0).unwrap();
        s.dismiss("a", "s1", "shell-1", 0).unwrap();
        s.apply_state(1, &one(2, "other", Urgency::Quiet), 0).unwrap();
        s.apply_state(1, &one(3, "s1", Urgency::NeedsYou), 0).unwrap();
        assert_eq!(s.snapshot(0).badge, 1);
    }

    #[test]
    fn dismiss_is_delivered_only_when_online_and_written_only_when_not_ephemeral() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(
            1,
            &rows(
                1,
                vec![row("keep", Urgency::Notice, PLAIN), row("eph", Urgency::Notice, EPHEMERAL)],
            ),
            0,
        )
        .unwrap();
        assert_eq!(s.dismiss("a", "eph", "shell-1", 0).unwrap().feed, None);

        s.disconnect(1, 0);
        let out = s.dismiss("a", "keep", "shell-1", 0).unwrap();
        assert_eq!(out.deliver_to, None);
        assert!(out.feed.is_some());

        let mut s = store_with("c", 3, EPHEMERAL);
        s.apply_state(3, &one(1, "r", Urgency::Notice), 0).unwrap();
        let out = s.dismiss("c", "r", "shell-1", 0).unwrap();
        assert_eq!((out.deliver_to, out.feed), (Some(3), None));
    }

    #[test]
    fn dismiss_of_an_unseen_item_is_passed_on_but_not_recorded() {
        let mut s = store_with("a", 1, PLAIN);
        let out = s.dismiss("a", "nope", "shell-1", 0).unwrap();
        assert_eq!((out.deliver_to, out.feed), (Some(1), None));
        assert!(s.apps["a"].dismissed.is_empty());
        assert_eq!(s.dismiss("zz", "x", "shell-1", 0), Err(ErrorCode::UnknownApp));
        assert_eq!(s.dismiss("a", "bad id", "shell-1", 0), Err(ErrorCode::BadFrame));
    }

    #[test]
    fn dismiss_honours_the_current_connections_ephemeral_flag() {
        // Plain state kept across a rebind to an ephemeral connection: the app is now in an
        // ephemeral session, so acknowledging its items leaves no record either.
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(1, "r", Urgency::Notice), 0).unwrap();
        s.disconnect(1, 0);
        s.register(2, "a", "a", "local", ALL, EPHEMERAL, 0).unwrap();
        let out = s.dismiss("a", "r", "shell-1", 0).unwrap();
        assert_eq!((out.deliver_to, out.feed), (Some(2), None));
    }

    // --- snapshot and fanout ------------------------------------------------------------

    #[test]
    fn snapshot_lists_apps_by_name() {
        let mut s = Store::new();
        s.register(1, "z-id", "Alpha", "local", ALL, PLAIN, 0).unwrap();
        s.register(2, "a-id", "Beta", "node-x", ALL, PLAIN, 0).unwrap();
        let snap = s.snapshot(0);
        let order: Vec<_> = snap.apps.iter().map(|a| a.app_id.as_str()).collect();
        assert_eq!(order, vec!["z-id", "a-id"]);
        assert_eq!(snap.apps[1].origin, "node-x");
    }

    #[test]
    fn fanout_is_paced_and_latest_wins() {
        let mut s = Store::new();
        assert!(s.take_fanout(0).is_none(), "nothing changed");
        s.register(1, "a", "a", "local", ALL, PLAIN, 0).unwrap();
        assert!(s.take_fanout(0).is_some());
        assert!(s.take_fanout(1).is_none(), "not dirty");

        s.apply_state(1, &one(1, "r", Urgency::Notice), 10).unwrap();
        assert!(s.take_fanout(FANOUT_INTERVAL_MS - 1).is_none(), "too soon");
        s.apply_state(1, &one(2, "r", Urgency::Alert), 20).unwrap();
        let snap = s.take_fanout(FANOUT_INTERVAL_MS).expect("interval elapsed");
        assert_eq!((snap.apps[0].rev, snap.icon_urgency), (2, Urgency::Alert));
        assert!(s.take_fanout(FANOUT_INTERVAL_MS * 10).is_none());
    }

    #[test]
    fn stale_frames_heartbeats_and_events_do_not_trigger_fanout() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &one(5, "r", Urgency::Quiet), 0).unwrap();
        s.take_fanout(0).unwrap();
        s.apply_state(1, &one(5, "r", Urgency::Alert), 1000).unwrap();
        s.heartbeat(1, 1000);
        s.apply_event(1, &event("e", Urgency::Alert), 1000, 0).unwrap();
        assert!(s.take_fanout(1000).is_none());
        s.set_mute(true);
        assert!(s.take_fanout(1000).unwrap().muted);
    }

    #[test]
    fn removed_apps_are_gone_and_their_connection_is_refused() {
        let mut s = store_with("a", 1, PLAIN);
        assert_eq!(s.remove_app("a"), Ok(Some(1)));
        assert!(s.snapshot(0).apps.is_empty());
        let err = s.apply_state(1, &one(1, "r", Urgency::Quiet), 0).unwrap_err();
        assert_eq!(err.0, ErrorCode::NotPermitted);
        assert_eq!(s.remove_app("a"), Err(ErrorCode::UnknownApp));
        assert_eq!(s.set_volume("a", Volume::Off), Err(ErrorCode::UnknownApp));
    }

    // --- persistence --------------------------------------------------------------------

    #[test]
    fn ephemeral_items_reach_shells_flagged_and_the_store_never_writes() {
        let mut s = store_with("a", 1, PLAIN);
        s.apply_state(1, &rows(1, vec![row("eph", Urgency::Notice, EPHEMERAL)]), 0).unwrap();
        assert!(s.holds_ephemeral());
        // The snapshot goes to shells over the socket; it must carry the flag so they honour it.
        let json = serde_json::to_string(&s.snapshot(0)).unwrap();
        assert!(json.contains(r#""ephemeral":true"#), "{json}");

        // The store's only output toward disk is FeedEntry values. Check that the non-test
        // part of this file has no way to write anything itself.
        let source = include_str!("store.rs");
        let (code, _) = source.split_once("#[cfg(test)]").unwrap();
        for needle in [
            "std::fs",
            "tokio::fs",
            "File",
            "io::Write",
            "OpenOptions",
            "write!",
            "println!",
            "eprintln!",
        ] {
            assert!(!code.contains(needle), "store.rs must not use {needle}");
        }
    }
}
