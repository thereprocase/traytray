# M0.7 and M0.4: named pipe cross-build and Windows 11 tray visibility

Date: 2026-10-03

## Environment

- **Build machine:** the Linux desktop (CachyOS). rustc 1.97.1, cargo 1.97.1, target
  `x86_64-pc-windows-gnu`, linker x86_64-w64-mingw32-gcc 16.2.0. `cargo build --offline`, no
  crates.
- **Test machine:** the Windows 11 test VM. Windows 11 Enterprise Evaluation 25H2, build
  26200.6584, 1280 x 800, no network egress, one local test account (administrator) signed in to
  the desktop. Windows PowerShell 5.1.26100.6584. Defender real-time protection on, engine
  4.18.23110.3, signatures 1.403.7.0 (old; the VM has no egress to update them).
- **How things ran:** ssh commands land in Session 0, so everything that had to be on the
  desktop ran through one-shot scheduled tasks with an interactive-logon principal. The
  LocalSystem client ran through a scheduled task as SYSTEM. Screens were taken host-side with
  QMP `screendump` and read as images; each capture also counted the probe icon's magenta
  pixels (a fully visible 16 x 16 icon is 256).
- Code: `spikes/m0/win-cross/` (pipe spike, `run-case.sh`, VM-side `vm/*.cmd`, `vm/task.ps1`),
  `spikes/m0/win-tray/` (`TrayProbe.cs`, `build-probe.ps1`, `inspect-nis.ps1`, `shot.sh`).
  Raw evidence (logs with SIDs and account names, screens, registry exports) is in
  `spikes/m0/private/` as `m07-*` and `m04-*`.

## M0.7: std-only Rust named pipe, cross-built, run on Windows 11

The program (`traytray-pipe-spike.exe`) has a server mode and a client mode. Every Windows call is
declared by hand with `extern "system"`.

- **Server:** reads its token user SID and builds `D:P(A;;GA;;;<user SID>)` with
  `ConvertStringSecurityDescriptorToSecurityDescriptorW`. It creates `\\.\pipe\traytray-spike-<16 hex>`
  (from `BCryptGenRandom`) with `FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS` and
  one instance. It reads the DACL back from the live handle and accepts one client.
  Then it calls `GetNamedPipeClientProcessId` and `GetNamedPipeClientSessionId`, reads one
  frame, and gets the client's token user with `ImpersonateNamedPipeClient` + `OpenThreadToken`.
  It compares that user and the PID the client claims with what the kernel reports, then replies
  with one frame.
- **Client:** opens the pipe with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, so a server
  squatting on the name cannot impersonate it. It reads `GetNamedPipeServerProcessId`, sends one
  frame and checks the reply.
- Optional flags: `--deny-network` adds `(D;;GA;;;NU)`. `--control-allow-system` adds
  `(A;;GA;;;SY)` for the control run only.

| Question | Result | Evidence |
|---|---|---|
| Cross-builds with `cargo build --offline --release --target x86_64-pc-windows-gnu` | PASS | Builds with no warnings. PE32+ console exe. Imports only KERNEL32, ADVAPI32, bcrypt, ntdll and the UCRT `api-ms-win-crt-*` set. No libgcc or winpthread DLL. |
| Exe size | recorded | 273,408 bytes (267 KiB) with `opt-level="s"`, LTO, strip, `panic="abort"`. Final SHA256 `7f0b638b…de8ff531`. |
| Current-user DACL applied | PASS | Read back from the handle: `O:<user>D:P(A;;FA;;;<user SID>)`. The DACL has one entry, and `GA` is stored as `FA`. |
| Accept one client, kernel PID, one NDJSON frame each way (same user, desktop session) | PASS | Server: `client_pid=3460 client_session=1`, `received {"type":"hello","pid":3460,...}`, `client_sid=<same> same_user=true`, `claimed_pid=3460 kernel_pid=3460 pid_matches=true`. Client: `server_pid=2076`, `received {"type":"welcome","server_pid":2076,"seen_client_pid":3460,"pid_matches":true,...}`, `server_pid_matches=true`, both exit 0. |
| Different principal: LocalSystem | PASS (refused) | Client running as `S-1-5-18`: `CreateFileW(pipe) failed, GetLastError=5`. The server kept waiting. |
| Control: the DACL is what refuses LocalSystem | PASS | Server with `(A;;GA;;;SY)` added. The same SYSTEM client connected. The server's peer check then logged `client_sid=S-1-5-18 same_user=false` and replied `{"type":"error","reason":"peer_mismatch"}`, and the client exited 1. This also exercised the peer check's refusal path, which the DACL otherwise hides. |
| Same user, other logon session (ssh, Session 0, token has NETWORK `S-1-5-2`, high integrity) | **Connects** with the plain user-only DACL | `client_session=0 ... same_user=true pid_matches=true`, frames exchanged. With `--deny-network` the same client got `GetLastError=5`, and a desktop client afterwards still connected (`client_session=1`, frames exchanged). |
| Defender / SmartScreen interference | No interference observed | `Get-MpThreatDetection`: 0 detections. Defender's operational log for the hour shows only 2001 (signature update failed, no egress) and 5007 (config). No prompt was visible in the desktop capture. SmartScreen was **not exercised**: files copied over scp carry no Mark-of-the-Web (no `Zone.Identifier` stream), and every start was from a console or task, not an Explorer double-click. |
| Another user account | not tested | No second interactive account exists on the VM, and none was created. LocalSystem stood in as the other principal. |

