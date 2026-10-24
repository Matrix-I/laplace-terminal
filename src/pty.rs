//! POSIX pseudo-terminal: spawns the user's shell on the slave side and runs
//! the reader thread that parses output straight into the shared grid.

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::grid::Grid;

pub struct Pty {
    master: RawFd,
    pid: libc::pid_t,
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

impl Pty {
    pub fn spawn_in(
        cols: u16,
        rows: u16,
        cell_w: u32,
        cell_h: u32,
        cwd: Option<&str>,
        shell_features: Option<crate::config::ShellFeatures>,
    ) -> io::Result<Pty> {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh".into() } else { "/bin/bash".into() });
        let shell_name = shell.rsplit('/').next().unwrap_or("sh").to_string();

        // Everything the child needs is prepared before fork(): after fork only
        // async-signal-safe calls are allowed.
        let path = cstr(shell.as_str());
        let argv0 = cstr(format!("-{shell_name}")); // leading '-' = login shell
        let argv = [argv0.as_ptr(), std::ptr::null()];

        let target_dir = cwd
            .filter(|d| !d.is_empty() && std::path::Path::new(d).is_dir())
            .map( String::from )
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".into()));

        let mut env: Vec<CString> = std::env::vars()
            .filter(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "TERM" | "COLORTERM" | "TERM_PROGRAM" | "TERM_PROGRAM_VERSION" | "PWD" | "OLDPWD"
                ) && !k.starts_with("GHOSTTY_")
                    && !k.starts_with("KITTY_")
                    && !k.starts_with("ITERM_")
                    && !k.starts_with("WEZTERM_")
            })
            .map(|(k, v)| cstr(format!("{k}={v}")))
            .collect();
        env.push(cstr("TERM=xterm-256color"));
        env.push(cstr("COLORTERM=truecolor"));
        env.push(cstr("TERM_PROGRAM=laplace"));
        env.push(cstr(concat!("TERM_PROGRAM_VERSION=", env!("CARGO_PKG_VERSION"))));
        if let Some(feats) = shell_features {
            env.push(cstr("LAPLACE_SHELL_INTEGRATION=1"));
            let mut list = Vec::new();
            if feats.cursor { list.push("cursor"); }
            if feats.sudo { list.push("sudo"); }
            if feats.title { list.push("title"); }
            if !list.is_empty() {
                env.push(cstr(format!("LAPLACE_SHELL_INTEGRATION_FEATURES={}", list.join(","))));
            }
        }
        env.push(cstr(format!("PWD={target_dir}")));
        if std::env::var_os("LANG").is_none() && std::env::var_os("LC_ALL").is_none() {
            env.push(cstr("LANG=en_US.UTF-8"));
        }
        let mut envp: Vec<*const libc::c_char> = env.iter().map(|s| s.as_ptr()).collect();
        envp.push(std::ptr::null());
        let start_dir = cstr(target_dir);

        let mut ws = winsize(cols, rows, cell_w, cell_h);
        let (mut master, mut slave): (libc::c_int, libc::c_int) = (-1, -1);
        // SAFETY: plain libc FFI with valid pointers.
        unsafe {
            if libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                &mut ws,
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
                if slave > 2 {
                    libc::close(slave);
                }
                libc::close(master);
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
            libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
            Ok(Pty { master, pid })
        }
    }

    /// Query the current working directory of the foreground process group (or
    /// the login shell itself) directly from the OS kernel.
    pub fn cwd(&self) -> Option<String> {
        let pgrp = unsafe { libc::tcgetpgrp(self.master) };
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
            libc::ioctl(self.master, libc::TIOCSWINSZ as _, &ws);
        }
    }

    pub fn write(&self, data: &[u8]) {
        write_all(self.master, data);
    }

    /// Spawn Thread 1: blocking reads into a fixed 64 KB stack buffer, parsed by
    /// `vte` directly into the locked grid. Coalesces back-to-back shell writes
    /// (e.g. `\r\n` + `PROMPT_SP` + multi-line Powerline prompt) so intermediate
    /// incomplete states don't trigger redundant GPU frames.
    pub fn spawn_reader(
        &self,
        grid: Arc<Mutex<Grid>>,
        notify: impl Fn() + Send + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        // SAFETY: dup of a valid fd; the reader owns its own descriptor.
        let fd = unsafe { libc::dup(self.master) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        std::thread::Builder::new()
            .name("pty-reader".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut buf = [0u8; 65536];
                let mut parser = vte::Parser::new();
                let mut reply: Vec<u8> = Vec::with_capacity(256);
                loop {
                    // SAFETY: buf is a valid writable buffer of buf.len() bytes.
                    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                    if n < 0 {
                        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break; // EIO: slave closed (shell exited)
                    }
                    if n == 0 {
                        break;
                    }

                    let mut chunk_len = n as usize;
                    let mut total_bytes = 0usize;
                    let batch_start = std::time::Instant::now();
                    let mut was_bracketed = grid.lock().bracketed_paste;
                    loop {
                        total_bytes += chunk_len;
                        let (sync_active, now_bracketed) = {
                            let mut g = grid.lock();
                            for &b in &buf[..chunk_len] {
                                parser.advance(&mut *g, b);
                            }
                            if !g.response.is_empty() {
                                std::mem::swap(&mut g.response, &mut reply);
                            }
                            (g.sync_output, g.bracketed_paste)
                        };
                        if now_bracketed {
                            was_bracketed = true;
                        }
                        if !reply.is_empty() {
                            write_all(fd, &reply);
                            reply.clear();
                        }

                        // When the user presses Enter at a prompt, zsh turns off bracketed paste
                        // (`\x1b[?2004l`), emits `PROMPT_SP` (`%\r \r\x1b[J`, < 512 bytes), runs
                        // prompt git status (~25-35ms), and then emits the prompt + `\x1b[?2004h`.
                        // Waiting up to 40ms for the prompt to arrive avoids rendering an
                        // intermediate blank-line jump before the prompt appears.
                        let awaiting_prompt = was_bracketed && !now_bracketed && total_bytes < 512;
                        let max_batch_ms = if sync_active || awaiting_prompt { 45 } else { 3 };
                        if batch_start.elapsed().as_millis() >= max_batch_ms {
                            break;
                        }
                        let wait_ms = if awaiting_prompt {
                            40
                        } else if sync_active {
                            6
                        } else {
                            1
                        };
                        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                        let pr = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
                        if pr <= 0 || (pfd.revents & libc::POLLIN) == 0 {
                            break;
                        }
                        let n2 = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                        if n2 <= 0 {
                            break;
                        }
                        chunk_len = n2 as usize;
                    }
                    notify();
                }
                unsafe { libc::close(fd) };
                on_exit();
            })?;
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: closing our fd hangs up the terminal; SIGHUP ends the shell.
        unsafe {
            libc::close(self.master);
            libc::kill(self.pid, libc::SIGHUP);
            let mut status = 0;
            libc::waitpid(self.pid, &mut status, libc::WNOHANG);
        }
    }
}

fn write_all(fd: RawFd, mut data: &[u8]) {
    while !data.is_empty() {
        // SAFETY: data is a valid readable slice.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        data = &data[n as usize..];
    }
}

#[cfg(target_os = "macos")]
fn pid_cwd(pid: libc::pid_t) -> Option<String> {
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
fn pid_cwd(pid: libc::pid_t) -> Option<String> {
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
    fn current_process_cwd_resolves() {
        let pid = unsafe { libc::getpid() };
        let cwd = pid_cwd(pid).expect("pid_cwd for current process");
        assert!(std::path::Path::new(&cwd).is_dir());
    }
}
