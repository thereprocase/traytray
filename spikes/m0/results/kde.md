# M0.1 and M0.2: KDE Plasma spikes

Date: 2026-10-03

## Environment

- The Linux desktop: KDE Plasma 6.7.4 (plasma-workspace 6.7.4, libplasma 6.7.4, kwin 6.7.4),
  Wayland session, KDE Frameworks 6.29 (kded6 owns `org.kde.StatusNotifierWatcher`).
- Qt 6.11.2 (qt6-base, qt6-declarative), CMake 4.4.2 with Ninja, GCC 16.2.1. No
  extra-cmake-modules.
- Rust 1.97.1, zbus 5.18.0 (`default-features = false, features = ["tokio"]`), tokio 1.53.1,
  futures-util 0.3.33. Built with `cargo build --offline`; the lockfile resolves 77 packages,
  all from the local cache.
- Python 3.14.7, standard library only.
- At test time the session had one third-party tray item (a status daemon
  using ksni). No Electron, libappindicator or Qt-app items were running.

Raw logs, the dbus-monitor capture and the screenshot are in `spikes/m0/private/` (gitignored).

## M0.1: zbus StatusNotifierHost next to plasmashell

Code: `spikes/m0/sni-host/` (one binary; `--test-item` adds a throwaway item, see below).

What it does: reads the watcher, requests `org.kde.StatusNotifierHost-<pid>`, calls
`RegisterStatusNotifierHost`, reads every item's properties and calls
`com.canonical.dbusmenu.GetLayout(0, -1, [])`, then logs watcher and item signals for 20 s.
The only calls made on items are property reads and GetLayout.

Because the desktop had only one item and it was quiet, a 20 s listen alone would have been a
vacuous signal test. With `--test-item` a second connection publishes a `Passive` item (kept
out of the visible panel) with a two-entry menu, emits NewIcon and NewStatus, then closes.

### Q1. Does a second host coexist with plasmashell? PASS

- Registration succeeded with no error, and the watcher kept `IsStatusNotifierHostRegistered = true`.
- At the end of the run both hosts owned their names:
  `hosts on bus at end: ["org.kde.StatusNotifierHost-<plasmashell pid>", "org.kde.StatusNotifierHost-<spike pid>"]`.
- plasmashell's own tray kept working while the spike host was registered. dbus-monitor
  showed plasmashell's connection reading the new test item while it was registered:
  `2 × Properties.GetAll on /StatusNotifierItem` and `1 × dbusmenu.GetLayout on /MenuBar`, all
  sent from plasmashell's unique name.
- The pre-existing item stayed registered throughout (`items_before=1`, `items_at_end=1`, same
  entry). After the spike exited, only plasmashell's host name remained on the bus. The
  plasmashell PID did not change.

### Q2. Live items and menus? PASS (one real item plus the test item)

Real item (a third-party status daemon), read from the spike host:

```
Id, Title, Status = "NeedsAttention", Category = "SystemServices", IconName set, IconPixmap sizes = []
ToolTip title set, pixmaps = 0, ItemIsMenu = false, Menu = /MenuBar
GetLayout revision=0 -> 7 entries (6 with label + icon-name, 1 separator)
```

Test item, read as soon as its Registered signal arrived: Id, Title, Status=Passive, Category,
IconName and `Menu = /MenuBar` were read, and GetLayout returned 2 entries. AttentionIconName,
the pixmap properties and ToolTip came back as `UnknownProperty` because the test item does not
implement them. The host logged these per property and carried on, which is the behaviour
needed for real items that implement only part of the spec.

dbus-monitor confirms the spike sent only `Properties.Get`, `Properties.GetAll` and
`dbusmenu.GetLayout`. It sent no Activate, ContextMenu, Scroll, Event or AboutToShow.

### Q3. Signals over the listen window: PASS

