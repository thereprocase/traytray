//! Everything traytrayd writes to disk. See docs/design.md, "Persistence contract".
//!
//! This module is the only code in the core that touches the filesystem for writing, and it
//! writes exactly three kinds of file into one state directory:
//!
//! - `feed-<name>.jsonl`: one per app, the short feed of non-ephemeral events and dismisses;
//! - `pairings.json`: pairing records (token hashes only; that rule belongs to `pairing`);
//! - `settings.json`: host settings (mute and per-app volume).
//!
//! Keeping every write in one place is what makes the ephemeral audit tractable: if a file
//! name is not produced here, traytrayd did not write it. `classify` and `list_files` exist so
//! the audit can check that claim against a real directory.
//!
//! The only other names that ever appear are this module's own short-lived temp files, which
//! start with `TEMP_PREFIX`, are removed on every failure path, and are swept on `open` in
//! case a crash left one behind.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use traytray_proto::framing;
use traytray_proto::limits::FEED_EVENTS_PER_APP;
use traytray_proto::{Urgency, Volume};

use crate::UnixMillis;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

pub const PAIRINGS_FILE: &str = "pairings.json";
pub const SETTINGS_FILE: &str = "settings.json";
const FEED_PREFIX: &str = "feed-";
const FEED_SUFFIX: &str = ".jsonl";
/// Marks feed names that carry a hash instead of the hex app id. `s`, `h` and `-` never occur
/// in lowercase hex, so the two naming schemes cannot collide.
const FEED_HASHED_MARK: &str = "sha256-";
/// The file name limit of common filesystems (NAME_MAX on ext4, btrfs and tmpfs; every name
/// here is ASCII, so NTFS's UTF-16 count is the same). Every name this module creates must
/// fit, temp names included, or writes fail for some app ids and never for others.
const MAX_NAME_BYTES: usize = 255;
/// Longest app id, in bytes, that is spelled out in hex. 120 bytes is 240 hex digits, which
/// with the prefix and suffix stays within `MAX_NAME_BYTES`. Longer ids (remote ids are
/// `<app>@<node>`) are named by their SHA-256 instead.
const MAX_HEX_ID_BYTES: usize = 120;

/// Every temp file this module creates starts with this, so a leftover is recognisable as
/// ours, safe to delete, and never mistaken for one of the three contract files.
pub const TEMP_PREFIX: &str = ".traytray-tmp.";

/// A temp file name. It deliberately does not contain the target's name: a feed name can
/// already be close to `MAX_NAME_BYTES`, and a temp name built on top of it would not fit,
/// so every compaction of that feed would fail while the file kept growing.
fn temp_file_name(pid: u32, seq: u64) -> String {
    format!("{TEMP_PREFIX}{pid}.{seq}")
}

// Raising MAX_HEX_ID_BYTES or lengthening a prefix must not quietly push a name past the
// limit; these fail the build instead. 10 and 20 are the most digits a u32 and a u64 take.
const _: () = assert!(
    FEED_PREFIX.len() + 2 * MAX_HEX_ID_BYTES + FEED_SUFFIX.len() <= MAX_NAME_BYTES,
    "hex feed names must fit MAX_NAME_BYTES"
);
const _: () = assert!(
    TEMP_PREFIX.len() + 10 + 1 + 20 <= MAX_NAME_BYTES,
    "temp file names must fit MAX_NAME_BYTES"
);

#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// Uniqueness for temp names within this process. Not randomness: `create_new` is what
/// guarantees a temp file is never shared, this only makes collisions rare.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------------------

/// One line of an app's feed. The store decides what reaches the feed; this module trusts
/// that ephemeral events and dismisses of ephemeral items have already been filtered out.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FeedEntry {
    /// Host wall clock when the entry was recorded. Display only; feed order is file order.
    pub wall: UnixMillis,
    pub app_id: String,
    pub record: FeedRecord,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeedRecord {
    Event {
        event_id: String,
        urgency: Urgency,
        title: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
    },
    /// The user acknowledged an item. `shell` records which shell sent the dismiss.
    Dismiss { item_id: String, shell: String },
}

/// Host settings. Both fields belong to the user and override anything an app asks for.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub muted: bool,
    /// Keyed by app id. A missing entry means `Volume::All`.
    #[serde(default)]
    pub volumes: BTreeMap<String, Volume>,
}

/// What a name in the state directory is, according to the persistence contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Feed,
    Pairings,
    Settings,
    /// One of this module's temp files. Present only mid-write or after a crash.
    Temp,
}

/// Classifies a file name. `None` means the name is outside the persistence contract, which
/// the ephemeral audit treats as a failure.
pub fn classify(name: &str) -> Option<FileKind> {
    if name == PAIRINGS_FILE {
        return Some(FileKind::Pairings);
    }
    if name == SETTINGS_FILE {
        return Some(FileKind::Settings);
    }
    if name.starts_with(TEMP_PREFIX) {
        return Some(FileKind::Temp);
    }
    let stem = name.strip_prefix(FEED_PREFIX)?.strip_suffix(FEED_SUFFIX)?;
    let digits = stem.strip_prefix(FEED_HASHED_MARK).unwrap_or(stem);
    let is_hex = !digits.is_empty()
        && digits.len() % 2 == 0
        && digits.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    is_hex.then_some(FileKind::Feed)
}