Harness finding: `ImpersonateNamedPipeClient` fails until something has been read from the
pipe. The first build checked the peer before reading and logged `client_sid=unavailable`.
Reading the first frame before the peer check fixed it.

**M0.7 overall: PASS.** Frames were observed both ways on the VM. The kernel-reported client
PID matched the PID the client claimed. The user-only DACL refused LocalSystem.

## M0.4: Windows 11 notification-icon visibility and IsPromoted

The probe is a 5 KB WinForms exe compiled on the VM with `Add-Type -OutputType
WindowsApplication`. It shows one `NotifyIcon` with a solid magenta 16 x 16 icon and exits when
a `stop` file appears next to it. Copy A and copy B are the same binary at two new paths.

| Question | Result | Evidence |
|---|---|---|
| (1) Is a brand-new app's icon in the taskbar corner or in the overflow by default? | **Overflow** (PASS: answered) | Probe A: the corner crop shows chevron, OneDrive, network and volume only. Magenta pixels on screen: 0. Opening the chevron showed the magenta icon in the overflow flyout (256 pixels). Probe B, a second new path: also not in the corner (0 pixels), and both icons were in the overflow (512 pixels). |
| (2) Does an entry appear under `HKCU:\Control Panel\NotifyIconSettings\<id>`, and with which values? | PASS | It appears within 5 s of the icon showing, with these values: `UID` (DWORD 1, the NotifyIcon id), `ExecutablePath` (REG_SZ, full exe path), `InitialTooltip` (REG_SZ), and `IconSnapshot` (REG_BINARY, a 119-byte PNG). There is **no `IsPromoted` value** on a new entry. The parent's `UIOrderList` (REG_BINARY) grew from 32 to 48 bytes: the two new key ids were prepended as little-endian QWORDs. |
| (3a) `IsPromoted`=1 / 0 applied live | PASS | With 1, the icon was in the corner within 2 s (magenta 202 to 256, the cursor covered part of it). With 0, it left the corner within 2 s (0 pixels). |
| (3b) after an Explorer restart | PASS | The value was set, then `Stop-Process explorer -Force`, and Explorer restarted itself in the same session. With 1: the value was still 1 and the icon was in the corner (256). With 0: the value was still 0, the corner had no icon (0), and the icon was in the overflow (256). |
| (3c) after sign-out and sign-in | PASS | The value was set, then `logoff`. It read back the same while signed out and after signing back in with the VM tooling (a new session each time), and the probe was relaunched. With 1: the icon was in the corner (256). With 0: the corner had no icon (0) and the icon was in the overflow (256). The probe reused the same key both times (one entry for its path). |

More observations:
- **The key is per exe path.** The same binary at a second path got a new key and a new id.
- **`InitialTooltip` is written only once.** Relaunching probe A with a different tooltip left
  the stored value unchanged.
- **Entries outlive their apps.** One entry belongs to an app that was not running and had no
  `IsPromoted`. The probes' entries stayed after they exited, until this spike removed them.
- **Some entries have no plain path.** `ExecutablePath` can start with a known-folder GUID
  (`{1AC14E77-…}\SecurityHealthSystray.exe`, `{F38BF404-…}\explorer.exe`). These entries carry
  `IconGuid` instead of `UID`, and one has `Publisher`.