```
+3.0 s  StatusNotifierWatcher.StatusNotifierItemRegistered  (×3, see below)
+6.0 s  StatusNotifierItem.NewIcon      sender=<test item>
+8.0 s  StatusNotifierItem.NewStatus    "Passive"
+11.0 s StatusNotifierWatcher.StatusNotifierItemUnregistered (×3), ~2 ms after the item's connection closed
```

The real item emitted no signals during the window, so it was not exercised.

### Findings that matter for the implementation

1. **kded's watcher emits every signal three times**, on `/StatusNotifierWatcher`,
   `/modules/StatusNotifierWatcher` and `/modules/statusnotifierwatcher`. A host that matches
   on interface alone sees duplicates. It must match on `path=/StatusNotifierWatcher` or
   dedupe.
2. `ProtocolVersion` reads `0` on this watcher.
3. Entries arrived as `<well-known name>/<object path>`. The `:1.N/path` form that
   libappindicator items use was not observed here, because none were running.
4. If the host subscribes before registering, no Registered signal is missed. Reading an item
   at the moment its Registered signal arrives works.
5. Each property is read separately with its own error handling. Partial items are normal.

## M0.2: compiled C++ QML plugin in a Plasma 6 applet

Code: `spikes/m0/kde-plugin/`:

- `plugin/` (plain CMake, `qt_add_qml_module`, URI `org.traytray.spike`, one MODULE `.so`)
- `plasmoid/` (metadata.json plus contents/ui/main.qml)
- `server/ndjson_server.py`
- `make-bundled.sh` (variant packaging, see Q8)

`TrayClient` (QML_ELEMENT) connects with QLocalSocket and reads in 64 KB chunks. It appends a
segment only if the line would stay ≤ 262144 bytes. Otherwise it clears the line, counts a
rejection and discards bytes up to the next newline. It exposes these properties:

- `latestFrame`, `framesReceived`, `framesRejected`, `lastError`
- `maxBufferedBytes`: the high-water mark of the partial-line buffer

`send()` accepts only a JSON object and re-encodes it compactly before appending `\n`.

The server binds `$XDG_RUNTIME_DIR/traytray-spike-m0.sock` with mode 0600 and sends one frame a
second. Four of those frames are special:

- seq 4: exactly 262144 B (should be accepted)
- seq 6: 262145 B (should be rejected)
- seq 8: 4 MiB (should be rejected)
- seq 10: not JSON (should be rejected)

The plasmoid was installed with `kpackagetool6 --type Plasma/Applet --install` and run with
`plasmawindowed org.traytray.spike.m0`, with `QML_IMPORT_PATH=<build>/qml`. It was not added
to the panel. QML console output only reached stderr with `QT_FORCE_STDERR_LOGGING=1`; without
it, Qt sent it to the journal.

### Q4. Plugin loads: PASS

With `QML_IMPORT_PATH` set, stderr has no QML import error. The only other lines are the
desktop theme's legacy-metadata warning.

Negative control (same package, `QML_IMPORT_PATH` unset):
`main.qml:5:1: module "org.traytray.spike" is not installed`.

### Q5. Frames display: PASS

stderr shows `traytray-spike: connected = true` followed by one log line per frame. The
screenshot (`spectacle -b -n -a`, `private/m0.2-run2-active.png`) shows the plasmawindowed
window with "Connected", "Frames: 13 Rejected: 3 Max buffered: 262144" and the latest frame
text `{"seq":16,"text":"frame 16 from test server","type":"heartbeat"}`.

### Q6. QML → server: PASS

The server log shows a frame on connect and an `action` frame after every 5th received frame:

```
RECV 72B {"app_id":"spike-shell","proto_version":0,"role":"shell","type":"hello"}
RECV 82B {"action_id":"ack","app":"spike-server","item_id":"frame","rev":5,"type":"action"}
RECV 83B {"action_id":"ack","app":"spike-server","item_id":"frame","rev":10,"type":"action"}
```

### Q7. Oversized-line rejection: PASS

