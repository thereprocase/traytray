//! Pairing state machine and token authentication. See docs/design.md, "Pairing".
//!
//! Pure: time and randomness come from the caller, and persistence is the caller's job via
//! serde on `PairRecord`. The only secrets handled here are the 6-digit code and the bearer
//! token; the token is returned once in `Granted` and only its SHA-256 is ever stored.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use traytray_proto::limits::MAX_NAME_CHARS;
use traytray_proto::sanitize::{clean, valid_id};
use traytray_proto::{ErrorCode, PairOutcome, PairPending, PairRequest, Tier};

use crate::{Millis, UnixMillis};

/// How long the user-opened window accepts `pair_request`.
pub const WINDOW_MS: Millis = 2 * 60 * 1000;
/// How long an accepted code stays valid, and how long an unaccepted request may wait.
pub const CODE_TTL_MS: Millis = 2 * 60 * 1000;
/// Rate-limit horizon and budgets.
pub const RATE_WINDOW_MS: Millis = 10 * 60 * 1000;
pub const MAX_REQUESTS_PER_NODE: usize = 5;
pub const MAX_REQUESTS_GLOBAL: usize = 20;
pub const MAX_CODE_ATTEMPTS: u8 = 3;
pub const TOKEN_PREFIX: &str = "tt1_";

/// Hard cap on remembered attempts; it only bounds memory. When full, the oldest entry is
/// evicted, never the newest: a full history still holds at least `MAX_REQUESTS_GLOBAL`
/// in-horizon attempts, so every request is refused, and the evicted entry is older than all
/// kept ones, so the budget returns only after the most recent attempts age out.
const MAX_HISTORY: usize = 256;
const _: () = assert!(MAX_HISTORY >= MAX_REQUESTS_GLOBAL);
/// Rejection sampling gives up after this many draws per digit, so a broken or constant
/// random source fails loudly instead of hanging the daemon.
const MAX_DRAWS_PER_DIGIT: usize = 256;

/// The Tailscale `whois` result for the TCP peer. Verified, unlike anything in a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub stable_node_id: String,
    pub node_name: String,
    pub login: String,
}

/// What is persisted per paired app. The token itself is never a field here.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PairRecord {
    /// `<claimed_app_id>@<stable_node_id>`.
    pub app_id: String,
    /// Lowercase hex SHA-256 of the full token string.
    pub token_sha256: String,
    pub stable_node_id: String,
    pub login: String,
    pub display_name: String,
    pub tiers: Vec<Tier>,
    pub paired_at: UnixMillis,
}

/// Returned once on success. `Debug` is redacted so the token cannot reach a log by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Granted {
    pub app_id: String,
    pub token: String,
}

impl std::fmt::Debug for Granted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Granted")
            .field("app_id", &self.app_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Failure of `submit_code`. `outcome` is set when the pending request was ended by this
/// call, so the caller can send `PairDone` to the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitError {
    pub code: ErrorCode,
    pub outcome: Option<PairOutcome>,
}

impl SubmitError {
    fn plain(code: ErrorCode) -> Self {
        Self {
            code,
            outcome: None,
        }
    }
}

#[derive(Debug, Clone)]
struct Accepted {
    code: [u8; 6],
    display_name: String,
    tiers: Vec<Tier>,
}

#[derive(Debug, Clone)]
struct Pending {
    request_id: String,
    peer: PeerIdentity,
    claimed_app_id: String,
    /// Before accept: when the request times out. After accept: when the code expires.
    deadline: Millis,
    failures: u8,
    accepted: Option<Accepted>,
}

#[derive(Debug, Default)]
pub struct Pairing {
    window_until: Option<Millis>,
    pending: Option<Pending>,
    records: Vec<PairRecord>,
    /// (when, stable_node_id) of every counted request.
    history: VecDeque<(Millis, String)>,
}

impl Pairing {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_records(records: Vec<PairRecord>) -> Self {
        Self {
            records,
            ..Self::default()
        }
    }

    pub fn records(&self) -> &[PairRecord] {
        &self.records
    }

    pub fn open_window(&mut self, now: Millis) {
        self.window_until = Some(now.saturating_add(WINDOW_MS));
    }