- A pre-existing OneDrive entry carries `IsPromoted`=1.
- Caveat: the "Explorer restart" was a forced kill, so Explorer had no chance to write state on
  exit. Sign-out is the graceful-exit case, and the value survived it in both directions.

**M0.4 overall: PASS.** New icons go to the overflow by default. Demote (and Promote) through
`IsPromoted` apply live, and they survive an Explorer restart and a sign-out.

## Consequences for docs/design.md

1. **Local transport (Protocol, "Local transport").** A current-user DACL is not enough on its
   own. The same user's non-interactive logons (ssh or service-style, Session 0, NETWORK SID)
   connect to it. Spell out:
   - The DACL is `D:P(D;;GA;;;NU)(A;;GA;;;<user SID>)`.
   - The pipe is created with `FILE_FLAG_FIRST_PIPE_INSTANCE` (squatting shows as a create
     failure) and `PIPE_REJECT_REMOTE_CLIENTS`.
   - Peer check: `GetNamedPipeClientProcessId` plus the token user from
     `ImpersonateNamedPipeClient`. It is done **after reading the first frame**, because
     Windows refuses impersonation before any read.
   - Decide whether the core also requires `GetNamedPipeClientSessionId` to equal its own
     session.
   - SDK clients open the pipe with `SECURITY_IDENTIFICATION` and check
     `GetNamedPipeServerProcessId`.
   - The design does not name the pipe. The core needs a well-known per-user name (for
     example, one derived from the user SID) that SDKs can find. The spike's random name does
     not do this.
2. **M0.7 can be marked done** once the testlog entry exists. The std-only cross-build runs on
   Windows 11, the exe is 267 KiB, and it needs no MinGW runtime DLLs. Still open: SmartScreen
   on a downloaded (Mark-of-the-Web) exe started from Explorer.
3. **Drawer, Windows.** Demote meets the gate ("survives an Explorer restart and a sign-out"),
   so it can ship. It writes the entry's `IsPromoted` DWORD (0 = overflow, 1 = corner), backs
   up the old value or its absence, and needs no Explorer restart. Add:
   - `focus_or_launch` must resolve known-folder GUID prefixes in `ExecutablePath` and treat a
     missing file as "not launchable".
   - Entries persist for apps that are not running or are uninstalled, so the drawer cannot use
     the registry alone to show what is live.
   - `IconSnapshot` is a PNG the drawer can show.
   - `InitialTooltip` is only the first-seen tooltip, so it is not a reliable label.
4. **Windows shell and robo-rightclick handover.** On a fresh install, Traytray's own icon will
   be in the overflow. "Reports at least one visible shell" needs a definition: the shell
   running versus its icon actually in the taskbar corner. Add a first-run, user-clicked "Show
   in the taskbar corner" that sets `IsPromoted`=1 on the host's own entry. That is the same
   mechanism as Demote, so it is never automatic.
5. **docs/testlog.md** needs a dated entry for these runs before any of the above is cited as
   verified. This spike did not edit docs/testlog.md.

## Cleanup done

- The VM's `NotifyIconSettings` was restored with the account signed out: both probe keys were
  deleted and `UIOrderList` was rewritten to its backed-up 32 bytes. `reg export` SHA256
  matched the pre-test backup (`DCDEA538…`) right after the restore and again after signing
  back in.
- All five `TraytraySpike*` scheduled tasks were removed (0 left). No probe or pipe process was
  left (0). The VM's spike folder was deleted (`Test-Path` false).
- The VM was signed back in, then shut down (`status: down`, no qemu process left). The
  usb-tablet device was added at runtime only and is gone with the VM process.
- Nothing was installed. No Plasma or user config was touched. Nothing was committed.

## Reviewer corrections (independent check, same day)

- M0.7 cases A-C ran on an earlier build than the final exe (rebuilt between case C and case D,
  most likely only to add `--control-allow-system`); that earlier build's hash was not recorded.
- The exe also imports `api-ms-win-core-synch-l1-2-0.dll` (an inbox API set); the "no MinGW
  runtime DLL" conclusion holds.
- The NETWORK-SID attribution for the ssh-session client is inferred from the deny entry working,
  not shown directly.
- M0.4 registry read-backs are from a hand-written observation log; placement results are backed
  by screenshots and pixel counts.
- NotifyIconSettings entries are keyed by exe path, so a Demote does not survive an app update
  that changes its exe path (e.g. versioned install folders). Not tested.