- seq 4 (exactly 262144 B) was accepted: `frame 4 len 262144`.
- seq 6 (262145 B) was rejected: `rejected = 1 ... lastError = frame exceeds 262144 bytes`.
  The next frame displayed was seq 7.
- seq 8 (4 MiB) was rejected: `rejected = 2`. The next frame was seq 9.
- seq 10 (not JSON) was rejected: `lastError = frame is not a JSON object`.
- `maxBufferedBytes` stayed at 262144 for the whole run, including the 4 MiB line, so the
  oversized line was never held in memory.
- The stream recovered after each rejection: received counts and the server's `rev` acks line
  up with exactly three rejections.

### Q8. Can the plugin be loaded by an applet in the real system tray? PARTIAL (not run in the tray)

Adding the applet to the real panel or tray was out of scope, so nothing here was observed
inside the system tray itself. Each finding below is labelled with how well it is
established.

- **VERIFIED (runtime, plasmawindowed).** The engine's import path list is
  `["/usr/bin", "qrc:/qt-project.org/imports", "qrc:/qt/qml", <QML_IMPORT_PATH entries>, "/usr/lib/qt6/qml"]`.
  It contains neither `~/.local/lib/qml` nor the plasmoid package directory.
- **VERIFIED (source, libplasma v6.7.4).** Every applet in a process shares one plain
  `QQmlEngine` (`SharedQmlEnginePrivate::engine()`, `std::make_shared<QQmlEngine>()`).
  libplasma, plasma-workspace `shell/` and `applets/systemtray/` at v6.7.4 contain no
  `addImportPath` or `setImportPathList` calls. The applet's main QML is loaded from
  `applet->mainScript()`, the package's file URL.
- **VERIFIED (runtime).** The running plasmashell's environment has no `QML_IMPORT_PATH` and
  no `QT_PLUGIN_PATH`.
- **VERIFIED (source).** The system tray offers any applet whose metadata has a non-empty
  `X-Plasma-NotificationAreaCategory`. It finds them through
  `PluginLoader::listAppletMetaData`, which includes KPackage `Plasma/Applet` packages, so
  user-local `~/.local/share/plasma/plasmoids` packages are included.
- **LIKELY.** A tray-embedded applet **does not** find a QML module installed under
  `~/.local/lib/qml`, or anywhere outside `/usr/lib/qt6/qml`. It would need one of these:
  - `QML_IMPORT_PATH` exported into plasmashell's environment, for example from a
    `~/.config/plasma-workspace/env/*.sh` script, which takes effect at the next login;
  - a system-wide install to `/usr/lib/qt6/qml`.
- **VERIFIED (plasmawindowed, `QML_IMPORT_PATH` unset).** A plasmoid can carry its own native
  plugin. The module (qmldir, `.so`, qmltypes) was copied into the package at
  `contents/lib/traytrayspike/` and imported with `import "../lib/traytrayspike"`. With no
  import path set, the plugin loaded, frames displayed, the hello and ack frames reached the
  server and the same rejections were counted. Package id: `org.traytray.spike.m0bundled`,
  built by `make-bundled.sh`.
- **LIKELY.** The same bundled package also works inside the system tray. The tray uses the
  same shared engine and loads the applet from the same package file URL. Not observed.
- **UNKNOWN.** What happens if two different paths provide the same module URI to the one
  shared engine (for example a bundled copy plus a system copy after an upgrade). Also unknown
  is whether plasmashell picks up a replaced `.so` without a restart; it most likely does not,
  since a loaded plugin is not unloaded.

## Consequences for docs/design.md

1. **Drawer (KDE).** "the core registers as a StatusNotifierHost with Plasma's watcher, so it
   gets live items" holds. Add that the watcher's signals must be matched on
   `path=/StatusNotifierWatcher` (kded emits each one on three paths), and that item
   properties are optional and read one by one.
2. **M0.1 milestone line.** It can be marked passed once a testlog entry is added. The test
   covered one real item; coverage of libappindicator, Electron and Qt items is still open.