    /// Closing also drops a pending request: it was started under a window the user has
    /// now withdrawn. Returns the dropped request id so the caller can notify both sides.
    pub fn close_window(&mut self) -> Option<String> {
        self.window_until = None;
        self.pending.take().map(|p| p.request_id)
    }

    pub fn window_open(&self, now: Millis) -> bool {
        self.window_until.is_some_and(|until| now < until)
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Time out a pending request or code. Call from the host's tick.
    pub fn expire(&mut self, now: Millis) -> Option<(String, PairOutcome)> {
        if self.pending.as_ref().is_some_and(|p| now >= p.deadline) {
            return self
                .pending
                .take()
                .map(|p| (p.request_id, PairOutcome::Expired));
        }
        None
    }

    pub fn request(
        &mut self,
        now: Millis,
        peer: &PeerIdentity,
        req: PairRequest,
        request_id: String,
    ) -> Result<PairPending, ErrorCode> {
        if !self.window_open(now) {
            // Not counted: otherwise a peer could spend the global budget before the user
            // ever opens a window.
            return Err(ErrorCode::PairingClosed);
        }
        if peer.stable_node_id.is_empty() || !valid_id(&request_id) {
            return Err(ErrorCode::NotPermitted);
        }

        // Counted before every other check, so refused attempts still use up the budget.
        if !self.record_attempt(now, &peer.stable_node_id) {
            return Err(ErrorCode::RateLimited);
        }
        if self.pending.is_some() {
            return Err(ErrorCode::PairingBusy);
        }
        // '@' is the namespace separator; allowing it would let one node claim an id that
        // parses as another node's namespaced id.
        if !valid_id(&req.app_id) || req.app_id.contains('@') {
            return Err(ErrorCode::BadFrame);
        }
        let claimed_name = clean_name(&req.name);
        if claimed_name.is_empty() {
            return Err(ErrorCode::BadFrame);
        }

        let view = PairPending {
            request_id: request_id.clone(),
            claimed_name,
            claimed_app_id: req.app_id.clone(),
            node_name: clean_name(&peer.node_name),
            login: clean_name(&peer.login),
            requested_tiers: dedup(req.requested_tiers),
        };
        self.pending = Some(Pending {
            request_id,
            peer: peer.clone(),
            claimed_app_id: req.app_id,
            deadline: now.saturating_add(CODE_TTL_MS),
            failures: 0,
            accepted: None,
        });
        Ok(view)
    }

    /// Returns false when this attempt exceeds the per-node or global budget.
    fn record_attempt(&mut self, now: Millis, node: &str) -> bool {
        self.history
            .retain(|(t, _)| now.saturating_sub(*t) < RATE_WINDOW_MS);
        let per_node = self.history.iter().filter(|(_, n)| n == node).count();
        let global = self.history.len();
        if self.history.len() >= MAX_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back((now, node.to_owned()));
        per_node < MAX_REQUESTS_PER_NODE && global < MAX_REQUESTS_GLOBAL
    }

    /// The user accepted: generate the code. `random` yields uniformly random bytes.
    pub fn accept(
        &mut self,
        request_id: &str,
        display_name: &str,
        tiers: Vec<Tier>,
        now: Millis,
        random: impl FnMut() -> u8,
    ) -> Result<(String, u64), ErrorCode> {
        let pending = match self.pending.as_mut() {
            Some(p) if p.request_id == request_id => p,
            _ => return Err(ErrorCode::BadFrame),
        };
        if now >= pending.deadline {
            self.pending = None;
            return Err(ErrorCode::CodeExpired);
        }
        // A second accept would mint a new code for a request the user already answered.
        if pending.accepted.is_some() {
            return Err(ErrorCode::BadFrame);
        }
        let display_name = clean_name(display_name);
        if display_name.is_empty() {
            return Err(ErrorCode::BadFrame);
        }
        let code = generate_code(random).ok_or(ErrorCode::BadFrame)?;
        pending.accepted = Some(Accepted {
            code,
            display_name,
            tiers: dedup(tiers),
        });
        pending.deadline = now.saturating_add(CODE_TTL_MS);
        Ok((code_string(&code), CODE_TTL_MS / 1000))
    }

    pub fn reject(&mut self, request_id: &str) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.request_id == request_id)
        {
            self.pending = None;
            return true;
        }
        false
    }

    pub fn submit_code(
        &mut self,
        now: Millis,
        peer: &PeerIdentity,
        request_id: &str,
        code: &str,
        token_bytes: [u8; 32],
        wall: UnixMillis,
    ) -> Result<Granted, SubmitError> {
        let Some(pending) = self.pending.as_mut() else {
            return Err(SubmitError::plain(ErrorCode::BadCode));
        };
        // Unrelated ids don't count: only someone who holds the real request id can burn
        // attempts, and an id-less stranger must not be able to lock out the real requester.
        if pending.request_id != request_id {
            return Err(SubmitError::plain(ErrorCode::BadCode));
        }
        if now >= pending.deadline {
            self.pending = None;
            return Err(SubmitError {
                code: ErrorCode::CodeExpired,
                outcome: Some(PairOutcome::Expired),
            });
        }

        let node_ok = ct_eq(
            peer.stable_node_id.as_bytes(),
            pending.peer.stable_node_id.as_bytes(),
        );
        // Before accept there is no code to match, but the attempt still counts so a
        // requester cannot probe the state for free.
        let code_ok = match &pending.accepted {
            Some(a) => ct_eq(&a.code.map(|d| b'0' + d), code.as_bytes()),
            None => false,
        };
        if !(node_ok && code_ok) {
            pending.failures += 1;
            let wrong = if node_ok {
                ErrorCode::BadCode
            } else {
                ErrorCode::NodeMismatch
            };
            if pending.failures >= MAX_CODE_ATTEMPTS {
                self.pending = None;
                return Err(SubmitError {
                    code: wrong,
                    outcome: Some(PairOutcome::LockedOut),
                });
            }
            return Err(SubmitError::plain(wrong));
        }

        let pending = self.pending.take().expect("checked above");
        let accepted = pending.accepted.expect("code_ok implies accepted");
        let token = format!("{TOKEN_PREFIX}{}", hex(&token_bytes));
        let app_id = format!("{}@{}", pending.claimed_app_id, pending.peer.stable_node_id);
        self.records.retain(|r| r.app_id != app_id);
        self.records.push(PairRecord {
            app_id: app_id.clone(),
            token_sha256: token_hash(&token),
            stable_node_id: pending.peer.stable_node_id,
            login: pending.peer.login,
            display_name: accepted.display_name,
            tiers: accepted.tiers,
            paired_at: wall,
        });
        self.window_until = None;
        Ok(Granted { app_id, token })
    }

    pub fn authenticate(&self, token: &str, peer: &PeerIdentity) -> Result<&PairRecord, ErrorCode> {
        let presented = token_hash(token);
        // Visit every record without early exit so timing does not reveal which hash matched.
        let mut found: Option<&PairRecord> = None;
        for r in &self.records {
            if ct_eq(r.token_sha256.as_bytes(), presented.as_bytes()) {
                found = Some(r);
            }
        }
        let record = found.ok_or(ErrorCode::TokenRevoked)?;
        // A stolen token is useless from another node or another account.
        if record.stable_node_id != peer.stable_node_id || record.login != peer.login {
            return Err(ErrorCode::NodeMismatch);
        }
        Ok(record)
    }

    pub fn revoke(&mut self, app_id: &str) -> bool {
        let before = self.records.len();
        self.records.retain(|r| r.app_id != app_id);
        self.records.len() != before
    }
}

