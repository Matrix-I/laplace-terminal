//! POSIX pseudo-terminal: spawns the user's shell on the slave side and runs
//! the reader thread that parses output straight into the shared grid.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::mpsc;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::grid::Grid;

#[derive(Debug)]
pub(crate) enum WritePayload {
    Inline(u8, [u8; 15]),
    Heap(Vec<u8>),
}

impl WritePayload {
    #[inline]
    pub fn from_slice(data: &[u8]) -> Self {
        if data.len() <= 15 {
            let mut buf = [0u8; 15];
            buf[..data.len()].copy_from_slice(data);
            WritePayload::Inline(data.len() as u8, buf)
        } else {
            WritePayload::Heap(data.to_vec())
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            WritePayload::Inline(len, buf) => &buf[..*len as usize],
            WritePayload::Heap(v) => v.as_slice(),
        }
    }
}

/// Dropping a `Pty` closes the master side and wakes both I/O threads through the
/// shutdown pipe; once their duplicated descriptors are closed the kernel hangs up
/// the terminal (SIGHUP to the shell and its foreground job) and the reader thread
/// reaps the child, so no zombie is left behind.
pub struct Pty {
    master: OwnedFd,
    pub pid: libc::pid_t,
    /// Bytes for the shell, written by the writer thread so the UI never blocks.
    tx: mpsc::Sender<WritePayload>,
    /// Read end of the shutdown pipe, polled by the I/O threads.
    shutdown_rx: Arc<OwnedFd>,
    /// Write end of the shutdown pipe: closing it (on drop) signals shutdown.
    _shutdown_tx: OwnedFd,
    /// Whether the reader thread was spawned (which takes over reaping the child).
    reader_spawned: std::sync::atomic::AtomicBool,
}

impl Drop for Pty {
    fn drop(&mut self) {
        if !self.reader_spawned.load(std::sync::atomic::Ordering::SeqCst) {
            reap_child(self.pid);
        }
    }
}

fn winsize(cols: u16, rows: u16, cell_w: u32, cell_h: u32) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: (cols as u32 * cell_w).min(u16::MAX as u32) as u16,
        ws_ypixel: (rows as u32 * cell_h).min(u16::MAX as u32) as u16,
    }
}

fn cstr(s: impl Into<Vec<u8>>) -> CString {
    CString::new(s).unwrap_or_else(|_| CString::new("?").unwrap())
}

/// Shell integration for `shell_name`, unless disabled (`None`).
fn shell_integration_for(
    shell_name: &str,
    kind: Option<crate::config::ShellIntegration>,
) -> Option<crate::shell_integration::Injection> {
    crate::shell_integration::prepare(shell_name, kind?, |k| std::env::var(k).ok())
}

impl Pty {
    pub fn spawn_in(
        cols: u16,
        rows: u16,
        cell_w: u32,
        cell_h: u32,
        cwd: Option<&str>,
        integration: Option<(crate::config::ShellIntegration, crate::config::ShellFeatures)>,
    ) -> io::Result<Pty> {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh".into() } else { "/bin/bash".into() });
        let shell_name = shell.rsplit('/').next().unwrap_or("sh").to_string();

        // Everything the child needs is prepared before fork(): after fork only
        // async-signal-safe calls are allowed.
        let path = cstr(shell.as_str());
        let injection = shell_integration_for(&shell_name, integration.map(|(kind, _)| kind));
        // A leading '-' in argv[0] makes a login shell; injected bash emulates one.
        let args: Vec<CString> = match injection.as_ref().and_then(|i| i.argv.as_ref()) {
            Some(a) => a.iter().map(|s| cstr(s.as_str())).collect(),
            None => vec![cstr(format!("-{shell_name}"))],
        };
        let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());
        let injected_env = injection.map(|i| i.env).unwrap_or_default();

        let target_dir = cwd
            .filter(|d| !d.is_empty() && std::path::Path::new(d).is_dir())
            .map( String::from )
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".into()));

