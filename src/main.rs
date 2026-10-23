//! Entry point: `winit` event loop in `ControlFlow::Wait` mode (0% idle CPU),
//! coalesced PTY redraw wakeups, and keyboard-to-ANSI translation.

mod atlas;
mod grid;
mod pty;
mod renderer;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::grid::Grid;
use crate::pty::Pty;
use crate::renderer::Renderer;

const DEFAULT_PT: f32 = 14.0;

#[derive(Debug, Clone, Copy)]
enum AppEvent {
    WakeUp,
    ChildExit,
}

struct State {
    window: Arc<Window>,
    renderer: Renderer,
    grid: Arc<Mutex<Grid>>,
    pty: Pty,
    mods: ModifiersState,
    font_pt: f32,
}

struct App {
    proxy: EventLoopProxy<AppEvent>,
    wakeup_queued: Arc<AtomicBool>,
    state: Option<State>,
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Laplace")
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let font = atlas::load_font();
        let font_pt = std::env::var("LAPLACE_FONT_SIZE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_PT)
            .clamp(6.0, 72.0);
        let font_px = (font_pt * window.scale_factor() as f32).round();

        let renderer = Renderer::new(window.clone(), font, font_px);
        let (cols, rows) = renderer.grid_size();
        let (cw, ch) = renderer.cell_size();

        let grid = Arc::new(Mutex::new(Grid::new(cols, rows)));
        let pty = Pty::spawn(cols as u16, rows as u16, cw, ch).expect("spawn PTY");

        // Coalesce wakeups: if a wakeup is already in flight, the PTY reader skips
        // sending another event and keeps parsing at full memory speed.
        let p1 = self.proxy.clone();
        let p2 = self.proxy.clone();
        let queued = self.wakeup_queued.clone();
        pty.spawn_reader(
            grid.clone(),
            move || {
                if !queued.swap(true, Ordering::AcqRel) {
                    let _ = p1.send_event(AppEvent::WakeUp);
                }
            },
            move || {
                let _ = p2.send_event(AppEvent::ChildExit);
            },
        )
        .expect("spawn PTY reader");

        self.state = Some(State {
            window,
            renderer,
            grid,
            pty,
            mods: ModifiersState::default(),
            font_pt,
        });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::WakeUp => {
                self.wakeup_queued.store(false, Ordering::Release);
                if let Some(s) = &mut self.state {
                    s.renderer.render(&s.grid);
                }
            }
            AppEvent::ChildExit => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(s) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::ModifiersChanged(m) => s.mods = m.state(),

            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    s.renderer.resize(size.width, size.height);
                    sync_size(s);
                    s.window.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let px = (s.font_pt * scale_factor as f32).round();
                s.renderer.set_font_px(px, scale_factor);
                let sz = s.window.inner_size();
                s.renderer.resize(sz.width, sz.height);
                sync_size(s);
                s.window.request_redraw();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as isize,
                    MouseScrollDelta::PixelDelta(p) => {
                        (p.y as f32 / s.renderer.cell_size().1 as f32).round() as isize
                    }
                };
                if lines != 0 {
                    s.grid.lock().scroll_display(lines);
                    s.window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => handle_key(s, &event),

            WindowEvent::RedrawRequested => {
                // Re-arm before rendering so any PTY write during the draw schedules the next frame.
                self.wakeup_queued.store(false, Ordering::Release);
                s.renderer.render(&s.grid);
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // On macOS, `winit` runs `UserEvent` and `RedrawRequested` inside the
        // `kCFRunLoopBeforeWaiting` observer. If the PTY reader thread sends a
        // `WakeUp` while `s.renderer.render()` is executing, `CFRunLoopWakeUp`
        // fires before the run loop actually enters `mach_msg` sleep and gets lost.
        // Polling for one pass whenever `wakeup_queued` is set guarantees the
        // prompt redraws immediately while keeping 0% CPU once idle.
        if self.wakeup_queued.load(Ordering::Acquire) {
            event_loop.set_control_flow(ControlFlow::Poll);
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn sync_size(s: &mut State) {
    let (cols, rows) = s.renderer.grid_size();
    let (cw, ch) = s.renderer.cell_size();
    {
        let mut g = s.grid.lock();
        if (g.cols, g.rows) == (cols, rows) {
            return;
        }
        g.resize(cols, rows);
    }
    s.pty.resize(cols as u16, rows as u16, cw, ch);
}

fn zoom(s: &mut State, delta_pt: f32) {
    s.font_pt = if delta_pt == 0.0 { DEFAULT_PT } else { (s.font_pt + delta_pt).clamp(6.0, 72.0) };
    let sf = s.window.scale_factor();
    s.renderer.set_font_px((s.font_pt * sf as f32).round(), sf);
    sync_size(s);
    s.window.request_redraw();
}

fn handle_key(s: &mut State, ev: &KeyEvent) {
    if ev.state != ElementState::Pressed {
        return;
    }
    let ctrl = s.mods.control_key();
    let alt = s.mods.alt_key();
    let shift = s.mods.shift_key();
    let super_key = s.mods.super_key();

    // Font size shortcuts (Cmd on macOS, Ctrl+Shift on Linux).
    #[cfg(target_os = "macos")]
    let zoom_mod = super_key;
    #[cfg(not(target_os = "macos"))]
    let zoom_mod = ctrl && shift;
    if zoom_mod {
        if let Key::Character(ch) = &ev.logical_key {
            match ch.as_str() {
                "=" | "+" => return zoom(s, 1.0),
                "-" | "_" => return zoom(s, -1.0),
                "0" => return zoom(s, 0.0),
                _ => {}
            }
        }
    }
    if super_key {
        return;
    }

    // Shift+PageUp / PageDown scrolls the scrollback buffer locally.
    if shift {
        let rows = s.grid.lock().rows.saturating_sub(1).max(1) as isize;
        match ev.logical_key {
            Key::Named(NamedKey::PageUp) => {
                s.grid.lock().scroll_display(rows);
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::PageDown) => {
                s.grid.lock().scroll_display(-rows);
                s.window.request_redraw();
                return;
            }
            _ => {}
        }
    }

    let app_cursor = {
        let mut g = s.grid.lock();
        g.reset_display();
        g.app_cursor
    };

    let m = 1 + (shift as u8) + if alt { 2 } else { 0 } + if ctrl { 4 } else { 0 };
    let mut scratch = [0u8; 32];
    let bytes: &[u8] = match &ev.logical_key {
        Key::Named(NamedKey::Enter) => {
            if alt { b"\x1b\r" } else { b"\r" }
        }
        Key::Named(NamedKey::Backspace) => {
            if alt { b"\x1b\x7f" } else if ctrl { b"\x08" } else { b"\x7f" }
        }
        Key::Named(NamedKey::Tab) => {
            if shift { b"\x1b[Z" } else { b"\t" }
        }
        Key::Named(NamedKey::Escape) => b"\x1b",
        Key::Named(NamedKey::Space) => {
            if ctrl { b"\x00" } else if alt { b"\x1b " } else { b" " }
        }
        Key::Named(k @ (NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::ArrowRight | NamedKey::ArrowLeft | NamedKey::Home | NamedKey::End)) => {
            let code = match k {
                NamedKey::ArrowUp => b'A',
                NamedKey::ArrowDown => b'B',
                NamedKey::ArrowRight => b'C',
                NamedKey::ArrowLeft => b'D',
                NamedKey::Home => b'H',
                _ => b'F',
            };
            if m > 1 {
                let n = push_csi_mod(&mut scratch, 1, m, code);
                &scratch[..n]
            } else {
                scratch[0] = 0x1b;
                scratch[1] = if app_cursor { b'O' } else { b'[' };
                scratch[2] = code;
                &scratch[..3]
            }
        }
        Key::Named(k @ (NamedKey::Insert | NamedKey::Delete | NamedKey::PageUp | NamedKey::PageDown)) => {
            let num = match k {
                NamedKey::Insert => 2,
                NamedKey::Delete => 3,
                NamedKey::PageUp => 5,
                _ => 6,
            };
            let n = push_tilde(&mut scratch, num, m);
            &scratch[..n]
        }
        Key::Named(k @ (NamedKey::F1 | NamedKey::F2 | NamedKey::F3 | NamedKey::F4)) if m == 1 => {
            scratch[0] = 0x1b;
            scratch[1] = b'O';
            scratch[2] = match k {
                NamedKey::F1 => b'P',
                NamedKey::F2 => b'Q',
                NamedKey::F3 => b'R',
                _ => b'S',
            };
            &scratch[..3]
        }
        Key::Named(k) => {
            let num: Option<u8> = match k {
                NamedKey::F1 => Some(11),
                NamedKey::F2 => Some(12),
                NamedKey::F3 => Some(13),
                NamedKey::F4 => Some(14),
                NamedKey::F5 => Some(15),
                NamedKey::F6 => Some(17),
                NamedKey::F7 => Some(18),
                NamedKey::F8 => Some(19),
                NamedKey::F9 => Some(20),
                NamedKey::F10 => Some(21),
                NamedKey::F11 => Some(23),
                NamedKey::F12 => Some(24),
                _ => None,
            };
            let Some(num) = num else { return };
            let n = push_tilde(&mut scratch, num, m);
            &scratch[..n]
        }
        Key::Character(ch) if ctrl => {
            let b = ch.as_bytes()[0];
            let c = match b {
                b'a'..=b'z' => b - b'a' + 1,
                b'A'..=b'Z' => b - b'A' + 1,
                b'@' | b'2' | b' ' => 0,
                b'[' | b'3' => 27,
                b'\\' | b'4' => 28,
                b']' | b'5' => 29,
                b'^' | b'6' => 30,
                b'_' | b'7' | b'/' => 31,
                b'?' | b'8' => 127,
                _ => return,
            };
            if alt {
                scratch[0] = 0x1b;
                scratch[1] = c;
                &scratch[..2]
            } else {
                scratch[0] = c;
                &scratch[..1]
            }
        }
        _ => {
            let Some(txt) = &ev.text else { return };
            if txt.is_empty() {
                return;
            }
            if alt && txt.len() + 1 <= scratch.len() {
                scratch[0] = 0x1b;
                scratch[1..1 + txt.len()].copy_from_slice(txt.as_bytes());
                &scratch[..1 + txt.len()]
            } else {
                txt.as_bytes()
            }
        }
    };
    s.pty.write(bytes);
}

fn push_csi_mod(buf: &mut [u8; 32], p1: u8, m: u8, suffix: u8) -> usize {
    use std::io::Write;
    let mut cur = std::io::Cursor::new(&mut buf[..]);
    let _ = write!(cur, "\x1b[{p1};{m}{}", suffix as char);
    cur.position() as usize
}

fn push_tilde(buf: &mut [u8; 32], num: u8, m: u8) -> usize {
    use std::io::Write;
    let mut cur = std::io::Cursor::new(&mut buf[..]);
    if m > 1 {
        let _ = write!(cur, "\x1b[{num};{m}~");
    } else {
        let _ = write!(cur, "\x1b[{num}~");
    }
    cur.position() as usize
}

fn main() {
    let event_loop = EventLoop::<AppEvent>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App {
        proxy,
        wakeup_queued: Arc::new(AtomicBool::new(false)),
        state: None,
    };
    event_loop.run_app(&mut app).expect("run app");
}