/// Cleaned for display, and trimmed so a newline can't leave a trailing space that makes two
/// names look identical to the eye but not to the host.
fn clean_name(s: &str) -> String {
    clean(s, MAX_NAME_CHARS, false).trim().to_owned()
}

fn dedup(tiers: Vec<Tier>) -> Vec<Tier> {
    let mut out = Vec::with_capacity(tiers.len().min(3));
    for t in tiers {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Six decimal digits by rejection sampling: bytes 250..=255 are discarded because 256 is not
/// a multiple of 10 and `b % 10` would favour 0..=5.
fn generate_code(mut random: impl FnMut() -> u8) -> Option<[u8; 6]> {
    let mut digits = [0u8; 6];
    for slot in &mut digits {
        *slot = (0..MAX_DRAWS_PER_DIGIT)
            .map(|_| random())
            .find(|b| *b < 250)?
            % 10;
    }
    Some(digits)
}

fn code_string(digits: &[u8; 6]) -> String {
    digits.iter().map(|d| char::from(b'0' + d)).collect()
}

/// Compares without an early exit on the first differing byte. Length differences are folded
/// into the result rather than returned early.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= u64::from(x ^ y);
    }
    diff == 0
}

fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Millis = 1_000_000;

    fn peer(node: &str) -> PeerIdentity {
        PeerIdentity {
            stable_node_id: node.into(),
            node_name: format!("{node}-host"),
            login: "user@example".into(),
        }
    }

    fn req(app: &str) -> PairRequest {
        PairRequest {
            app_id: app.into(),
            name: "My App".into(),
            requested_tiers: vec![Tier::Menu],
        }
    }

    fn fixed(digits: &'static [u8]) -> impl FnMut() -> u8 {
        let mut i = 0;
        move || {
            let b = digits[i % digits.len()];
            i += 1;
            b
        }
    }

    /// Window open, request "r1" from node "n1" pending.
    fn pending() -> Pairing {
        let mut p = Pairing::new();
        p.open_window(T0);
        p.request(T0, &peer("n1"), req("app"), "r1".into()).unwrap();
        p
    }

    /// As `pending`, and accepted with code 123456.
    fn accepted() -> Pairing {
        let mut p = pending();
        let (code, secs) = p
            .accept(
                "r1",
                "Shown",
                vec![Tier::Menu],
                T0,
                fixed(&[1, 2, 3, 4, 5, 6]),
            )
            .unwrap();
        assert_eq!((code.as_str(), secs), ("123456", 120));
        p
    }

    fn submit(
        p: &mut Pairing,
        now: Millis,
        node: &str,
        code: &str,
    ) -> Result<Granted, SubmitError> {
        p.submit_code(now, &peer(node), "r1", code, [7; 32], 42)
    }

    fn request_err(p: &mut Pairing, node: &str, id: &str) -> ErrorCode {
        p.request(T0, &peer(node), req("a"), id.into()).unwrap_err()
    }

    #[test]
    fn request_refused_outside_window() {
        let mut p = Pairing::new();
        assert_eq!(request_err(&mut p, "n1", "r"), ErrorCode::PairingClosed);
        p.open_window(T0);
        assert!(p
            .request(T0 + WINDOW_MS - 1, &peer("n1"), req("a"), "r".into())
            .is_ok());
    }

    #[test]
    fn window_expires_after_two_minutes() {
        let mut p = Pairing::new();
        p.open_window(T0);
        assert_eq!(WINDOW_MS, 120_000);
        let late = p.request(T0 + WINDOW_MS, &peer("n1"), req("a"), "r".into());
        assert_eq!(late.unwrap_err(), ErrorCode::PairingClosed);
    }

    #[test]
    fn close_window_refuses_and_drops_pending() {
        let mut p = pending();
        assert_eq!(p.close_window().as_deref(), Some("r1"));
        assert!(!p.has_pending());
        assert_eq!(request_err(&mut p, "n2", "r2"), ErrorCode::PairingClosed);
    }

    #[test]
    fn closed_window_requests_do_not_spend_rate_budget() {
        let mut p = Pairing::new();
        for _ in 0..50 {
            let _ = p.request(T0, &peer("n1"), req("a"), "r".into());
        }
        p.open_window(T0);
        assert!(p.request(T0, &peer("n1"), req("a"), "r".into()).is_ok());
    }

    #[test]
    fn second_request_while_pending_is_busy() {
        let mut p = pending();
        assert_eq!(request_err(&mut p, "n2", "r2"), ErrorCode::PairingBusy);
    }

    #[test]
    fn per_node_rate_limit_counts_refused_attempts() {
        let mut p = pending();
        // Attempts 2..=5 are refused as busy but still count; the 6th is rate limited.
        for i in 0..4 {
            assert_eq!(
                request_err(&mut p, "n1", &format!("x{i}")),
                ErrorCode::PairingBusy
            );
        }
        assert_eq!(request_err(&mut p, "n1", "x"), ErrorCode::RateLimited);
        // Another node is unaffected by n1's budget.
        assert_eq!(request_err(&mut p, "n2", "y"), ErrorCode::PairingBusy);
    }

    #[test]
    fn invalid_requests_also_count() {
        let mut p = Pairing::new();
        p.open_window(T0);
        for _ in 0..5 {
            assert_eq!(
                p.request(T0, &peer("n1"), req("bad@id"), "r".into())
                    .unwrap_err(),
                ErrorCode::BadFrame
            );
        }
        assert_eq!(request_err(&mut p, "n1", "r"), ErrorCode::RateLimited);
    }

    #[test]
    fn rate_limit_recovers_after_ten_minutes() {
        let mut p = Pairing::new();
        for i in 0..5 {
            p.open_window(T0);
            let _ = p.request(T0, &peer("n1"), req("a"), format!("x{i}"));
            p.close_window();
        }
        let later = T0 + RATE_WINDOW_MS;
        p.open_window(later);
        assert!(p.request(later, &peer("n1"), req("a"), "z".into()).is_ok());
    }

    #[test]
    fn global_rate_limit_across_nodes() {
        let mut p = pending();
        for i in 0..19 {
            assert_eq!(
                request_err(&mut p, &format!("node{i}"), "x"),
                ErrorCode::PairingBusy
            );
        }
        // 20 attempts so far; the 21st from a fresh node hits the global budget.
        assert_eq!(request_err(&mut p, "fresh", "x"), ErrorCode::RateLimited);
    }

    #[test]
    fn history_is_bounded() {
        let mut p = pending();
        for i in 0..1000 {
            let _ = p.request(T0, &peer(&format!("n{i}")), req("a"), "x".into());
        }
        assert!(p.history.len() <= MAX_HISTORY);
    }

    #[test]
    fn full_history_keeps_newest_attempts() {
        let mut p = Pairing::new();
        p.open_window(T0);
        for i in 0..MAX_HISTORY {
            let _ = p.request(T0, &peer("evil"), req("a"), format!("a{i}"));
        }
        // Hammer just before the first burst ages out; these must outlive that burst.
        let hammer = T0 + RATE_WINDOW_MS - 1000;
        p.close_window();
        p.open_window(hammer);
        for i in 0..1000 {
            let _ = p.request(hammer, &peer("evil"), req("a"), format!("b{i}"));
        }
        let after_first_burst = T0 + RATE_WINDOW_MS;
        p.close_window();
        p.open_window(after_first_burst);
        let again = p.request(after_first_burst, &peer("evil"), req("a"), "c".into());
        assert_eq!(again.unwrap_err(), ErrorCode::RateLimited);
        let other = p.request(after_first_burst, &peer("fresh"), req("a"), "d".into());
        assert_eq!(other.unwrap_err(), ErrorCode::RateLimited);
        // The budget does return once the hammering itself ages out.
        let recovered = hammer + RATE_WINDOW_MS;
        p.open_window(recovered);
        assert!(p
            .request(recovered, &peer("evil"), req("a"), "e".into())
            .is_ok());
    }

    #[test]
    fn claimed_app_id_must_be_valid_and_without_at() {
        for bad in ["", "has space", "a@b", "@", "ctl\u{1b}x", &"x".repeat(129)] {
            let mut p = Pairing::new();
            p.open_window(T0);
            let got = p.request(T0, &peer("n1"), req(bad), "r".into());
            assert_eq!(got.unwrap_err(), ErrorCode::BadFrame, "{bad:?}");
            assert!(!p.has_pending());
        }
    }

    #[test]
    fn claimed_name_is_cleaned() {
        let mut p = Pairing::new();
        p.open_window(T0);
        let mut r = req("a");
        r.name = "\u{1b}[31mEvil\u{202E}Name\n".into();
        let view = p.request(T0, &peer("n1"), r, "r".into()).unwrap();
        assert_eq!(view.claimed_name, "EvilName");
        assert_eq!(view.node_name, "n1-host");

        let mut r = req("a");
        r.name = "\u{1b}[0m".into();
        let mut p = Pairing::new();
        p.open_window(T0);
        assert_eq!(
            p.request(T0, &peer("n1"), r, "r".into()).unwrap_err(),
            ErrorCode::BadFrame
        );
    }

    #[test]
    fn code_before_accept_is_impossible_and_counts() {
        let mut p = pending();
        for code in ["000000", "123456"] {
            assert_eq!(
                submit(&mut p, T0, "n1", code).unwrap_err().code,
                ErrorCode::BadCode
            );
        }
        let last = submit(&mut p, T0, "n1", "").unwrap_err();
        assert_eq!(last.outcome, Some(PairOutcome::LockedOut));
        assert!(p.records().is_empty());
    }

    #[test]
    fn accept_is_single_shot_and_bound_to_request() {
        let mut p = pending();
        let err = p.accept("other", "N", vec![], T0, fixed(&[1])).unwrap_err();
        assert_eq!(err, ErrorCode::BadFrame);
        p.accept("r1", "N", vec![], T0, fixed(&[1])).unwrap();
        let err = p.accept("r1", "N", vec![], T0, fixed(&[2])).unwrap_err();
        assert_eq!(err, ErrorCode::BadFrame);
    }

    #[test]
    fn accept_rejects_empty_name_and_broken_rng() {
        let mut p = pending();
        let err = p
            .accept("r1", "\u{1b}[0m ", vec![], T0, fixed(&[1]))
            .unwrap_err();
        assert_eq!(err, ErrorCode::BadFrame);
        let err = p.accept("r1", "N", vec![], T0, fixed(&[255])).unwrap_err();
        assert_eq!(err, ErrorCode::BadFrame);
        assert!(p.accept("r1", "N", vec![], T0, fixed(&[9])).is_ok());
    }

    #[test]
    fn reject_clears_pending() {
        let mut p = pending();
        assert!(!p.reject("nope"));
        assert!(p.has_pending());
        assert!(p.reject("r1"));
        assert!(!p.has_pending());
    }

    #[test]
    fn correct_code_grants_token_and_closes_window() {
        let mut p = accepted();
        let g = submit(&mut p, T0 + 1000, "n1", "123456").unwrap();
        assert_eq!(g.app_id, "app@n1");
        assert_eq!(g.token, format!("tt1_{}", "07".repeat(32)));
        assert!(!p.has_pending());
        assert!(!p.window_open(T0 + 1000));
        let r = &p.records()[0];
        assert_eq!(r.display_name, "Shown");
        assert_eq!((r.paired_at, r.tiers.clone()), (42, vec![Tier::Menu]));
        // Code is single-use.
        assert_eq!(
            submit(&mut p, T0 + 1000, "n1", "123456").unwrap_err().code,
            ErrorCode::BadCode
        );
    }

    #[test]
    fn three_wrong_codes_lock_out() {
        let mut p = accepted();
        for _ in 0..2 {
            let e = submit(&mut p, T0, "n1", "000000").unwrap_err();
            assert_eq!((e.code, e.outcome), (ErrorCode::BadCode, None));
        }
        let e = submit(&mut p, T0, "n1", "000000").unwrap_err();
        assert_eq!(
            (e.code, e.outcome),
            (ErrorCode::BadCode, Some(PairOutcome::LockedOut))
        );
        // The correct code no longer works.
        assert_eq!(
            submit(&mut p, T0, "n1", "123456").unwrap_err().code,
            ErrorCode::BadCode
        );
        assert!(p.records().is_empty());
    }

    #[test]
    fn malformed_codes_are_wrong_not_panics() {
        for code in ["12345", "1234567", "12345\u{0}", "１２３４５６", ""] {
            let mut p = accepted();
            assert_eq!(
                submit(&mut p, T0, "n1", code).unwrap_err().code,
                ErrorCode::BadCode
            );
        }
    }

    #[test]
    fn other_node_gets_node_mismatch_and_it_counts() {
        let mut p = accepted();
        let e = submit(&mut p, T0, "evil", "123456").unwrap_err();
        assert_eq!(e.code, ErrorCode::NodeMismatch);
        // The right code from the wrong node never succeeds, and two more failures lock out.
        submit(&mut p, T0, "evil", "123456").unwrap_err();
        let e = submit(&mut p, T0, "evil", "123456").unwrap_err();
        assert_eq!(
            (e.code, e.outcome),
            (ErrorCode::NodeMismatch, Some(PairOutcome::LockedOut))
        );
        assert!(!p.has_pending());
    }

    #[test]
    fn wrong_or_missing_request_id_is_bad_code_and_free() {
        let mut p = accepted();
        for _ in 0..10 {
            let e = p
                .submit_code(T0, &peer("n1"), "guess", "123456", [1; 32], 0)
                .unwrap_err();
            assert_eq!((e.code, e.outcome), (ErrorCode::BadCode, None));
        }
        assert!(submit(&mut p, T0, "n1", "123456").is_ok());
        // Nothing pending any more.
        assert_eq!(
            submit(&mut p, T0, "n1", "123456").unwrap_err().code,
            ErrorCode::BadCode
        );
    }

    #[test]
    fn expired_code_clears_pending() {
        let mut p = accepted();
        let e = submit(&mut p, T0 + CODE_TTL_MS, "n1", "123456").unwrap_err();
        assert_eq!(
            (e.code, e.outcome),
            (ErrorCode::CodeExpired, Some(PairOutcome::Expired))
        );
        assert!(!p.has_pending());
    }

    #[test]
    fn code_ttl_runs_from_accept_not_request() {
        let mut p = pending();
        let accept_at = T0 + CODE_TTL_MS - 1;
        p.accept("r1", "N", vec![], accept_at, fixed(&[1])).unwrap();
        assert!(submit(&mut p, accept_at + CODE_TTL_MS - 1, "n1", "111111").is_ok());
    }

    #[test]
    fn accept_after_request_timeout_fails() {
        let mut p = pending();
        let e = p
            .accept("r1", "N", vec![], T0 + CODE_TTL_MS, fixed(&[1]))
            .unwrap_err();
        assert_eq!(e, ErrorCode::CodeExpired);
        assert!(!p.has_pending());
    }

    #[test]
    fn expire_times_out_request_and_code() {
        let mut p = pending();
        assert_eq!(p.expire(T0 + CODE_TTL_MS - 1), None);
        assert_eq!(
            p.expire(T0 + CODE_TTL_MS),
            Some(("r1".into(), PairOutcome::Expired))
        );
        assert_eq!(p.expire(T0 + CODE_TTL_MS), None);
        let mut p = accepted();
        assert_eq!(
            p.expire(T0 + CODE_TTL_MS),
            Some(("r1".into(), PairOutcome::Expired))
        );
    }

    fn paired() -> (Pairing, String) {
        let mut p = accepted();
        let g = submit(&mut p, T0, "n1", "123456").unwrap();
        (p, g.token)
    }

    #[test]
    fn token_is_never_stored_in_plaintext() {
        let (p, token) = paired();
        let json = serde_json::to_string(p.records()).unwrap();
        assert!(!json.contains(&token));
        assert!(!json.contains(&token[TOKEN_PREFIX.len()..]));
        assert!(json.contains(&token_hash(&token)));
        assert_eq!(p.records()[0].token_sha256.len(), 64);
    }

    #[test]
    fn granted_debug_redacts_token() {
        let (_, token) = paired();
        let g = Granted {
            app_id: "a".into(),
            token: token.clone(),
        };
        assert!(!format!("{g:?}").contains(&token));
    }

    #[test]
    fn authenticate_accepts_matching_peer_and_survives_persistence() {
        let (p, token) = paired();
        let json = serde_json::to_string(p.records()).unwrap();
        let restored = Pairing::from_records(serde_json::from_str(&json).unwrap());
        let r = restored.authenticate(&token, &peer("n1")).unwrap();
        assert_eq!(r.app_id, "app@n1");
    }

    #[test]
    fn replayed_token_from_other_node_or_login_is_node_mismatch() {
        let (p, token) = paired();
        assert_eq!(
            p.authenticate(&token, &peer("n2")).unwrap_err(),
            ErrorCode::NodeMismatch
        );
        let mut other_login = peer("n1");
        other_login.login = "someone@else".into();
        assert_eq!(
            p.authenticate(&token, &other_login).unwrap_err(),
            ErrorCode::NodeMismatch
        );
    }

    #[test]
    fn unknown_or_malformed_token_is_revoked() {
        let (p, token) = paired();
        for t in [
            "",
            "tt1_",
            "garbage",
            &token[..token.len() - 1],
            &token.to_uppercase(),
        ] {
            let got = p.authenticate(t, &peer("n1"));
            assert_eq!(got.unwrap_err(), ErrorCode::TokenRevoked, "{t}");
        }
        // The stored hash is not itself a credential.
        let hash = p.records()[0].token_sha256.clone();
        assert_eq!(
            p.authenticate(&hash, &peer("n1")).unwrap_err(),
            ErrorCode::TokenRevoked
        );
    }

    #[test]
    fn revoke_invalidates_token() {
        let (mut p, token) = paired();
        assert!(!p.revoke("nope@n1"));
        assert!(p.revoke("app@n1"));
        assert!(!p.revoke("app@n1"));
        assert_eq!(
            p.authenticate(&token, &peer("n1")).unwrap_err(),
            ErrorCode::TokenRevoked
        );
    }

    #[test]
    fn repairing_replaces_record_and_old_token_dies() {
        let (mut p, old) = paired();
        p.open_window(T0);
        p.request(T0, &peer("n1"), req("app"), "r1".into()).unwrap();
        p.accept("r1", "Again", vec![], T0, fixed(&[9])).unwrap();
        let g = p
            .submit_code(T0, &peer("n1"), "r1", "999999", [8; 32], 43)
            .unwrap();
        assert_eq!(p.records().len(), 1);
        assert_eq!(
            p.authenticate(&old, &peer("n1")).unwrap_err(),
            ErrorCode::TokenRevoked
        );
        assert!(p.authenticate(&g.token, &peer("n1")).is_ok());
    }

    #[test]
    fn same_claimed_id_on_different_nodes_does_not_collide() {
        let (mut p, t1) = paired();
        p.open_window(T0);
        p.request(T0, &peer("n2"), req("app"), "r1".into()).unwrap();
        p.accept("r1", "Other", vec![], T0, fixed(&[3])).unwrap();
        let g = p
            .submit_code(T0, &peer("n2"), "r1", "333333", [9; 32], 0)
            .unwrap();
        assert_eq!(g.app_id, "app@n2");
        assert_eq!(p.records().len(), 2);
        assert!(p.authenticate(&t1, &peer("n1")).is_ok());
    }

    #[test]
    fn code_generation_is_unbiased() {
        // Feed every byte value equally often. A biased `b % 10` would give digits 0..=5
        // a larger share than 6..=9; rejection sampling must keep all ten within edge noise.
        let mut counts = [0usize; 10];
        let mut byte = 0u8;
        for _ in 0..25_000 {
            let code = generate_code(|| {
                let b = byte;
                byte = byte.wrapping_add(1);
                b
            })
            .unwrap();
            for d in code {
                counts[usize::from(d)] += 1;
            }
        }
        let min = *counts.iter().min().unwrap();
        let max = *counts.iter().max().unwrap();
        assert!(max - min <= 6, "{counts:?}");
    }

    #[test]
    fn rejection_discards_biased_range() {
        // 250..=255 must be skipped, so the first accepted byte here is 3.
        assert_eq!(generate_code(fixed(&[255, 250, 253, 3])).unwrap(), [3; 6]);
        assert_eq!(generate_code(fixed(&[249])).unwrap(), [9; 6]);
    }

    #[test]
    fn constant_time_compare_is_correct() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"123456", b"123456"));
        assert!(!ct_eq(b"123456", b"123457"));
        assert!(!ct_eq(b"123456", b"023456"));
        assert!(!ct_eq(b"123456", b"12345"));
        assert!(!ct_eq(b"12345", b"123456"));
        assert!(!ct_eq(b"123456", b"123456\0"));
        assert!(!ct_eq(b"", b"\0"));
    }
}