        let mut env: Vec<CString> = std::env::vars()
            .filter(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "TERM" | "COLORTERM" | "TERM_PROGRAM" | "TERM_PROGRAM_VERSION" | "PWD" | "OLDPWD"
                ) && !injected_env.iter().any(|(ik, _)| ik == k)
                    && !k.starts_with("GHOSTTY_")
                    && !k.starts_with("KITTY_")
                    && !k.starts_with("ITERM_")
                    && !k.starts_with("WEZTERM_")
            })
            .map(|(k, v)| cstr(format!("{k}={v}")))
            .collect();
        env.push(cstr("TERM=xterm-256color"));
        env.push(cstr("COLORTERM=truecolor"));
        env.push(cstr("TERM_PROGRAM=litty"));
        env.push(cstr(concat!("TERM_PROGRAM_VERSION=", env!("CARGO_PKG_VERSION"))));
        for (k, v) in &injected_env {
            env.push(cstr(format!("{k}={v}")));
        }
        if let Some((_, feats)) = integration {
            env.push(cstr("LITTY_SHELL_INTEGRATION=1"));
            env.push(cstr("LAPLACE_SHELL_INTEGRATION=1"));
            let mut list = Vec::new();
            if feats.cursor { list.push("cursor"); }
            if feats.sudo { list.push("sudo"); }
            if feats.title { list.push("title"); }
            if !list.is_empty() {
                let joined = list.join(",");
                env.push(cstr(format!("LITTY_SHELL_INTEGRATION_FEATURES={joined}")));
                env.push(cstr(format!("LAPLACE_SHELL_INTEGRATION_FEATURES={joined}")));
            }
        }
        env.push(cstr(format!("PWD={target_dir}")));
        if std::env::var_os("LANG").is_none() && std::env::var_os("LC_ALL").is_none() {
            env.push(cstr("LANG=en_US.UTF-8"));
        }
        let mut envp: Vec<*const libc::c_char> = env.iter().map(|s| s.as_ptr()).collect();
        envp.push(std::ptr::null());
        let start_dir = cstr(target_dir);
        let max_fd = (unsafe { libc::sysconf(libc::_SC_OPEN_MAX) }).clamp(256, 4096) as libc::c_int;

        let mut ws = winsize(cols, rows, cell_w, cell_h);
        let (mut master, mut slave): (libc::c_int, libc::c_int) = (-1, -1);
        // SAFETY: plain libc FFI with valid pointers.
        unsafe {
            if libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                // `*mut winsize` on macOS, `*const winsize` on Linux.
                std::ptr::addr_of_mut!(ws) as _,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }

            // UTF-8 aware line editing (correct backspace over multibyte chars).
            let mut tio: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(slave, &mut tio) == 0 {
                tio.c_iflag |= libc::IUTF8;
                libc::tcsetattr(slave, libc::TCSANOW, &tio);
            }

            let pid = libc::fork();
            if pid < 0 {
                let e = io::Error::last_os_error();
                libc::close(master);
                libc::close(slave);
                return Err(e);
            }
            if pid == 0 {
                // ---- child ----
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);
                // Close all inherited file descriptors >= 3 so no PTY masters, sockets,
                // or driver handles leak into the shell.
                #[cfg(all(target_os = "linux", target_env = "gnu"))]
                if libc::close_range(3, !0, 0) != 0 {
                    for fd in 3..max_fd {
                        libc::close(fd);
                    }
                }
                #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
                for fd in 3..max_fd {
                    libc::close(fd);
                }
                // Rust ignores SIGPIPE; ignored dispositions survive exec, so reset them.
                for sig in [libc::SIGPIPE, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGCHLD, libc::SIGHUP] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
                libc::chdir(start_dir.as_ptr());
                libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                libc::_exit(127);
            }

            // ---- parent ----
            libc::close(slave);
            let master = OwnedFd::from_raw_fd(master);
            set_cloexec(master.as_raw_fd());
            // Non-blocking master: the I/O threads poll it together with the shutdown pipe.
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);

            let mut pipe_fds = [-1; 2];
            if libc::pipe(pipe_fds.as_mut_ptr()) != 0 {
                let e = io::Error::last_os_error();
                drop(master);
                reap_child(pid);
                return Err(e);
            }
            let shutdown_rx = Arc::new(OwnedFd::from_raw_fd(pipe_fds[0]));
            let shutdown_tx = OwnedFd::from_raw_fd(pipe_fds[1]);
            set_cloexec(shutdown_rx.as_raw_fd());
            set_cloexec(shutdown_tx.as_raw_fd());

            let (tx, rx) = mpsc::channel::<WritePayload>();
            let writer_fd = match dup_cloexec(master.as_raw_fd()) {
                Ok(fd) => fd,
                Err(e) => {
                    drop(master);
                    reap_child(pid);
                    return Err(e);
                }
            };
            let writer_shutdown = shutdown_rx.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("pty-writer".into())
                .stack_size(64 * 1024)
                .spawn(move || {
                    while let Ok(data) = rx.recv() {
                        if !write_all(writer_fd.as_raw_fd(), data.as_slice(), writer_shutdown.as_raw_fd()) {
                            return;
                        }
                    }
                })
            {
                drop(master);
                reap_child(pid);
                return Err(e);
            }

            Ok(Pty {
                master,
                pid,
                tx,
                shutdown_rx,
                _shutdown_tx: shutdown_tx,
                reader_spawned: std::sync::atomic::AtomicBool::new(false),
            })
        }
    }

    /// Query the current working directory of the foreground process group (or
    /// the login shell itself) directly from the OS kernel.
    /// A program other than the shell owns the terminal (the foreground
    /// process group is not the shell's).
    pub fn has_running_process(&self) -> bool {
        // SAFETY: `tcgetpgrp` only reads the terminal's foreground group.
        let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        pgrp > 0 && pgrp != self.pid
    }

    pub fn cwd(&self) -> Option<String> {
        let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        if pgrp > 0 && pgrp != self.pid {
            if let Some(dir) = pid_cwd(pgrp) {
                return Some(dir);
            }
        }
        pid_cwd(self.pid)
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) {
        let ws = winsize(cols, rows, cell_w, cell_h);
        // SAFETY: valid fd and pointer; the kernel sends SIGWINCH to the shell.
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws);
        }
    }

    /// Queue bytes for the shell. Uses WritePayload to avoid heap allocation on keystrokes.
    pub fn write(&self, data: &[u8]) {
        let _ = self.tx.send(WritePayload::from_slice(data));
    }

    /// Spawn Thread 1: reads into a fixed 64 KB stack buffer, parsed by `vte`
    /// directly into the locked grid. Coalesces back-to-back shell writes
    /// (e.g. `\r\n` + `PROMPT_SP` + multi-line Powerline prompt) so intermediate
    /// incomplete states don't trigger redundant GPU frames. When the shell exits
    /// or the `Pty` is dropped, the thread closes its descriptor and reaps the child.
    pub fn spawn_reader(
        &self,
        grid: Arc<Mutex<Grid>>,
        notify: impl Fn() + Send + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        let fd = match dup_cloexec(self.master.as_raw_fd()) {
            Ok(fd) => fd,
            Err(e) => {
                reap_child(self.pid);
                return Err(e);
            }
        };
        let shutdown = self.shutdown_rx.clone();
        let tx = self.tx.clone();
        let pid = self.pid;
        struct ChildReaper(libc::pid_t);
        impl Drop for ChildReaper {
            fn drop(&mut self) {
                reap_child(self.0);
            }
        }
        if let Err(e) = std::thread::Builder::new()
            .name("pty-reader".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let _reaper = ChildReaper(pid);
                let raw = fd.as_raw_fd();
                let mut buf = [0u8; 65536];
                let mut parser = vte::Parser::new();
                let mut reply: Vec<u8> = Vec::with_capacity(256);
                loop {
                    if !wait_ready(raw, libc::POLLIN, shutdown.as_raw_fd()) {
                        break; // Pty dropped
                    }
                    // SAFETY: buf is a valid writable buffer of buf.len() bytes.
                    let n = unsafe { libc::read(raw, buf.as_mut_ptr().cast(), buf.len()) };
                    if n < 0 {
                        let kind = io::Error::last_os_error().kind();
                        if matches!(kind, io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) {
                            continue;
                        }
                        break; // EIO: slave closed (shell exited)
                    }
                    if n == 0 {
                        break;
                    }

                    let mut chunk_len = n as usize;
                    let batch_start = std::time::Instant::now();
                    loop {
                        let sync_active = {
                            let mut g = grid.lock();
                            for &b in &buf[..chunk_len] {
                                g.feed_byte(&mut parser, b);
                            }
                            if !g.response.is_empty() {
                                std::mem::swap(&mut g.response, &mut reply);
                            }
                            g.sync_output
                        };
                        if !reply.is_empty() {
                            // Same queue as keyboard input, so replies never interleave with a paste.
                            let _ = tx.send(WritePayload::Heap(std::mem::take(&mut reply)));
                        }

                        // Never hold output back waiting for the next prompt: prompt themes take
                        // anywhere from a few ms to hundreds (e.g. git status), and holding the
                        // newline after Enter until then makes typing feel uneven. Only an
                        // application's synchronized-output frame (DECSET 2026) is waited for.
                        let max_batch_ms = if sync_active { 45 } else { 3 };
                        if batch_start.elapsed().as_millis() >= max_batch_ms {
                            break;
                        }
                        let wait_ms = if sync_active { 6 } else { 0 };
                        let mut pfd = libc::pollfd { fd: raw, events: libc::POLLIN, revents: 0 };
                        let pr = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
                        if pr <= 0 || (pfd.revents & libc::POLLIN) == 0 {
                            break;
                        }
                        let n2 = unsafe { libc::read(raw, buf.as_mut_ptr().cast(), buf.len()) };
                        if n2 <= 0 {
                            break;
                        }
                        chunk_len = n2 as usize;
                    }
                    notify();
                }
                // Release our handles on the master first so the terminal can hang up
                // (the writer thread exits once every sender is gone).
                drop(tx);
                drop(fd);
                drop(_reaper);
                on_exit();
            })
        {
            reap_child(self.pid);
            return Err(e);
        }
        self.reader_spawned.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

