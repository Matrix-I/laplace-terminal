//! Quick Terminal global hotkey outside macOS (macOS uses Carbon, see
//! `macos::set_global_hotkey`).
//!
//! - X11: a passive key grab on the root window, served by a thread that
//!   waits for the key press.
//! - Wayland has no global hotkey API for clients, so a control socket lets a
//!   desktop-environment shortcut run `laplace +toggle-quick-terminal`. The
//!   socket works under X11 too.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use winit::event_loop::EventLoopProxy;

use crate::keymap::{Action, Trigger};
use crate::AppEvent;

/// What `laplace +toggle-quick-terminal` sends.
const TOGGLE: &[u8] = b"toggle-quick-terminal\n";

/// Where hotkey presses go, set by [`listen`].
static PROXY: parking_lot::Mutex<Option<EventLoopProxy<AppEvent>>> = parking_lot::const_mutex(None);

fn toggle() {
    if let Some(proxy) = PROXY.lock().as_ref() {
        let _ = proxy.send_event(AppEvent::Menu(Action::ToggleQuickTerminal));
    }
}

/// The control socket: in `$XDG_RUNTIME_DIR` (private to the user), else `/tmp`.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("laplace.sock"),
        // SAFETY: getuid has no preconditions.
        _ => PathBuf::from(format!("/tmp/laplace-{}.sock", unsafe { libc::getuid() })),
    }
}

/// `laplace +toggle-quick-terminal`: ask the running instance to toggle.
pub fn send_toggle() -> std::io::Result<()> {
    UnixStream::connect(socket_path())?.write_all(TOGGLE)
}

/// Remember the event proxy for hotkey presses and serve the control socket
/// for the rest of the process. Another running instance keeps the socket; a
/// stale file left by a crash is replaced.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn listen(proxy: EventLoopProxy<AppEvent>) {
    *PROXY.lock() = Some(proxy);
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else { return };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let spawned = std::thread::Builder::new().name("laplace-control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut buf = [0u8; 64];
            let mut stream = stream;
            let n = stream.read(&mut buf).unwrap_or(0);
            if &buf[..n] == TOGGLE {
                toggle();
            }
        }
    });
    if spawned.is_err() {
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(target_os = "macos")]
pub fn set_global_hotkey(_trigger: Option<&Trigger>) {}

/// Grab (or, with `None`, release) the Quick Terminal hotkey on X11. Needs
/// [`listen`] first, which provides the event proxy.
#[cfg(not(target_os = "macos"))]
pub fn set_global_hotkey(trigger: Option<&Trigger>) {
    x11::set(trigger);
}

#[cfg(not(target_os = "macos"))]
mod x11 {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    use crate::keymap::Trigger;

    struct Grabber {
        conn: Arc<RustConnection>,
        root: u32,
        /// Key code and modifiers currently grabbed.
        grabbed: Option<(u8, ModMask)>,
    }

    static GRABBER: Mutex<Option<Grabber>> = parking_lot::const_mutex(None);

    pub fn set(trigger: Option<&Trigger>) {
        let mut slot = GRABBER.lock();
        if slot.is_none() {
            if trigger.is_none() {
                return;
            }
            // No X server (pure Wayland): the control socket is the way in.
            let Ok((conn, screen)) = x11rb::connect(None) else {
                eprintln!(
                    "laplace: no X11 display for the Quick Terminal hotkey; bind `laplace +toggle-quick-terminal` to a desktop shortcut instead"
                );
                return;
            };
            let root = conn.setup().roots[screen].root;
            let conn = Arc::new(conn);
            let events = conn.clone();
            let spawned = std::thread::Builder::new().name("laplace-hotkey".into()).spawn(move || {
                while let Ok(event) = events.wait_for_event() {
                    if let Event::KeyPress(_) = event {
                        super::toggle();
                    }
                }
            });
            if spawned.is_err() {
                return;
            }
            *slot = Some(Grabber { conn, root, grabbed: None });
        }
        let Some(g) = slot.as_mut() else { return };
        if let Some((code, mods)) = g.grabbed.take() {
            for extra in lock_variants() {
                let _ = g.conn.ungrab_key(code, g.root, mods | extra);
            }
        }
        if let Some(t) = trigger {
            match grab(g, t) {
                Ok(grabbed) => g.grabbed = Some(grabbed),
                Err(e) => eprintln!("laplace: Quick Terminal hotkey: {e}"),
            }
        }
        let _ = g.conn.flush();
    }

    /// Caps Lock and Num Lock must not stop the hotkey: grab every combination.
    fn lock_variants() -> [ModMask; 4] {
        [ModMask::default(), ModMask::LOCK, ModMask::M2, ModMask::LOCK | ModMask::M2]
    }

    fn grab(g: &Grabber, t: &Trigger) -> Result<(u8, ModMask), String> {
        let sym = crate::keymap::x11_keysym(&t.key).ok_or("key not supported for a global hotkey")?;
        let setup = g.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = g
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let per = map.keysyms_per_keycode.max(1) as usize;
        let index = map.keysyms.chunks(per).position(|syms| syms.contains(&sym)).ok_or("key not on this keyboard")?;
        let code = min + index as u8;
        let m = t.mods;
        let mut mods = ModMask::default();
        for (on, mask) in [(m.shift, ModMask::SHIFT), (m.ctrl, ModMask::CONTROL), (m.alt, ModMask::M1), (m.sup, ModMask::M4)] {
            if on {
                mods |= mask;
            }
        }
        for extra in lock_variants() {
            g.conn
                .grab_key(true, g.root, mods | extra, code, GrabMode::ASYNC, GrabMode::ASYNC)
                .map_err(|e| e.to_string())?
                .check()
                .map_err(|_| "the key is taken by another application".to_string())?;
        }
        Ok((code, mods))
    }
}