/// The feed file name for an app. App ids may contain `/`, `..`, `@`, NUL or anything else an
/// app sends, so the id is never used as a path component; only its hex encoding (or the hex
/// of its SHA-256, when long) is.
pub fn feed_file_name(app_id: &str) -> String {
    let bytes = app_id.as_bytes();
    if bytes.len() <= MAX_HEX_ID_BYTES {
        format!("{FEED_PREFIX}{}{FEED_SUFFIX}", hex(bytes))
    } else {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(bytes);
        format!("{FEED_PREFIX}{FEED_HASHED_MARK}{}{FEED_SUFFIX}", hex(&digest))
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    s
}

// ---------------------------------------------------------------------------------------
// Locating the state directory
// ---------------------------------------------------------------------------------------

/// The platform's state directory for traytray, or `None` if the environment does not name
/// one. `env` looks up an environment variable; pass `|k| std::env::var_os(k)` in production.
pub fn state_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if cfg!(windows) { windows_state_dir(env) } else { xdg_state_dir(env) }
}

/// `$XDG_STATE_HOME/traytray`, else `~/.local/state/traytray`. The XDG spec says a relative
/// value must be ignored, and a relative `HOME` would make the location depend on the
/// daemon's working directory, so both must be absolute.
pub fn xdg_state_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |k: &str| env(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(base) = absolute("XDG_STATE_HOME") {
        return Some(base.join("traytray"));
    }
    absolute("HOME").map(|home| home.join(".local").join("state").join("traytray"))
}

/// `%LOCALAPPDATA%\Traytray\state`. LocalAppData is per-user and not roamed, which is what a
/// feed and token hashes for this machine want.
pub fn windows_state_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let base = PathBuf::from(env("LOCALAPPDATA")?);
    base.is_absolute().then(|| base.join("Traytray").join("state"))
}

// ---------------------------------------------------------------------------------------
// The state directory
// ---------------------------------------------------------------------------------------

/// An opened, checked state directory. All reads and writes go through it.
///
/// One traytrayd owns a state directory at a time: `open` sweeps leftover temp files, and the
/// feed line counts are cached in memory.
#[derive(Debug)]
pub struct StateDir {
    path: PathBuf,
    /// Per feed file: lines on disk and whether the last one lacks its newline. Lets `append`
    /// decide when to prune without rereading the file on every event.
    feeds: HashMap<String, FeedFile>,
    /// Makes every compaction fail with this error, for tests of the failure path that must
    /// not depend on file permissions (which root ignores).
    #[cfg(test)]
    compaction_fault: Option<io::ErrorKind>,
}

/// The outcome of a successful `append`. Every variant means the entry is in the feed.
#[derive(Debug)]
pub enum Appended {
    /// The entry was written and the file is still within twice the retention limit.
    Written,
    /// The entry was written and the file was rewritten to the newest entries.
    Compacted,
    /// The entry was written, but rewriting the file to the newest entries failed. The file
    /// stays larger than the limit until a later append's compaction succeeds. Worth logging;
    /// not a reason to append again.
    CompactionFailed(io::Error),
}

#[derive(Debug, Clone, Copy)]
struct FeedFile {
    lines: usize,
    unterminated: bool,
}