fn set_cloexec(fd: RawFd) {
    // SAFETY: fcntl on a descriptor we own.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
}

fn dup_cloexec(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: dup of a valid fd; the returned descriptor is owned by the caller.
    let new_fd = unsafe { libc::dup(fd) };
    if new_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    set_cloexec(new_fd);
    Ok(unsafe { OwnedFd::from_raw_fd(new_fd) })
}

/// Block until `fd` reports `events` (or hangs up / errors). Returns `false` once the
/// shutdown pipe becomes readable or is closed.
fn wait_ready(fd: RawFd, events: libc::c_short, shutdown: RawFd) -> bool {
    loop {
        let mut fds = [
            libc::pollfd { fd, events, revents: 0 },
            libc::pollfd { fd: shutdown, events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: fds is a valid array of two pollfd structs.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if r < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if fds[1].revents != 0 {
            return false;
        }
        if fds[0].revents != 0 {
            return true;
        }
    }
}

/// Write all of `data` to the non-blocking master, waiting for buffer space as needed.
/// Returns `false` if the terminal is gone or shutdown was requested.
fn write_all(fd: RawFd, mut data: &[u8], shutdown: RawFd) -> bool {
    while !data.is_empty() {
        // SAFETY: data is a valid readable slice.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            match io::Error::last_os_error().kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    if !wait_ready(fd, libc::POLLOUT, shutdown) {
                        return false;
                    }
                    continue;
                }
                _ => return false,
            }
        }
        data = &data[n as usize..];
    }
    true
}