3. **shells/kde packaging.** "a compiled C++ QML plugin" needs a stated install location,
   because a user-local `~/.local/lib/qml` install is likely invisible to plasmashell. Options,
   in order of least friction:
   - (a) ship the QML module inside the plasmoid package and import it by relative path
     (VERIFIED in plasmawindowed, LIKELY in the tray);
   - (b) a system package that installs into `/usr/lib/qt6/qml`;
   - (c) a login-time `QML_IMPORT_PATH`.

   Option (a) means a Qt-version-specific `.so` lives in the package. The shell must be
   rebuilt for each Qt minor version and reinstalled, and plasmashell must restart to load a
   new build. Record the choice in an ADR.
4. **Metadata.** To appear in the tray, the applet's metadata.json needs
   `X-Plasma-NotificationAreaCategory`. In Plasma 6 the old `X-Plasma-NotificationArea` key is
   not what the tray checks.
5. **Framing limit.** The 256 KB per-frame limit can be enforced in the shell plugin without
   buffering, as shown here. Rejecting at the moment the line crosses the limit, then
   discarding up to the newline, keeps the stream in sync. The design could state that an
   oversized frame is dropped and the connection kept, rather than closed. Either is
   implementable; this spike kept the connection.
6. **Shell verification.** plasmawindowed works as a test harness for the applet without
   touching the panel. It writes `~/.config/plasmawindowed-appletsrc`, `plasmawindowedrc` and
   `~/.cache/plasmawindowed/`, so test scripts should clean those up.

## Cleanup done

- `kpackagetool6 --remove` for `org.traytray.spike.m0` and `org.traytray.spike.m0bundled`. The
  empty `~/.local/share/plasma/plasmoids` directory that the install created was removed.
- All plasmawindowed instances (run under `timeout`), the NDJSON servers, the spike host and
  the dbus-monitor capture have exited. No `plasmawindowed`, `ndjson_server`, `sni-host-spike`
  or spike `dbus-monitor` process remains.
- The test socket was removed by the server on exit; `$XDG_RUNTIME_DIR` has no spike socket.
- Removed `~/.config/plasmawindowed-appletsrc`, `~/.config/plasmawindowedrc` and
  `~/.cache/plasmawindowed/`. Their birth times showed they were created by these runs.
- plasmashell and kwin were not restarted, and no Plasma config was edited. The watcher's
  final state matches the start: one item, plasmashell's host only.
- Side effect left behind: four core dumps of processes this spike started, all from tools run
  without `WAYLAND_DISPLAY` set. They are a `plasmashell --version` probe, two
  `plasmawindowed --help` probes and one `spectacle`. They were standalone processes, not the
  running shell, and are left in systemd-coredump storage.
- Build outputs stay in the gitignored `sni-host/target/` and `kde-plugin/build/`.
  `sni-host/Cargo.lock` was generated by the offline build. Nothing was committed.

## Reviewer corrections (independent check, same day)

- Q7: "never held in memory" applies to the plugin's own line buffer only. QLocalSocket's internal
  read buffer is unbounded by default and was not measured; the real plugin must set
  `readBufferSize` or read eagerly.
- Q8, bundled variant: only 2 of the 3 rejection cases were exercised (the client closed after the
  4 MiB frame; the non-JSON case and the rev 10/15 acks were not seen). The finding that the
  bundled module loads with no import path stands.
- Q1: "the watcher kept IsStatusNotifierHostRegistered = true" was not observed (the property was
  read once, before registering). The plasmashell PID and host-name claims were confirmed
  afterwards by the reviewer, not by saved evidence.
- Q3 is plumbing-only: every signal came from the spike's own test item.
- The run that produced the Q4-Q7 evidence used an earlier revision of main.qml; one other run
  failed to connect ("Invalid name") and is not part of the PASS evidence.
- Cleanup: five core dumps (not four) came from tools started without a Wayland display.
