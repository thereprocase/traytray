//! Spike M0.7: a std-only named pipe server and client for Windows, cross-built from Linux.
//!
//! Server: creates `\\.\pipe\traytray-spike-<random>` with a DACL that grants only the current
//! user, accepts one client, reads the client's PID from the kernel, checks the client's token
//! user against its own, and exchanges one newline-delimited JSON frame each way.
//! Client: connects with identification-only impersonation, checks the server PID, sends one
//! frame and prints the reply.
//!
//! No crates: every Windows call is declared below by hand.

#[cfg(not(windows))]
fn main() {
    eprintln!("this spike only runs on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::run());
}

#[cfg(windows)]
mod win {
    use std::ffi::{c_void, OsStr};
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    type Handle = *mut c_void;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

    #[repr(C)]
    struct SecurityAttributes {
        n_length: u32,
        security_descriptor: *mut c_void,
        inherit_handle: i32,
    }

    const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    const PIPE_TYPE_BYTE: u32 = 0x0;
    const PIPE_READMODE_BYTE: u32 = 0x0;
    const PIPE_WAIT: u32 = 0x0;
    const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x8;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER_CLASS: u32 = 1;
    const SDDL_REVISION_1: u32 = 1;
    const SE_KERNEL_OBJECT: u32 = 6;
    const OWNER_SECURITY_INFORMATION: u32 = 0x1;
    const DACL_SECURITY_INFORMATION: u32 = 0x4;
    const LABEL_SECURITY_INFORMATION: u32 = 0x10;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x2;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn GetCurrentThread() -> Handle;
        fn GetLastError() -> u32;
        fn CloseHandle(h: Handle) -> i32;
        fn LocalFree(p: *mut c_void) -> *mut c_void;
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer: u32,
            in_buffer: u32,
            default_timeout: u32,
            sa: *const SecurityAttributes,
        ) -> Handle;
        fn ConnectNamedPipe(h: Handle, overlapped: *mut c_void) -> i32;
        fn DisconnectNamedPipe(h: Handle) -> i32;
        fn GetNamedPipeClientProcessId(h: Handle, pid: *mut u32) -> i32;
        fn GetNamedPipeClientSessionId(h: Handle, session: *mut u32) -> i32;
        fn GetNamedPipeServerProcessId(h: Handle, pid: *mut u32) -> i32;
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sa: *const SecurityAttributes,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
        fn WaitNamedPipeW(name: *const u16, timeout_ms: u32) -> i32;
        fn ReadFile(h: Handle, buf: *mut u8, len: u32, read: *mut u32, ov: *mut c_void) -> i32;
        fn WriteFile(h: Handle, buf: *const u8, len: u32, written: *mut u32, ov: *mut c_void)
            -> i32;
        fn FlushFileBuffers(h: Handle) -> i32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn OpenThreadToken(thread: Handle, access: u32, open_as_self: i32, token: *mut Handle)
            -> i32;
        fn GetTokenInformation(
            token: Handle,
            class: u32,
            buf: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: *mut c_void, out: *mut *mut u16) -> i32;
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl: *const u16,
            revision: u32,
            sd: *mut *mut c_void,
            sd_len: *mut u32,
        ) -> i32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd: *mut c_void,
            revision: u32,
            info: u32,
            out: *mut *mut u16,
            out_len: *mut u32,
        ) -> i32;
        fn GetSecurityInfo(
            handle: Handle,
            object_type: u32,
            info: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            sd: *mut *mut c_void,
        ) -> u32;
        fn ImpersonateNamedPipeClient(h: Handle) -> i32;
        fn RevertToSelf() -> i32;
    }

    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(alg: *mut c_void, buf: *mut u8, len: u32, flags: u32) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Reads a NUL-terminated wide string allocated by the system, then frees it with LocalFree.
    unsafe fn take_local_wide(p: *mut u16) -> String {
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        LocalFree(p as *mut c_void);
        s
    }

    fn last_error(what: &str) -> String {
        format!("{what} failed, GetLastError={}", unsafe { GetLastError() })
    }

    /// String SID of a token's user. The token handle is closed by the caller.
    unsafe fn token_user_sid(token: Handle) -> Result<String, String> {
        // u64 backing keeps the TOKEN_USER structure (which starts with a pointer) aligned.
        let mut buf = vec![0u64; 64];
        let mut needed = 0u32;
        if GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * 8) as u32,
            &mut needed,
        ) == 0
        {
            return Err(last_error("GetTokenInformation(TokenUser)"));
        }
        let sid = *(buf.as_ptr() as *const *mut c_void);
        let mut out: *mut u16 = null_mut();
        if ConvertSidToStringSidW(sid, &mut out) == 0 {
            return Err(last_error("ConvertSidToStringSidW"));
        }
        Ok(take_local_wide(out))
    }

    fn current_user_sid() -> Result<String, String> {
        unsafe {
            let mut token: Handle = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(last_error("OpenProcessToken"));
            }
            let r = token_user_sid(token);
            CloseHandle(token);
            r
        }
    }

    /// The connected client's user SID, read by impersonating it at identification level
    /// (enough to query the token; it grants no access as the client).
    fn client_user_sid(pipe: Handle) -> Result<String, String> {
        unsafe {
            if ImpersonateNamedPipeClient(pipe) == 0 {
                return Err(last_error("ImpersonateNamedPipeClient"));
            }
            let mut token: Handle = null_mut();
            let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token);
            let err = GetLastError();
            RevertToSelf();
            if opened == 0 {
                return Err(format!("OpenThreadToken failed, GetLastError={err}"));
            }
            let r = token_user_sid(token);
            CloseHandle(token);
            r
        }
    }

    fn random_hex(bytes: usize) -> Result<String, String> {
        let mut buf = vec![0u8; bytes];
        let status = unsafe {
            BCryptGenRandom(
                null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(format!("BCryptGenRandom failed, NTSTATUS=0x{status:08x}"));
        }
        Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Owner, DACL and label of a kernel object as SDDL, read back from the live handle.
    fn handle_sddl(h: Handle) -> Result<String, String> {
        unsafe {
            let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
            let mut sd: *mut c_void = null_mut();
            let rc = GetSecurityInfo(
                h,
                SE_KERNEL_OBJECT,
                info,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut sd,
            );
            if rc != 0 {
                return Err(format!("GetSecurityInfo failed, rc={rc}"));
            }
            let mut out: *mut u16 = null_mut();
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                info,
                &mut out,
                null_mut(),
            );
            LocalFree(sd);
            if ok == 0 {
                return Err(last_error("ConvertSecurityDescriptorToStringSecurityDescriptorW"));
            }
            Ok(take_local_wide(out))
        }
    }

    /// Reads bytes until the first newline. One frame per direction is all this spike needs,
    /// so there is no buffering across frames.
    fn read_line(h: Handle, limit: usize) -> Result<String, String> {
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let mut n = 0u32;
            if unsafe { ReadFile(h, byte.as_mut_ptr(), 1, &mut n, null_mut()) } == 0 {
                return Err(last_error("ReadFile"));
            }
            if n == 0 {
                return Err("end of stream before newline".into());
            }
            if byte[0] == b'\n' {
                break;
            }
            out.push(byte[0]);
            if out.len() > limit {
                return Err(format!("frame longer than {limit} bytes"));
            }
        }
        String::from_utf8(out).map_err(|_| "frame is not UTF-8".to_string())
    }

    fn write_all(h: Handle, data: &[u8]) -> Result<(), String> {
        let mut off = 0;
        while off < data.len() {
            let mut n = 0u32;
            let rest = &data[off..];
            if unsafe { WriteFile(h, rest.as_ptr(), rest.len() as u32, &mut n, null_mut()) } == 0 {
                return Err(last_error("WriteFile"));
            }
            off += n as usize;
        }
        Ok(())
    }

    fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    /// Pulls an unsigned integer field out of a flat JSON object. Enough for the spike's own
    /// frames; the real core uses a proper parser.
    fn json_u32_field(frame: &str, key: &str) -> Option<u32> {
        let pat = format!("\"{key}\":");
        let start = frame.find(&pat)? + pat.len();
        let digits: String = frame[start..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    }

    fn log(line: &str) {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn usage() -> i32 {
        eprintln!(
            "usage:\n  traytray-pipe-spike server [--name NAME] [--deny-network] [--control-allow-system]\n  traytray-pipe-spike client --name NAME [--message TEXT]"
        );
        2
    }

    pub fn run() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut name: Option<String> = None;
        let mut message = "hello from client".to_string();
        let mut deny_network = false;
        let mut allow_system = false;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--name" if i + 1 < args.len() => {
                    name = Some(args[i + 1].clone());
                    i += 2;
                }
                "--message" if i + 1 < args.len() => {
                    message = args[i + 1].clone();
                    i += 2;
                }
                "--deny-network" => {
                    deny_network = true;
                    i += 1;
                }
                "--control-allow-system" => {
                    allow_system = true;
                    i += 1;
                }
                _ => return usage(),
            }
        }
        let result = match args.first().map(String::as_str) {
            Some("server") => server(name, deny_network, allow_system),
            Some("client") => match name {
                Some(n) => client(&n, &message),
                None => return usage(),
            },
            _ => return usage(),
        };
        match result {
            Ok(()) => 0,
            Err(e) => {
                log(&format!("ERROR {e}"));
                1
            }
        }
    }

    fn pipe_path(name: &str) -> String {
        if name.starts_with(r"\\.\pipe\") {
            name.to_string()
        } else {
            format!(r"\\.\pipe\{name}")
        }
    }

    fn server(name: Option<String>, deny_network: bool, allow_system: bool) -> Result<(), String> {
        let name = match name {
            Some(n) => n,
            None => format!("traytray-spike-{}", random_hex(8)?),
        };
        let path = pipe_path(&name);
        let sid = current_user_sid()?;
        // P = protected: no inherited entries. GA for the user only; nothing for anyone else.
        // The optional NETWORK deny mirrors robo-rightclick's pipe for comparison.
        let mut sddl = if deny_network {
            format!("D:P(D;;GA;;;NU)(A;;GA;;;{sid})")
        } else {
            format!("D:P(A;;GA;;;{sid})")
        };
        // Control run only: lets LocalSystem through the DACL so the peer check behind it is
        // exercised, and shows that the DACL (not something else) refuses LocalSystem normally.
        if allow_system {
            sddl.push_str("(A;;GA;;;SY)");
        }
        let sddl_w = wide(&sddl);
        let mut sd: *mut c_void = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_w.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        } == 0
        {
            return Err(last_error("ConvertStringSecurityDescriptorToSecurityDescriptorW"));
        }
        let sa = SecurityAttributes {
            n_length: std::mem::size_of::<SecurityAttributes>() as u32,
            security_descriptor: sd,
            inherit_handle: 0,
        };
        let path_w = wide(&path);
        let pipe = unsafe {
            CreateNamedPipeW(
                path_w.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                &sa,
            )
        };
        unsafe { LocalFree(sd) };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateNamedPipeW"));
        }
        let my_pid = std::process::id();
        let mut my_session = 0u32;
        unsafe { ProcessIdToSessionId(my_pid, &mut my_session) };
        log(&format!("server pid={my_pid} session={my_session}"));
        log(&format!("server sid={sid}"));
        log(&format!("server requested_sddl={sddl}"));
        log(&format!("server effective_sddl={}", handle_sddl(pipe)?));
        log(&format!("server listening {path}"));

        let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) };
        if connected == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            let e = last_error("ConnectNamedPipe");
            unsafe { CloseHandle(pipe) };
            return Err(e);
        }
        let outcome = serve_one(pipe, &sid, my_pid);
        unsafe {
            FlushFileBuffers(pipe);
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
        }
        outcome
    }

    fn serve_one(pipe: Handle, server_sid: &str, my_pid: u32) -> Result<(), String> {
        let mut client_pid = 0u32;
        if unsafe { GetNamedPipeClientProcessId(pipe, &mut client_pid) } == 0 {
            return Err(last_error("GetNamedPipeClientProcessId"));
        }
        let mut client_session = 0u32;
        unsafe { GetNamedPipeClientSessionId(pipe, &mut client_session) };
        log(&format!("server client_pid={client_pid} client_session={client_session}"));

        // Windows refuses ImpersonateNamedPipeClient until something has been read from the
        // pipe (ERROR_CANNOT_IMPERSONATE), so the first frame is read before the peer check.
        let frame = read_line(pipe, 256 * 1024)?;
        log(&format!("server received {frame}"));
        let client_sid = client_user_sid(pipe);
        let same_user = matches!(&client_sid, Ok(s) if s == server_sid);
        log(&format!(
            "server client_sid={} same_user={same_user}",
            match &client_sid {
                Ok(s) => s.clone(),
                Err(e) => format!("unavailable ({e})"),
            }
        ));
        if !same_user {
            // The peer credential check: the DACL should already have refused this client.
            write_all(pipe, b"{\"type\":\"error\",\"reason\":\"peer_mismatch\"}\n")?;
            return Err("client token user differs from server user".into());
        }

        let claimed = json_u32_field(&frame, "pid");
        let pid_matches = claimed == Some(client_pid);
        log(&format!(
            "server claimed_pid={} kernel_pid={client_pid} pid_matches={pid_matches}",
            claimed.map_or("none".to_string(), |p| p.to_string())
        ));
        let reply = format!(
            "{{\"type\":\"welcome\",\"server_pid\":{my_pid},\"seen_client_pid\":{client_pid},\"pid_matches\":{pid_matches},\"echo\":\"{}\"}}\n",
            json_escape(&frame)
        );
        write_all(pipe, reply.as_bytes())?;
        log(&format!("server sent {}", reply.trim_end()));
        Ok(())
    }

    fn client(name: &str, message: &str) -> Result<(), String> {
        let path = pipe_path(name);
        let path_w = wide(&path);
        unsafe { WaitNamedPipeW(path_w.as_ptr(), 5000) };
        // Identification level: a server squatting on the name cannot act as this client.
        let h = unsafe {
            CreateFileW(
                path_w.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateFileW(pipe)"));
        }
        let my_pid = std::process::id();
        let mut server_pid = 0u32;
        unsafe { GetNamedPipeServerProcessId(h, &mut server_pid) };
        log(&format!("client pid={my_pid} connected {path} server_pid={server_pid}"));
        let frame = format!(
            "{{\"type\":\"hello\",\"pid\":{my_pid},\"message\":\"{}\"}}\n",
            json_escape(message)
        );
        let result = write_all(h, frame.as_bytes()).and_then(|_| {
            log(&format!("client sent {}", frame.trim_end()));
            let reply = read_line(h, 256 * 1024)?;
            log(&format!("client received {reply}"));
            match json_u32_field(&reply, "server_pid") {
                Some(p) if p == server_pid => {
                    log("client server_pid_matches=true");
                    Ok(())
                }
                other => Err(format!("server_pid mismatch: reply {other:?} kernel {server_pid}")),
            }
        });
        unsafe { CloseHandle(h) };
        result
    }
}
