//! Entry point: `winit` event loop in `ControlFlow::Wait` mode (0% idle CPU),
//! coalesced PTY redraw wakeups, and keyboard-to-ANSI translation.

mod atlas;
mod config;
mod fontdb;
mod grid;
mod macos;
mod pty;
mod renderer;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::grid::{Grid, Hyperlink, Selection};
use crate::pty::Pty;
use crate::renderer::{Renderer, TabBarAction};

#[derive(Debug, Clone, Copy)]
enum AppEvent {
    WakeUp,
    ChildExit(u64),
}

struct Tab {
    id: u64,
    grid: Arc<Mutex<Grid>>,
    pty: Pty,
}

struct State {
    window: Arc<Window>,
    renderer: Renderer,
    tabs: Vec<Tab>,
    tab_titles: Vec<String>,
    active: usize,
    next_tab_id: u64,
    mods: ModifiersState,
    font_pt: f32,
    mouse_pos: (f64, f64),
    selecting: bool,
    drag_moved: bool,
    last_click_time: Option<std::time::Instant>,
    last_click_cell: Option<(usize, usize)>,
    click_count: u8,
    hovered_link: Option<Hyperlink>,
    config: config::Config,
    config_mtime: Option<std::time::SystemTime>,
    mouse_hidden: bool,
    cursor_blink_epoch: std::time::Instant,
    focused: bool,
}

struct App {
    proxy: EventLoopProxy<AppEvent>,
    wakeup_queued: Arc<AtomicBool>,
    state: Option<State>,
}

impl ApplicationHandler<AppEvent> for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            if let Some(s) = &self.state {
                s.window.request_redraw();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let cfg = config::Config::load();
        grid::set_default_colors(cfg.foreground, cfg.background);
        grid::set_palette(&cfg.palette);

        let attrs = Window::default_attributes()
            .with_title("Laplace")
            .with_transparent(cfg.background_opacity < 1.0)
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        #[cfg(target_os = "macos")]
        macos::apply_macos_window_config(&window, &cfg);

        let font = atlas::load_font(&cfg.font_family);
        let font_pt = cfg.font_size.clamp(6.0, 72.0);
        let font_px = (font_pt * window.scale_factor() as f32).round();

        let mut renderer = Renderer::new(
            window.clone(),
            font,
            font_px,
            cfg.background_opacity,
            cfg.cursor_style.shader_id(),
        );

        #[cfg(target_os = "macos")]
        macos::apply_macos_window_config(&window, &cfg);

        if let (Some(w_cols), Some(w_rows)) = (cfg.window_width, cfg.window_height) {
            let (cw, ch) = renderer.cell_size();
            let pad = renderer.pad();
            let sf = window.scale_factor();
            let inner_w = (w_cols * cw + 2 * pad) as f64 / sf;
            let inner_h = (w_rows * ch + 2 * pad) as f64 / sf;
            let _ = window.request_inner_size(LogicalSize::new(inner_w, inner_h));
            let sz = window.inner_size();
            renderer.resize(sz.width, sz.height);
        }

        let (cols, rows) = renderer.grid_size_for_tabs(false);
        let (cw, ch) = renderer.cell_size();

        let tab_id = 1;
        let tab = spawn_tab(
            tab_id,
            cols,
            rows,
            cw,
            ch,
            None,
            Some(cfg.shell_integration_features),
            &self.proxy,
            &self.wakeup_queued,
        )
        .expect("spawn initial PTY");
        let title = tab_title(&tab);

        let config_mtime = config::config_mtime();
        self.state = Some(State {
            window,
            renderer,
            tabs: vec![tab],
            tab_titles: vec![title],
            active: 0,
            next_tab_id: 2,
            mods: ModifiersState::default(),
            font_pt,
            mouse_pos: (0.0, 0.0),
            selecting: false,
            drag_moved: false,
            last_click_time: None,
            last_click_cell: None,
            click_count: 0,
            hovered_link: None,
            config: cfg,
            config_mtime,
            mouse_hidden: false,
            cursor_blink_epoch: std::time::Instant::now(),
            focused: true,
        });