impl StateDir {
    /// Creates the directory if needed (mode 0700 on unix) and checks it is safe to use: not
    /// a symlink, a directory, and owned by the current user. An existing directory with
    /// looser permissions is tightened, since only traytrayd should ever read it.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::open_checked(path.into(), current_uid)
    }

    /// `open` with the current user's uid supplied by `our_uid`, so tests can exercise the
    /// owner rule on a directory they own instead of depending on how /tmp happens to be set
    /// up on the machine running them.
    fn open_checked(path: PathBuf, our_uid: fn(&Path) -> io::Result<u32>) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid_input("state directory must be an absolute path"));
        }
        create_private_dir(&path)?;

        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("state directory {} is a symlink", path.display()),
            ));
        }
        if !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("state directory {} is not a directory", path.display()),
            ));
        }
        // Ownership must be settled before anything is changed or deleted inside it.
        check_owner(&path, &meta, our_uid)?;
        #[cfg(unix)]
        if meta.mode() & 0o777 != DIR_MODE {
            fs::set_permissions(&path, fs::Permissions::from_mode(DIR_MODE))?;
        }

        let dir = StateDir {
            path,
            feeds: HashMap::new(),
            #[cfg(test)]
            compaction_fault: None,
        };
        dir.sweep_temp_files()?;
        Ok(dir)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // --- feed ---------------------------------------------------------------------------

    /// Appends one entry to its app's feed. When the file passes twice the retention limit it
    /// is rewritten with only the newest `FEED_EVENTS_PER_APP` entries, so the file stays
    /// bounded while most appends cost one write.
    ///
    /// Appends are not fsynced: the feed is a short memory, and losing the last few entries
    /// to a power cut costs less than a disk flush per event.
    ///
    /// `Err` means the entry was not appended (though a failed write may have left part of a
    /// line, which readers skip). Once the line is written the result is `Ok`, even if the
    /// prune that follows fails: reporting that as an error would make a retrying caller
    /// record the entry twice. A failed prune is retried by the next append.
    pub fn append(&mut self, entry: &FeedEntry) -> io::Result<Appended> {
        if entry.app_id.is_empty() {
            return Err(invalid_input("feed entry has an empty app id"));
        }
        let name = feed_file_name(&entry.app_id);
        let path = self.path.join(&name);
        let mut line = framing::encode(entry).map_err(io::Error::other)?;

        let mut state = match self.feeds.get(&name) {
            Some(s) => *s,
            None => scan_feed(&path)?,
        };
        if state.unterminated {
            // A crash mid-append left a partial line; end it so the new entry stays readable.
            line.insert(0, b'\n');
        }

        let written = open_feed_for_append(&path).and_then(|mut f| f.write_all(&line));
        if let Err(e) = written {
            // The file may now hold part of the line; rescan next time rather than guess.
            self.feeds.remove(&name);
            return Err(e);
        }
        state.lines += 1;
        state.unterminated = false;
        self.feeds.insert(name.clone(), state);

        if state.lines <= 2 * FEED_EVENTS_PER_APP {
            return Ok(Appended::Written);
        }
        Ok(match self.compact_feed(&name, &entry.app_id) {
            Ok(()) => Appended::Compacted,
            Err(e) => Appended::CompactionFailed(e),
        })
    }

    /// The newest `FEED_EVENTS_PER_APP` entries of an app's feed, oldest first. Lines that do
    /// not parse, or that belong to another app, are skipped rather than failing the read.
    pub fn read(&self, app_id: &str) -> io::Result<Vec<FeedEntry>> {
        let bytes = match fs::read(self.path.join(feed_file_name(app_id))) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut entries: Vec<FeedEntry> =
            parse_feed(&bytes, app_id).into_iter().map(|(_, e)| e).collect();
        let excess = entries.len().saturating_sub(FEED_EVENTS_PER_APP);
        entries.drain(..excess);
        Ok(entries)
    }

    /// "Clear history": removes the app's feed file. Absent and empty read the same, and
    /// removing leaves nothing of the old entries in the directory.
    pub fn clear(&mut self, app_id: &str) -> io::Result<()> {
        let name = feed_file_name(app_id);
        self.feeds.remove(&name);
        match fs::remove_file(self.path.join(&name)) {
            Ok(()) => sync_dir(&self.path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Unpairing: the app's feed must not outlive its pairing. Kept separate from `clear` so
    /// callers state which of the two user actions they are carrying out; on disk both leave
    /// no feed file. The pairing record itself is removed by saving the new pairings list.
    pub fn delete(&mut self, app_id: &str) -> io::Result<()> {
        self.clear(app_id)
    }

    fn compact_feed(&mut self, name: &str, app_id: &str) -> io::Result<()> {
        let path = self.path.join(name);
        let bytes = fs::read(&path)?;
        let valid = parse_feed(&bytes, app_id);
        let keep = &valid[valid.len().saturating_sub(FEED_EVENTS_PER_APP)..];
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for (raw, _) in keep {
            // The original bytes are kept, not re-encoded, so fields a newer traytrayd wrote
            // survive a compaction by an older one.
            out.extend_from_slice(raw);
            out.push(b'\n');
        }
        #[cfg(test)]
        let result = match self.compaction_fault {
            Some(kind) => Err(io::Error::from(kind)),
            None => self.write_atomic(name, &out),
        };
        #[cfg(not(test))]
        let result = self.write_atomic(name, &out);
        match result {
            Ok(()) => {
                self.feeds
                    .insert(name.to_owned(), FeedFile { lines: keep.len(), unterminated: false });
            }
            Err(_) => {
                self.feeds.remove(name);
            }
        }
        result
    }

    // --- pairings and settings ----------------------------------------------------------

    /// Pairing records, or an empty list if none were ever saved. A file that does not parse
    /// is an error, not an empty list: treating it as empty would silently unpair every app
    /// at the next save.
    pub fn load_pairings<T: DeserializeOwned>(&self) -> io::Result<Vec<T>> {
        self.load_json(PAIRINGS_FILE).map(Option::unwrap_or_default)
    }

    pub fn save_pairings<T: Serialize>(&self, records: &[T]) -> io::Result<()> {
        self.save_json(PAIRINGS_FILE, &records)
    }

    /// Settings, or the defaults if none were ever saved. A file that does not parse is an
    /// error for the same reason as pairings: a silent reset would unmute everything.
    pub fn load_settings(&self) -> io::Result<Settings> {
        self.load_json(SETTINGS_FILE).map(Option::unwrap_or_default)
    }

    pub fn save_settings(&self, settings: &Settings) -> io::Result<()> {
        self.save_json(SETTINGS_FILE, settings)
    }

    fn load_json<T: DeserializeOwned>(&self, name: &str) -> io::Result<Option<T>> {
        match fs::read(self.path.join(name)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{name}: {e}"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn save_json<T: Serialize + ?Sized>(&self, name: &str, value: &T) -> io::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        bytes.push(b'\n');
        self.write_atomic(name, &bytes)
    }

    // --- audit --------------------------------------------------------------------------

    /// Every entry in the state directory, sorted, whatever it is. The audit compares this
    /// against `classify`; listing only the names this module expects would hide strays.
    pub fn list_files(&self) -> io::Result<Vec<PathBuf>> {
        let mut out = fs::read_dir(&self.path)?
            .map(|e| e.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()?;
        out.sort();
        Ok(out)
    }

    // --- writing ------------------------------------------------------------------------

    /// Replaces `name` so that a reader, or a crash, sees either the old file or the new one,
    /// never a mix: write a temp file in the same directory, fsync it, rename it over the
    /// target, then fsync the directory so the rename itself is durable.
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let temp = TempFile::create(&self.path)?;
        let mut file = temp.file.as_ref().expect("temp file is open until committed");
        file.write_all(bytes)?;
        file.sync_all()?;
        temp.commit(&self.path.join(name))?;
        sync_dir(&self.path)
    }

    fn sweep_temp_files(&self) -> io::Result<()> {
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let is_temp = entry.file_name().to_str().is_some_and(|n| n.starts_with(TEMP_PREFIX));
            if is_temp && !entry.file_type()?.is_dir() {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
}

/// A temp file that deletes itself unless `commit` renames it into place. Every early return
/// between creation and rename therefore cleans up, including ones added later.
struct TempFile {
    path: PathBuf,
    file: Option<File>,
}

impl TempFile {
    fn create(dir: &Path) -> io::Result<Self> {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(temp_file_name(std::process::id(), seq));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(FILE_MODE);
        let file = opts.open(&path)?;
        Ok(TempFile { path, file: Some(file) })
    }

    fn commit(mut self, target: &Path) -> io::Result<()> {
        // Close before renaming: Windows refuses to rename a file that is still open.
        self.file = None;
        fs::rename(&self.path, target)?;
        self.path = PathBuf::new();
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        self.file = None;
        if !self.path.as_os_str().is_empty() {
            // Best effort: a failure here leaves a TEMP_PREFIX file, which `open` sweeps.
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn open_feed_for_append(path: &Path) -> io::Result<File> {
    // The directory is private, so a symlink here can only come from the same user, which the
    // threat model puts out of scope. Refusing it still keeps an accident from making the
    // feed append JSON to some unrelated file.
    match fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not a regular file", path.display()),
            ));
        }
        _ => {}
    }
    let mut opts = OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    opts.mode(FILE_MODE);
    let file = opts.open(path)?;
    // `mode` applies only on creation; a file left by an older build or copied in by hand is
    // brought back to 0600 before anything more is added to it.
    #[cfg(unix)]
    if file.metadata()?.mode() & 0o777 != FILE_MODE {
        file.set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
    }
    Ok(file)
}

fn scan_feed(path: &Path) -> io::Result<FeedFile> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    Ok(FeedFile {
        lines: bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count(),
        unterminated: bytes.last().is_some_and(|&b| b != b'\n'),
    })
}

/// Parses a feed file into (raw line, entry) pairs, keeping only lines that decode and belong
/// to `app_id`. A torn final line or hand-edited garbage costs that line, not the feed.
fn parse_feed<'a>(bytes: &'a [u8], app_id: &str) -> Vec<(&'a [u8], FeedEntry)> {
    bytes
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_slice::<FeedEntry>(l).ok().map(|e| (l, e)))
        .filter(|(_, e)| e.app_id == app_id)
        .collect()
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    // Parents get default permissions; only the traytray directory itself is made private, so
    // creating it never changes the mode of a shared directory such as ~/.local.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(DIR_MODE);
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// The current user's effective uid. std exposes no `getuid`, and the project takes no libc
/// dependency, so it is learned from a probe file: a file we create is owned by our
/// effective uid. In a directory we cannot write, the probe fails and so does `open`, which
/// is the right outcome too.
#[cfg(unix)]
fn current_uid(dir: &Path) -> io::Result<u32> {
    let probe = TempFile::create(dir)?;
    let uid = probe.file.as_ref().expect("probe is open").metadata()?.uid();
    Ok(uid)
}

/// Windows has no uid; `check_owner` there never asks for one.
#[cfg(not(unix))]
fn current_uid(_dir: &Path) -> io::Result<u32> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no uid on this platform"))
}

/// Refuses a directory owned by someone else.
#[cfg(unix)]
fn check_owner(
    dir: &Path,
    dir_meta: &fs::Metadata,
    our_uid: fn(&Path) -> io::Result<u32>,
) -> io::Result<()> {
    let our_uid = our_uid(dir)?;
    if dir_meta.uid() != our_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "state directory {} is not owned by the current user (owner uid {}, ours {})",
                dir.display(),
                dir_meta.uid(),
                our_uid
            ),
        ));
    }
    Ok(())
}