/// Reap the shell. Give it a moment to exit on hangup, then SIGKILL so a shell that
/// ignores SIGHUP can never linger as an orphan. Safe to signal `pid` here: it cannot
/// be reused until it has been waited for, and this is the only place that waits.
fn reap_child(pid: libc::pid_t) {
    let mut status = 0;
    for _ in 0..40 {
        // SAFETY: plain libc FFI with a valid pointer.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r != 0 {
            return; // reaped, or not our child (ECHILD)
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, &mut status, 0);
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn pid_cwd(pid: libc::pid_t) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    unsafe {
        let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        let ret = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast::<libc::c_void>(),
            size,
        );
        if ret != size {
            return None;
        }
        let cstr = std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast::<libc::c_char>());
        let s = cstr.to_str().ok()?.to_string();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn pid_cwd(pid: libc::pid_t) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_pty_before_spawn_reader_reaps_the_shell() {
        let pty = Pty::spawn_in(80, 24, 8, 16, None, None).expect("spawn pty");
        let pid = pty.pid;
        drop(pty);
        let mut status = 0;
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(r, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[test]
    fn dropping_pty_never_blocks_and_reaps_the_shell() {
        let pty = Pty::spawn_in(80, 24, 8, 16, None, None).expect("spawn pty");
        let pid = pty.pid;
        let (tx, rx) = std::sync::mpsc::channel();
        pty.spawn_reader(Arc::new(Mutex::new(Grid::new(80, 24))), || {}, move || {
            let _ = tx.send(());
        })
        .expect("spawn reader");

        // Far more than the kernel PTY buffer: must be queued, not block the caller.
        let payload = vec![b'x'; 4 << 20];
        let start = std::time::Instant::now();
        pty.write(&payload);
        assert!(start.elapsed() < std::time::Duration::from_millis(200));

        drop(pty);
        rx.recv_timeout(std::time::Duration::from_secs(10)).expect("reader thread exits");
        // Reaped: the pid no longer exists as our child (not even as a zombie).
        let r = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(r, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[test]
    fn shell_exit_is_reported_and_reaped() {
        let pty = Pty::spawn_in(80, 24, 8, 16, None, None).expect("spawn pty");
        let pid = pty.pid;
        let (tx, rx) = std::sync::mpsc::channel();
        pty.spawn_reader(Arc::new(Mutex::new(Grid::new(80, 24))), || {}, move || {
            let _ = tx.send(());
        })
        .expect("spawn reader");
        pty.write(b"exit\r");
        rx.recv_timeout(std::time::Duration::from_secs(10)).expect("on_exit fires");
        let r = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(r, -1);
        drop(pty);
    }

    #[test]
    fn output_after_enter_is_not_held_back_waiting_for_the_prompt() {
        let pty = Pty::spawn_in(80, 24, 8, 16, None, None).expect("spawn pty");
        let grid = Arc::new(Mutex::new(Grid::new(80, 24)));
        let screen = |g: &Grid| -> String {
            let row = |r: usize| -> String { g.display_row(r).iter().map(|c| char::from_u32(c.codepoint).filter(|&c| c != '\0').unwrap_or(' ')).collect() };
            (0..24).map(|r| row(r).trim_end().to_string()).collect::<Vec<_>>().join("\n")
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let notify_grid = grid.clone();
        pty.spawn_reader(grid, move || {
            let _ = tx.send(screen(&notify_grid.lock()));
        }, || {})
        .expect("spawn reader");

        // Let the shell finish starting up before typing.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        while rx.try_recv().is_ok() {}
        // Like zsh after Enter: bracketed paste goes off, a newline is echoed, and the
        // prompt follows once its precmd finishes (10ms here; agnoster's git status
        // takes 40-200ms). `%s%s` keeps the markers out of the echoed command line.
        pty.write(b"printf '\\033[?2004h'; sleep 0.3; printf '\\033[?2004l\\r\\n%s%s' AF TER; sleep 0.01; printf '%s%s' PRO MPT\r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut frames = Vec::new();
        while let Ok(text) = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            let done = text.contains("PROMPT");
            frames.push(text);
            if done {
                break;
            }
        }
        assert!(frames.last().is_some_and(|t| t.contains("PROMPT")), "prompt never arrived");
        assert!(
            frames.iter().any(|t| t.contains("AFTER") && !t.contains("PROMPT")),
            "the newline after Enter was only shown together with the prompt"
        );
    }

    #[test]
    fn current_process_cwd_resolves() {
        let pid = unsafe { libc::getpid() };
        let cwd = pid_cwd(pid).expect("pid_cwd for current process");
        assert!(std::path::Path::new(&cwd).is_dir());
    }
}