        #[cfg(target_os = "macos")]
        unsafe {
            extern "C" {
                fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
            }
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::WakeUp => {
                self.wakeup_queued.store(false, Ordering::Release);
                if let Some(s) = &mut self.state {
                    render_active(s);
                    s.window.request_redraw();
                }
            }
            AppEvent::ChildExit(exited_id) => {
                let Some(s) = &mut self.state else {
                    event_loop.exit();
                    return;
                };
                if let Some(pos) = s.tabs.iter().position(|t| t.id == exited_id) {
                    s.tabs.remove(pos);
                    if s.tabs.is_empty() {
                        event_loop.exit();
                        return;
                    }
                    if s.active >= s.tabs.len() {
                        s.active = s.tabs.len() - 1;
                    } else if pos < s.active {
                        s.active -= 1;
                    }
                    sync_size(s);
                    refresh_tab_titles(s);
                    s.renderer.invalidate();
                    s.window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(s) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::ModifiersChanged(m) => {
                s.mods = m.state();
                update_hovered_link(s);
            }

            WindowEvent::CursorMoved { position, .. } => {
                s.mouse_pos = (position.x, position.y);
                if s.mouse_hidden {
                    s.window.set_cursor_visible(true);
                    s.mouse_hidden = false;
                }
                if s.selecting {
                    if let Some(start) = s.last_click_cell {
                        let show_tabs = s.tabs.len() > 1;
                        let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
                        if let Some(end) = s.renderer.pixel_to_cell(position.x, position.y, show_tabs, cols, rows) {
                            if end != start || s.drag_moved {
                                s.drag_moved = true;
                                s.tabs[s.active].grid.lock().set_selection(Some(Selection { start, end }));
                                s.window.request_redraw();
                            }
                        }
                    }
                } else {
                    update_hovered_link(s);
                }
            }

            WindowEvent::MouseInput {
                state: btn_state,
                button: MouseButton::Left,
                ..
            } => match btn_state {
                ElementState::Pressed => {
                    #[cfg(target_os = "macos")]
                    let cmd_pressed = s.mods.super_key();
                    #[cfg(not(target_os = "macos"))]
                    let cmd_pressed = s.mods.control_key() || s.mods.super_key();

                    if cmd_pressed {
                        if let Some(link) = &s.hovered_link {
                            let cwd = s.tabs.get(s.active).and_then(|t| t.pty.cwd());
                            let cwd_path = cwd.as_deref().map(std::path::Path::new);
                            open_target(&link.url, link.is_path, cwd_path);
                            return;
                        }
                    }

                    if let Some(action) = s.renderer.hit_test_tab_bar(s.mouse_pos.0, s.mouse_pos.1, &s.tab_titles) {
                        match action {
                            TabBarAction::Select(idx) => select_tab(s, idx),
                            TabBarAction::NewTab => duplicate_active_tab(s, &self.proxy, &self.wakeup_queued),
                        }
                        return;
                    }
                    let show_tabs = s.tabs.len() > 1;
                    let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
                    if let Some((row, col)) =
                        s.renderer.pixel_to_cell(s.mouse_pos.0, s.mouse_pos.1, show_tabs, cols, rows)
                    {
                        let now = std::time::Instant::now();
                        let multi_click = s
                            .last_click_time
                            .is_some_and(|t| now.duration_since(t).as_millis() <= 450)
                            && s.last_click_cell.is_some_and(|(lr, _)| lr == row);

                        s.click_count = if multi_click {
                            (s.click_count % 3) + 1
                        } else {
                            1
                        };
                        s.last_click_time = Some(now);
                        s.last_click_cell = Some((row, col));

                        match s.click_count {
                            2 => {
                                let line_text =
                                    s.tabs[s.active].grid.lock().select_smart_line(row, col);
                                if !line_text.is_empty() {
                                    copy_to_clipboard(&line_text);
                                }
                                s.selecting = false;
                                s.drag_moved = false;
                                s.window.request_redraw();
                            }
                            3 => {
                                let line_text = s.tabs[s.active].grid.lock().select_line(row);
                                if !line_text.is_empty() {
                                    copy_to_clipboard(&line_text);
                                }
                                s.selecting = false;
                                s.drag_moved = false;
                                s.window.request_redraw();
                            }
                            _ => {
                                s.tabs[s.active].grid.lock().set_selection(None);
                                s.selecting = true;
                                s.drag_moved = false;
                                s.window.request_redraw();
                            }
                        }
                    }
                }
                ElementState::Released => {
                    if s.selecting {
                        s.selecting = false;
                        if s.drag_moved {
                            let text = s.tabs[s.active].grid.lock().selected_text();
                            if let Some(t) = text {
                                copy_to_clipboard(&t);
                            }
                        }
                    }
                }
            },

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
                #[cfg(target_os = "macos")]
                macos::apply_macos_window_config(&s.window, &s.config);
                s.window.request_redraw();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        (y as f64 * s.config.mouse_scroll_discrete).round() as isize
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        let cell_h = s.renderer.cell_size().1 as f64;
                        let px_lines = p.y / cell_h.max(1.0);
                        (px_lines * s.config.mouse_scroll_precision).round() as isize
                    }
                };
                if lines != 0 {
                    s.tabs[s.active].grid.lock().scroll_display(lines);
                    s.window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                handle_key(s, &event, event_loop, &self.proxy, &self.wakeup_queued);
            }

            WindowEvent::Focused(focused) => {
                s.focused = focused;
                if focused {
                    s.cursor_blink_epoch = std::time::Instant::now();
                    let mtime = config::config_mtime();
                    if mtime != s.config_mtime {
                        reload_config(s);
                    }
                }
                s.window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                // Re-arm before rendering so any PTY write during the draw schedules the next frame.
                self.wakeup_queued.store(false, Ordering::Release);
                render_active(s);
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.wakeup_queued.load(Ordering::Acquire) {
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        if let Some(s) = &self.state {
            if s.focused && cursor_blinks(s) {
                let now = std::time::Instant::now();
                let elapsed = s.cursor_blink_epoch.elapsed();
                let elapsed_ms = elapsed.as_millis();
                let next_boundary_ms = ((elapsed_ms / 500) + 1) * 500;
                let dur_to_next = std::time::Duration::from_millis((next_boundary_ms - elapsed_ms) as u64);
                let next_instant = now + dur_to_next;
                event_loop.set_control_flow(ControlFlow::WaitUntil(next_instant));
                return;
            }
        }
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

fn spawn_tab(
    tab_id: u64,
    cols: usize,
    rows: usize,
    cw: u32,
    ch: u32,
    cwd: Option<&str>,
    shell_features: Option<crate::config::ShellFeatures>,
    proxy: &EventLoopProxy<AppEvent>,
    wakeup_queued: &Arc<AtomicBool>,
) -> std::io::Result<Tab> {
    let grid = Arc::new(Mutex::new(Grid::new(cols, rows)));
    let pty = Pty::spawn_in(cols as u16, rows as u16, cw, ch, cwd, shell_features)?;
    let p1 = proxy.clone();
    let p2 = proxy.clone();
    let queued = wakeup_queued.clone();
    pty.spawn_reader(
        grid.clone(),
        move || {
            if !queued.swap(true, Ordering::AcqRel) {
                let _ = p1.send_event(AppEvent::WakeUp);
            }
        },
        move || {
            let _ = p2.send_event(AppEvent::ChildExit(tab_id));
        },
    )?;
    Ok(Tab { id: tab_id, grid, pty })
}

fn tab_title(tab: &Tab) -> String {
    {
        let g = tab.grid.lock();
        if !g.title.is_empty() {
            return g.title.clone();
        }
    }
    let Some(cwd) = tab.pty.cwd() else {
        return "shell".into();
    };
    let short_dir = if let Ok(home) = std::env::var("HOME") {
        if cwd == home {
            "~".to_string()
        } else if let Some(rest) = cwd.strip_prefix(&home) {
            format!("~{rest}")
        } else {
            cwd
        }
    } else {
        cwd
    };
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty() {
        short_dir
    } else {
        format!("{user}:{short_dir}")
    }
}

fn refresh_tab_titles(s: &mut State) {
    let mut changed = s.tab_titles.len() != s.tabs.len();
    s.tab_titles.resize_with(s.tabs.len(), String::new);
    for (dst, tab) in s.tab_titles.iter_mut().zip(&s.tabs) {
        let new_title = tab_title(tab);
        if *dst != new_title {
            *dst = new_title;
            changed = true;
        }
    }
    if changed {
        if s.tabs.len() > 1 {
            s.renderer.invalidate();
        }
        if let Some(active_title) = s.tab_titles.get(s.active) {
            s.window.set_title(active_title);
        }
    }
}

fn cursor_blinks(s: &State) -> bool {
    match s.config.cursor_style_blink {
        Some(b) => b,
        None => {
            if let Some(tab) = s.tabs.get(s.active) {
                let g = tab.grid.lock();
                match g.cursor_shape {
                    0 | 1 | 3 | 5 => true,
                    _ => false,
                }
            } else {
                true
            }
        }
    }
}

fn cursor_is_visible_now(s: &State) -> bool {
    if !s.focused {
        return true;
    }
    if !cursor_blinks(s) {
        return true;
    }
    let elapsed = s.cursor_blink_epoch.elapsed().as_millis();
    (elapsed % 1000) < 500
}

fn reload_config(s: &mut State) {
    let old = s.config.clone();
    let new = config::Config::load();
    s.config_mtime = config::config_mtime();
    if new == old {
        return;
    }

    grid::set_default_colors(new.foreground, new.background);
    grid::set_palette(&new.palette);

    #[cfg(target_os = "macos")]
    macos::apply_macos_window_config(&s.window, &new);

    s.renderer.set_bg_opacity(new.background_opacity);
    s.renderer.set_cursor_style(new.cursor_style.shader_id());

    let font_changed = new.font_family != old.font_family;
    let size_changed = (new.font_size - old.font_size).abs() > 0.001;
    if font_changed || size_changed {
        s.font_pt = new.font_size;
        let font = atlas::load_font(&new.font_family);
        let sf = s.window.scale_factor();
        let font_px = (s.font_pt * sf as f32).round();
        s.renderer.set_fonts(font, font_px);
        sync_size(s);
    }

    if !new.mouse_hide_while_typing && s.mouse_hidden {
        s.window.set_cursor_visible(true);
        s.mouse_hidden = false;
    }

    s.config = new;
    s.renderer.invalidate();
    s.window.request_redraw();
}

fn render_active(s: &mut State) {
    if s.tabs.is_empty() {
        return;
    }
    refresh_tab_titles(s);
    let tab_bar = (s.tabs.len() > 1).then_some((s.tab_titles.as_slice(), s.active));
    let blink_on = cursor_is_visible_now(s);
    s.renderer
        .render_with_tabs(&s.tabs[s.active].grid, tab_bar, s.hovered_link.as_ref(), blink_on);
}

fn open_target(target: &str, is_path: bool, cwd: Option<&std::path::Path>) {
    if is_path {
        let path = if target.starts_with("~/") {
            if let Ok(home) = std::env::var("HOME") {
                std::path::PathBuf::from(home).join(&target[2..])
            } else {
                std::path::PathBuf::from(target)
            }
        } else if target == "~" {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("~"))
        } else {
            let p = std::path::Path::new(target);
            if p.is_absolute() {
                p.to_path_buf()
            } else if let Some(base) = cwd {
                base.join(p)
            } else {
                p.to_path_buf()
            }
        };

        #[cfg(target_os = "macos")]
        let res = std::process::Command::new("open").arg(&path).spawn();
        #[cfg(target_os = "linux")]
        let res = std::process::Command::new("xdg-open").arg(&path).spawn();
        #[cfg(windows)]
        let res = std::process::Command::new("cmd")
            .args(["/c", "start", "", path.to_str().unwrap_or(target)])
            .spawn();

        if let Err(e) = res {
            eprintln!("laplace: failed to open path {path:?}: {e}");
        }
    } else {
        #[cfg(target_os = "macos")]
        let res = std::process::Command::new("open").arg(target).spawn();
        #[cfg(target_os = "linux")]
        let res = std::process::Command::new("xdg-open").arg(target).spawn();
        #[cfg(windows)]
        let res = std::process::Command::new("cmd")
            .args(["/c", "start", "", target])
            .spawn();

        if let Err(e) = res {
            eprintln!("laplace: failed to open URL {target}: {e}");
        }
    }
}

fn update_hovered_link(s: &mut State) {
    #[cfg(target_os = "macos")]
    let cmd_pressed = s.mods.super_key();
    #[cfg(not(target_os = "macos"))]
    let cmd_pressed = s.mods.control_key() || s.mods.super_key();

    let cwd = s.tabs.get(s.active).and_then(|t| t.pty.cwd());
    let cwd_path = cwd.as_deref().map(std::path::Path::new);

    let new_link = if cmd_pressed && !s.selecting {
        let show_tabs = s.tabs.len() > 1;
        let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
        if let Some((row, col)) = s.renderer.pixel_to_cell(s.mouse_pos.0, s.mouse_pos.1, show_tabs, cols, rows) {
            let g = s.tabs[s.active].grid.lock();
            g.find_hyperlink_at(row, col, cwd_path)
        } else {
            None
        }
    } else {
        None
    };

    if s.hovered_link != new_link {
        s.hovered_link = new_link;
        if s.hovered_link.is_some() {
            s.window.set_cursor(CursorIcon::Pointer);
        } else {
            s.window.set_cursor(CursorIcon::Default);
        }
        s.renderer.invalidate();
        s.window.request_redraw();
    }
}

fn duplicate_active_tab(
    s: &mut State,
    proxy: &EventLoopProxy<AppEvent>,
    wakeup_queued: &Arc<AtomicBool>,
) {
    let cwd = s.tabs.get(s.active).and_then(|t| t.pty.cwd());
    let (cols, rows) = s.renderer.grid_size_for_tabs(true);
    let (cw, ch) = s.renderer.cell_size();
    let id = s.next_tab_id;
    s.next_tab_id += 1;
    match spawn_tab(
        id,
        cols,
        rows,
        cw,
        ch,
        cwd.as_deref(),
        Some(s.config.shell_integration_features),
        proxy,
        wakeup_queued,
    ) {
        Ok(tab) => {
            let insert_at = (s.active + 1).min(s.tabs.len());
            s.tabs.insert(insert_at, tab);
            s.active = insert_at;
            sync_size(s);
            refresh_tab_titles(s);
            s.renderer.invalidate();
            s.window.request_redraw();
        }
        Err(e) => eprintln!("laplace: failed to spawn duplicate tab: {e}"),
    }
}

fn select_tab(s: &mut State, idx: usize) {
    if idx < s.tabs.len() && idx != s.active {
        s.active = idx;
        refresh_tab_titles(s);
        s.renderer.invalidate();
        s.window.request_redraw();
    }
}

fn close_active_tab(s: &mut State, event_loop: &ActiveEventLoop) {
    if s.tabs.is_empty() {
        event_loop.exit();
        return;
    }
    s.tabs.remove(s.active);
    if s.tabs.is_empty() {
        event_loop.exit();
        return;
    }
    if s.active >= s.tabs.len() {
        s.active = s.tabs.len() - 1;
    }
    sync_size(s);
    refresh_tab_titles(s);
    s.renderer.invalidate();
    s.window.request_redraw();
}

fn sync_size(s: &mut State) {
    let show_tabs = s.tabs.len() > 1;
    let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
    let (cw, ch) = s.renderer.cell_size();
    for tab in &s.tabs {
        let changed = {
            let mut g = tab.grid.lock();
            if (g.cols, g.rows) == (cols, rows) {
                false
            } else {
                g.resize(cols, rows);
                true
            }
        };
        if changed {
            tab.pty.resize(cols as u16, rows as u16, cw, ch);
        }
    }
}

fn zoom(s: &mut State, delta_pt: f32) {
    s.font_pt = if delta_pt == 0.0 {
        s.config.font_size
    } else {
        (s.font_pt + delta_pt).clamp(6.0, 72.0)
    };
    let sf = s.window.scale_factor();
    s.renderer.set_font_px((s.font_pt * sf as f32).round(), sf);
    sync_size(s);
    s.window.request_redraw();
}

fn handle_key(
    s: &mut State,
    ev: &KeyEvent,
    event_loop: &ActiveEventLoop,
    proxy: &EventLoopProxy<AppEvent>,
    wakeup_queued: &Arc<AtomicBool>,
) {
    if ev.state != ElementState::Pressed {
        return;
    }
    let ctrl = s.mods.control_key();
    let alt = s.mods.alt_key();
    let shift = s.mods.shift_key();
    let super_key = s.mods.super_key();

    if !matches!(
        &ev.logical_key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
        )
    ) {
        if s.config.mouse_hide_while_typing && !s.mouse_hidden {
            s.window.set_cursor_visible(false);
            s.mouse_hidden = true;
        }
    }
    s.cursor_blink_epoch = std::time::Instant::now();

    // Duplicate tab in same working directory:
    // - macOS: Cmd+T
    // - Linux: Ctrl+T (or Ctrl+Shift+T)
    if let Key::Character(ch) = &ev.logical_key {
        #[cfg(target_os = "macos")]
        let is_config = super_key && !ctrl && !alt && (ch == "," || (shift && ch == "<"));
        #[cfg(not(target_os = "macos"))]
        let is_config = ctrl && !alt && !super_key && (ch == "," || (shift && ch == "<"));
        if is_config {
            if shift {
                reload_config(s);
            } else {
                config::open_config_in_editor();
            }
            return;
        }

        #[cfg(target_os = "macos")]
        let is_new_tab = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("t");
        #[cfg(not(target_os = "macos"))]
        let is_new_tab = ctrl && !alt && !super_key && ch.eq_ignore_ascii_case("t");
        if is_new_tab {
            duplicate_active_tab(s, proxy, wakeup_queued);
            return;
        }

        // Copy selection: Cmd+C on macOS, Ctrl+Shift+C on Linux.
        #[cfg(target_os = "macos")]
        let is_copy = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("c");
        #[cfg(not(target_os = "macos"))]
        let is_copy = ctrl && shift && !alt && ch.eq_ignore_ascii_case("c");
        if is_copy {
            if let Some(text) = s.tabs[s.active].grid.lock().selected_text() {
                copy_to_clipboard(&text);
            }
            return;
        }

        // Paste clipboard: Cmd+V on macOS, Ctrl+Shift+V on Linux.
        #[cfg(target_os = "macos")]
        let is_paste = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("v");
        #[cfg(not(target_os = "macos"))]
        let is_paste = ctrl && shift && !alt && ch.eq_ignore_ascii_case("v");
        if is_paste {
            if let Some(text) = paste_from_clipboard() {
                if !text.is_empty() {
                    let bracketed = {
                        let mut g = s.tabs[s.active].grid.lock();
                        g.reset_display();
                        g.bracketed_paste
                    };
                    if bracketed {
                        let mut payload = Vec::with_capacity(text.len() + 12);
                        payload.extend_from_slice(b"\x1b[200~");
                        payload.extend_from_slice(text.as_bytes());
                        payload.extend_from_slice(b"\x1b[201~");
                        s.tabs[s.active].pty.write(&payload);
                    } else {
                        s.tabs[s.active].pty.write(text.as_bytes());
                    }
                    s.window.request_redraw();
                }
            }
            return;
        }

        // Close active tab: Cmd+W on macOS, Ctrl+Shift+W on Linux.
        #[cfg(target_os = "macos")]
        let is_close_tab = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("w");
        #[cfg(not(target_os = "macos"))]
        let is_close_tab = ctrl && shift && !alt && ch.eq_ignore_ascii_case("w");
        if is_close_tab {
            close_active_tab(s, event_loop);
            return;
        }

        // Switch tabs on macOS via Cmd+Shift+[ / ] (or Cmd+{ / }).
        #[cfg(target_os = "macos")]
        if super_key && !ctrl && !alt && s.tabs.len() > 1 {
            match ch.as_str() {
                "{" | "[" if shift => {
                    let prev = (s.active + s.tabs.len() - 1) % s.tabs.len();
                    select_tab(s, prev);
                    return;
                }
                "}" | "]" if shift => {
                    let next = (s.active + 1) % s.tabs.len();
                    select_tab(s, next);
                    return;
                }
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" if !shift => {
                    let n = (ch.as_bytes()[0] - b'1') as usize;
                    let target = if n == 8 && s.tabs.len() > 1 {
                        s.tabs.len() - 1
                    } else {
                        n
                    };
                    select_tab(s, target);
                    return;
                }
                _ => {}
            }
        }

        // Switch tabs on Linux via Alt+1..9.
        #[cfg(not(target_os = "macos"))]
        if alt && !ctrl && !shift && !super_key && s.tabs.len() > 1 {
            if let b'1'..=b'9' = ch.as_bytes()[0] {
                let n = (ch.as_bytes()[0] - b'1') as usize;
                let target = if n == 8 && s.tabs.len() > 1 {
                    s.tabs.len() - 1
                } else {
                    n
                };
                select_tab(s, target);
                return;
            }
        }
    }

    // Tab navigation with Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PageUp / Ctrl+PageDown,
    // and Cmd+Option+Left / Right on macOS.
    if s.tabs.len() > 1 {
        match &ev.logical_key {
            Key::Named(NamedKey::Tab) if ctrl && !alt && !super_key => {
                let next = if shift {
                    (s.active + s.tabs.len() - 1) % s.tabs.len()
                } else {
                    (s.active + 1) % s.tabs.len()
                };
                select_tab(s, next);
                return;
            }
            Key::Named(NamedKey::PageUp) if ctrl && !shift && !alt => {
                let prev = (s.active + s.tabs.len() - 1) % s.tabs.len();
                select_tab(s, prev);
                return;
            }
            Key::Named(NamedKey::PageDown) if ctrl && !shift && !alt => {
                let next = (s.active + 1) % s.tabs.len();
                select_tab(s, next);
                return;
            }
            #[cfg(target_os = "macos")]
            Key::Named(NamedKey::ArrowLeft) if super_key && alt && !ctrl && !shift => {
                let prev = (s.active + s.tabs.len() - 1) % s.tabs.len();
                select_tab(s, prev);
                return;
            }
            #[cfg(target_os = "macos")]
            Key::Named(NamedKey::ArrowRight) if super_key && alt && !ctrl && !shift => {
                let next = (s.active + 1) % s.tabs.len();
                select_tab(s, next);
                return;
            }
            _ => {}
        }
    }

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
        let rows = s.tabs[s.active].grid.lock().rows.saturating_sub(1).max(1) as isize;
        match ev.logical_key {
            Key::Named(NamedKey::PageUp) => {
                s.tabs[s.active].grid.lock().scroll_display(rows);
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::PageDown) => {
                s.tabs[s.active].grid.lock().scroll_display(-rows);
                s.window.request_redraw();
                return;
            }
            _ => {}
        }
    }

    let app_cursor = {
        let mut g = s.tabs[s.active].grid.lock();
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
    s.tabs[s.active].pty.write(bytes);
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

fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let cmds: &[(&str, &[&str])] = &[("/usr/bin/pbcopy", &[])];
    #[cfg(not(target_os = "macos"))]
    let cmds: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["-ib"]),
    ];

    for (bin, args) in cmds {
        if let Ok(mut child) = Command::new(bin)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }
}

fn paste_from_clipboard() -> Option<String> {
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let cmds: &[(&str, &[&str])] = &[("/usr/bin/pbpaste", &[])];
    #[cfg(not(target_os = "macos"))]
    let cmds: &[(&str, &[&str])] = &[
        ("wl-paste", &["-n"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["-ob"]),
    ];

    for (bin, args) in cmds {
        if let Ok(out) = Command::new(bin)
            .args(*args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    return Some(s);
                }
            }
        }
    }
    None
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