/// Windows: %LOCALAPPDATA% is created per user with an ACL limited to that user and SYSTEM.
/// Checking the owner SID needs Win32 calls the project does not take a dependency for.
#[cfg(not(unix))]
fn check_owner(
    _dir: &Path,
    _dir_meta: &fs::Metadata,
    _our_uid: fn(&Path) -> io::Result<u32>,
) -> io::Result<()> {
    Ok(())
}

/// Makes renames and removals in `dir` durable. Only unix can open a directory for this;
/// NTFS journals the metadata change itself.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn invalid_input(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique directory under the system temp dir, removed on drop. The state dir is a
    /// subdirectory so `StateDir::open` exercises creation.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!("traytray-feed-test-{}-{tag}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        fn state(&self) -> PathBuf {
            self.0.join("state")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl StateDir {
        /// `append` for tests that expect every compaction to succeed, so a failed prune
        /// cannot hide behind an `Ok`.
        fn append_ok(&mut self, entry: &FeedEntry) -> io::Result<Appended> {
            let result = self.append(entry);
            if let Ok(Appended::CompactionFailed(e)) = &result {
                panic!("compaction failed for {:?}: {e}", entry.app_id);
            }
            result
        }
    }

    fn event(app: &str, n: usize) -> FeedEntry {
        FeedEntry {
            wall: 1_700_000_000_000 + n as u64,
            app_id: app.into(),
            record: FeedRecord::Event {
                event_id: format!("e{n}"),
                urgency: Urgency::Notice,
                title: format!("event {n}"),
                body: String::new(),
                item_id: None,
            },
        }
    }

    fn event_ids(entries: &[FeedEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match &e.record {
                FeedRecord::Event { event_id, .. } => event_id.clone(),
                FeedRecord::Dismiss { item_id, .. } => format!("dismiss:{item_id}"),
            })
            .collect()
    }

    fn names(dir: &StateDir) -> Vec<String> {
        dir.list_files()
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect()
    }

    fn line_count(path: &Path) -> usize {
        fs::read(path).unwrap().iter().filter(|&&b| b == b'\n').count()
    }

    #[test]
    fn state_dir_follows_xdg_then_home() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
        assert_eq!(
            xdg_state_dir(env(&[("XDG_STATE_HOME", "/x/state"), ("HOME", "/home/u")])),
            Some(PathBuf::from("/x/state/traytray"))
        );
        assert_eq!(
            xdg_state_dir(env(&[("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.local/state/traytray"))
        );
        // Relative values are ignored per the XDG spec, empty ones count as unset.
        assert_eq!(
            xdg_state_dir(env(&[("XDG_STATE_HOME", "rel"), ("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.local/state/traytray"))
        );
        assert_eq!(
            xdg_state_dir(env(&[("XDG_STATE_HOME", ""), ("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.local/state/traytray"))
        );
        assert_eq!(xdg_state_dir(env(&[("HOME", "relative")])), None);
        assert_eq!(xdg_state_dir(env(&[])), None);

        assert_eq!(
            windows_state_dir(env(&[("LOCALAPPDATA", "/c/Users/u/AppData/Local")])),
            Some(PathBuf::from("/c/Users/u/AppData/Local").join("Traytray").join("state"))
        );
        assert_eq!(windows_state_dir(env(&[])), None);
    }

    #[test]
    fn retention_keeps_exactly_the_newest_after_passing_twice_the_limit() {
        let s = Scratch::new("retention");
        let mut dir = StateDir::open(s.state()).unwrap();
        let file = dir.path().join(feed_file_name("app"));

        for n in 1..=2 * FEED_EVENTS_PER_APP {
            dir.append_ok(&event("app", n)).unwrap();
        }
        assert_eq!(line_count(&file), 2 * FEED_EVENTS_PER_APP, "no prune until exceeded");

        dir.append_ok(&event("app", 2 * FEED_EVENTS_PER_APP + 1)).unwrap();
        assert_eq!(line_count(&file), FEED_EVENTS_PER_APP);
        let got = dir.read("app").unwrap();
        let want: Vec<String> = (FEED_EVENTS_PER_APP + 2..=2 * FEED_EVENTS_PER_APP + 1)
            .map(|n| format!("e{n}"))
            .collect();
        assert_eq!(event_ids(&got), want);
        assert_eq!(got.last().unwrap(), &event("app", 2 * FEED_EVENTS_PER_APP + 1));
        assert!(names(&dir).iter().all(|n| !n.starts_with(TEMP_PREFIX)));
    }

    /// Retention must hold for every app id length, in particular the longest ids still named
    /// in hex, whose feed names leave the least room for anything built on top of them.
    #[test]
    fn retention_holds_at_the_hex_and_hash_name_boundary() {
        let s = Scratch::new("boundary");
        let mut dir = StateDir::open(s.state()).unwrap();
        for len in [100, 110, 115, MAX_HEX_ID_BYTES, MAX_HEX_ID_BYTES + 1] {
            let id = "r".repeat(len);
            let file = dir.path().join(feed_file_name(&id));
            for n in 1..=2 * FEED_EVENTS_PER_APP {
                assert!(matches!(dir.append(&event(&id, n)), Ok(Appended::Written)), "len {len}");
            }
            let last = dir.append(&event(&id, 2 * FEED_EVENTS_PER_APP + 1));
            assert!(matches!(last, Ok(Appended::Compacted)), "len {len}: {last:?}");
            assert_eq!(line_count(&file), FEED_EVENTS_PER_APP, "len {len}");
        }
        assert!(names(&dir).iter().all(|n| !n.starts_with(TEMP_PREFIX)));
    }

    #[test]
    fn every_name_the_module_creates_fits_the_file_name_limit() {
        let longest_hex = feed_file_name(&"z".repeat(MAX_HEX_ID_BYTES));
        let hashed = feed_file_name(&"z".repeat(MAX_HEX_ID_BYTES + 1));
        let longest_temp = temp_file_name(u32::MAX, u64::MAX);
        for name in [&longest_hex, &hashed, &longest_temp] {
            assert!(name.len() <= MAX_NAME_BYTES, "{} bytes: {name}", name.len());
        }
        assert!(!hashed.contains(&hex(b"zz")), "long ids must be hashed: {hashed}");
        assert_eq!(classify(&longest_temp), Some(FileKind::Temp));
    }

    /// A failed prune must not be reported as a failed append: the entry is on disk, and a
    /// caller that retried on `Err` would record it twice.
    #[test]
    fn append_is_ok_when_only_the_prune_fails() {
        let s = Scratch::new("prunefail");
        let mut dir = StateDir::open(s.state()).unwrap();
        let file = dir.path().join(feed_file_name("app"));
        for n in 1..=2 * FEED_EVENTS_PER_APP {
            dir.append_ok(&event("app", n)).unwrap();
        }

        dir.compaction_fault = Some(io::ErrorKind::StorageFull);
        let over = 2 * FEED_EVENTS_PER_APP + 1;
        match dir.append(&event("app", over)) {
            Ok(Appended::CompactionFailed(e)) => assert_eq!(e.kind(), io::ErrorKind::StorageFull),
            other => panic!("expected CompactionFailed, got {other:?}"),
        }
        assert_eq!(line_count(&file), over, "the entry was written exactly once");
        assert_eq!(dir.read("app").unwrap().last().unwrap(), &event("app", over));

        // The prune is retried by the next append, and succeeds once the fault clears.
        assert!(matches!(dir.append(&event("app", over + 1)), Ok(Appended::CompactionFailed(_))));
        assert_eq!(line_count(&file), over + 1);
        dir.compaction_fault = None;
        assert!(matches!(dir.append(&event("app", over + 2)), Ok(Appended::Compacted)));
        assert_eq!(line_count(&file), FEED_EVENTS_PER_APP);
        assert_eq!(dir.read("app").unwrap().last().unwrap(), &event("app", over + 2));
        assert!(names(&dir).iter().all(|n| !n.starts_with(TEMP_PREFIX)));
    }

    #[test]
    fn read_returns_at_most_the_retention_limit() {
        let s = Scratch::new("readcap");
        let mut dir = StateDir::open(s.state()).unwrap();
        for n in 1..=FEED_EVENTS_PER_APP + 5 {
            dir.append_ok(&event("app", n)).unwrap();
        }
        let got = dir.read("app").unwrap();
        assert_eq!(got.len(), FEED_EVENTS_PER_APP);
        assert_eq!(event_ids(&got)[0], "e6");
    }

    #[test]
    fn retention_count_survives_reopening() {
        let s = Scratch::new("reopen");
        let file = s.state().join(feed_file_name("app"));
        {
            let mut dir = StateDir::open(s.state()).unwrap();
            for n in 1..=2 * FEED_EVENTS_PER_APP {
                dir.append_ok(&event("app", n)).unwrap();
            }
        }
        let mut dir = StateDir::open(s.state()).unwrap();
        dir.append_ok(&event("app", 0)).unwrap();
        assert_eq!(line_count(&file), FEED_EVENTS_PER_APP);
    }

    #[test]
    fn corrupt_and_foreign_lines_are_skipped() {
        let s = Scratch::new("corrupt");
        let mut dir = StateDir::open(s.state()).unwrap();
        dir.append_ok(&event("app", 1)).unwrap();
        let file = dir.path().join(feed_file_name("app"));
        let mut f = OpenOptions::new().append(true).open(&file).unwrap();
        f.write_all(b"{not json\n").unwrap();
        f.write_all(&framing::encode(&event("other", 9)).unwrap()).unwrap();
        // A torn write: no trailing newline.
        f.write_all(b"{\"wall\":1,\"app_id\":\"app\",\"rec").unwrap();
        drop(f);

        // A fresh StateDir has no cached line state, so it must notice the torn line itself.
        let mut dir = StateDir::open(s.state()).unwrap();
        dir.append_ok(&event("app", 2)).unwrap();
        assert_eq!(event_ids(&dir.read("app").unwrap()), ["e1", "e2"]);
    }

    #[test]
    fn dismiss_round_trips() {
        let s = Scratch::new("dismiss");
        let mut dir = StateDir::open(s.state()).unwrap();
        let d = FeedEntry {
            wall: 5,
            app_id: "app".into(),
            record: FeedRecord::Dismiss { item_id: "s1".into(), shell: "kde".into() },
        };
        dir.append_ok(&d).unwrap();
        assert_eq!(dir.read("app").unwrap(), vec![d]);
    }

    #[test]
    fn hostile_app_ids_map_to_safe_names_inside_the_dir() {
        let s = Scratch::new("names");
        let mut dir = StateDir::open(s.state()).unwrap();
        let long = "x".repeat(MAX_HEX_ID_BYTES + 1);
        let ids = [
            "../../escape",
            "..",
            "a/b",
            "/etc/passwd",
            "app@nNODE123CNTRL",
            "back\\slash",
            "nul\0byte",
            "ünï",
            long.as_str(),
        ];
        for (n, id) in ids.iter().enumerate() {
            dir.append_ok(&event(id, n)).unwrap();
        }
        let listed = dir.list_files().unwrap();
        assert_eq!(listed.len(), ids.len(), "every id gets its own file: {listed:?}");
        for p in &listed {
            assert_eq!(p.parent(), Some(dir.path()), "{p:?} escaped the state dir");
            let name = p.file_name().unwrap().to_str().unwrap();
            assert_eq!(classify(name), Some(FileKind::Feed), "{name}");
            assert!(name.len() <= 255, "{name} is too long for a file name");
        }
        assert_eq!(feed_file_name("a/b"), "feed-612f62.jsonl");
        assert_eq!(feed_file_name("..").len(), "feed-2e2e.jsonl".len());
        for (n, id) in ids.iter().enumerate() {
            assert_eq!(dir.read(id).unwrap(), vec![event(id, n)]);
        }
        // Nothing was created next to the state dir.
        assert_eq!(fs::read_dir(&s.0).unwrap().count(), 1);
    }

    #[test]
    fn empty_app_id_is_refused() {
        let s = Scratch::new("empty");
        let mut dir = StateDir::open(s.state()).unwrap();
        assert!(dir.append_ok(&event("", 1)).is_err());
        assert!(dir.list_files().unwrap().is_empty());
    }

    #[test]
    fn clear_and_delete_remove_the_feed() {
        let s = Scratch::new("clear");
        let mut dir = StateDir::open(s.state()).unwrap();
        dir.append_ok(&event("a", 1)).unwrap();
        dir.append_ok(&event("b", 1)).unwrap();
        dir.clear("a").unwrap();
        assert!(dir.read("a").unwrap().is_empty());
        assert_eq!(names(&dir), [feed_file_name("b")]);
        dir.delete("b").unwrap();
        assert!(dir.list_files().unwrap().is_empty());
        dir.clear("never-existed").unwrap();
        // The cached line count must not outlive the file.
        dir.append_ok(&event("a", 2)).unwrap();
        assert_eq!(event_ids(&dir.read("a").unwrap()), ["e2"]);
    }

    #[test]
    fn settings_and_pairings_round_trip_and_default_when_absent() {
        let s = Scratch::new("json");
        let dir = StateDir::open(s.state()).unwrap();
        assert_eq!(dir.load_settings().unwrap(), Settings::default());
        assert!(dir.load_pairings::<serde_json::Value>().unwrap().is_empty());

        let settings = Settings {
            muted: true,
            volumes: BTreeMap::from([("app".to_owned(), Volume::BadgeOnly)]),
        };
        dir.save_settings(&settings).unwrap();
        assert_eq!(dir.load_settings().unwrap(), settings);

        let pairings = vec![serde_json::json!({"app_id": "a@n1", "token_sha256": "00"})];
        dir.save_pairings(&pairings).unwrap();
        assert_eq!(dir.load_pairings::<serde_json::Value>().unwrap(), pairings);
    }

    #[test]
    fn unreadable_settings_or_pairings_are_an_error_not_a_reset() {
        let s = Scratch::new("badjson");
        let dir = StateDir::open(s.state()).unwrap();
        fs::write(dir.path().join(SETTINGS_FILE), b"{\"muted\": tru").unwrap();
        fs::write(dir.path().join(PAIRINGS_FILE), b"[").unwrap();
        assert_eq!(dir.load_settings().unwrap_err().kind(), io::ErrorKind::InvalidData);
        let err = dir.load_pairings::<serde_json::Value>().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn atomic_save_leaves_no_temp_files_on_success_or_failure() {
        let s = Scratch::new("atomic");
        let dir = StateDir::open(s.state()).unwrap();
        dir.save_settings(&Settings::default()).unwrap();
        dir.save_pairings::<u8>(&[]).unwrap();
        assert_eq!(names(&dir), [PAIRINGS_FILE, SETTINGS_FILE]);

        // Renaming a file over a non-empty directory fails on every platform, which forces
        // the failure path after the temp file has been written.
        fs::remove_file(dir.path().join(SETTINGS_FILE)).unwrap();
        fs::create_dir(dir.path().join(SETTINGS_FILE)).unwrap();
        fs::write(dir.path().join(SETTINGS_FILE).join("x"), b"").unwrap();
        assert!(dir.save_settings(&Settings::default()).is_err());
        assert_eq!(names(&dir), [PAIRINGS_FILE, SETTINGS_FILE]);
    }

    #[test]
    fn open_sweeps_leftover_temp_files() {
        let s = Scratch::new("sweep");
        drop(StateDir::open(s.state()).unwrap());
        let stray = s.state().join(format!("{TEMP_PREFIX}settings.json.1.1"));
        fs::write(&stray, b"half").unwrap();
        let dir = StateDir::open(s.state()).unwrap();
        assert!(!stray.exists());
        assert!(dir.list_files().unwrap().is_empty());
    }

    #[test]
    fn open_refuses_a_symlink_or_a_file() {
        let s = Scratch::new("refuse");
        let real = s.0.join("real");
        fs::create_dir(&real).unwrap();
        let file = s.0.join("file");
        fs::write(&file, b"").unwrap();
        assert!(StateDir::open(&file).is_err());
        assert!(StateDir::open(PathBuf::from("relative/state")).is_err());
        #[cfg(unix)]
        {
            let link = s.0.join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let err = StateDir::open(&link).unwrap_err();
            assert!(err.to_string().contains("symlink"), "{err}");
        }
    }

    /// The owner rule with the uid lookup replaced, so it runs the same everywhere (as root, in
    /// a sandbox whose /tmp the test user owns, in CI). The refusal must come before the
    /// directory's mode is changed or anything in it is deleted.
    #[cfg(unix)]
    #[test]
    fn open_refuses_a_directory_whose_owner_is_not_us_before_touching_it() {
        let s = Scratch::new("owneruid");
        drop(StateDir::open(s.state()).unwrap());
        fs::set_permissions(s.state(), fs::Permissions::from_mode(0o755)).unwrap();
        let stray = s.state().join(format!("{TEMP_PREFIX}1.1"));
        fs::write(&stray, b"half").unwrap();

        let someone_else = |dir: &Path| current_uid(dir).map(|uid| uid.wrapping_add(1));
        let err = StateDir::open_checked(s.state(), someone_else).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("not owned by the current user"), "{err}");
        assert_eq!(fs::metadata(s.state()).unwrap().mode() & 0o777, 0o755, "mode was changed");
        assert!(stray.exists(), "a file was deleted from a directory we do not own");

        // The same directory with the real uid is accepted, so the refusal above was the
        // owner rule and not some other check.
        StateDir::open_checked(s.state(), current_uid).unwrap();
        assert!(!stray.exists());
    }

    /// The system temp dir is usually root-owned and world-writable, which is exactly the
    /// case the owner check exists for: the probe succeeds but belongs to someone else. This
    /// runs the real uid lookup end to end where the machine allows it; the test above covers
    /// the rule unconditionally.
    #[cfg(unix)]
    #[test]
    fn open_refuses_a_directory_owned_by_someone_else() {
        let shared = Path::new("/tmp");
        let Ok(meta) = fs::symlink_metadata(shared) else { return };
        let s = Scratch::new("owner");
        let ours = fs::metadata(&s.0).unwrap().uid();
        if !meta.is_dir() || meta.uid() == ours || meta.mode() & 0o002 == 0 {
            eprintln!("skipped: /tmp is not a world-writable directory owned by someone else");
            return;
        }
        let err = StateDir::open(shared).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("not owned by the current user"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_private() {
        let s = Scratch::new("perms");
        let mut dir = StateDir::open(s.state()).unwrap();
        dir.append_ok(&event("app", 1)).unwrap();
        dir.save_settings(&Settings::default()).unwrap();
        dir.save_pairings::<u8>(&[]).unwrap();

        let mode = |p: &Path| fs::metadata(p).unwrap().mode() & 0o777;
        assert_eq!(mode(dir.path()), 0o700);
        for p in dir.list_files().unwrap() {
            assert_eq!(mode(&p), 0o600, "{p:?}");
        }
        // The compaction rewrite goes through a temp file too, so it must keep 0600.
        for n in 0..=2 * FEED_EVENTS_PER_APP {
            dir.append_ok(&event("app", n)).unwrap();
        }
        assert_eq!(mode(&dir.path().join(feed_file_name("app"))), 0o600);

        // Loose permissions left by something else are tightened, not trusted.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let loose = dir.path().join(feed_file_name("other"));
        fs::write(&loose, b"").unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
        let mut dir = StateDir::open(s.state()).unwrap();
        dir.append_ok(&event("other", 1)).unwrap();
        assert_eq!(mode(dir.path()), 0o700);
        assert_eq!(mode(&loose), 0o600);
    }

    /// The persistence contract, checked the way the ephemeral audit will: after normal use,
    /// every name in the state directory is a feed, pairings.json or settings.json.
    #[test]
    fn persistence_contract_only_three_kinds_of_file() {
        let s = Scratch::new("contract");
        let mut dir = StateDir::open(s.state()).unwrap();
        for n in 0..=2 * FEED_EVENTS_PER_APP {
            dir.append_ok(&event("local.agents", n)).unwrap();
        }
        dir.append_ok(&event("robo@nREMOTE", 1)).unwrap();
        dir.append_ok(&FeedEntry {
            wall: 9,
            app_id: "local.agents".into(),
            record: FeedRecord::Dismiss { item_id: "s1".into(), shell: "kde".into() },
        })
        .unwrap();
        dir.clear("robo@nREMOTE").unwrap();
        dir.append_ok(&event("robo@nREMOTE", 2)).unwrap();
        dir.save_settings(&Settings { muted: true, ..Settings::default() }).unwrap();
        dir.save_pairings(&[serde_json::json!({"app_id": "robo@nREMOTE"})]).unwrap();

        let all = names(&dir);
        let strays: Vec<&String> = all
            .iter()
            .filter(|n| {
                !matches!(
                    classify(n),
                    Some(FileKind::Feed | FileKind::Pairings | FileKind::Settings)
                )
            })
            .collect();
        assert!(strays.is_empty(), "files outside the persistence contract: {strays:?}");
        assert_eq!(
            all,
            [
                feed_file_name("local.agents"),
                feed_file_name("robo@nREMOTE"),
                PAIRINGS_FILE.to_owned(),
                SETTINGS_FILE.to_owned(),
            ]
        );
        // And nothing landed beside the state directory either.
        assert_eq!(fs::read_dir(&s.0).unwrap().count(), 1);
    }

    #[test]
    fn classify_rejects_near_misses() {
        assert_eq!(classify("feed-61.jsonl"), Some(FileKind::Feed));
        assert_eq!(classify(&feed_file_name(&"y".repeat(500))), Some(FileKind::Feed));
        for bad in [
            "feed-.jsonl",
            "feed-6.jsonl",
            "feed-6G.jsonl",
            "feed-61.jsonl.bak",
            "feed-61.json",
            "settings.json~",
            "pairings.json.old",
            "state.json",
        ] {
            assert_eq!(classify(bad), None, "{bad}");
        }
    }
}
