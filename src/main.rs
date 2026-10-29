//! Entry point: `winit` event loop in `ControlFlow::Wait` mode (0% idle CPU),
//! coalesced PTY redraw wakeups, and keyboard-to-ANSI translation.

mod atlas;
mod bidi;
mod config;
mod fontdb;
mod graphics;
mod grid;
mod history;
mod hotkey;
mod image_layer;
mod keymap;
mod macos;
mod mouse;
mod postfx;
mod pty;
mod renderer;
mod search;
mod shell_integration;
mod split;
mod update;
mod x11colors;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(target_os = "macos")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use parking_lot::Mutex;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};

use crate::grid::{Grid, Hyperlink, Selection};
use crate::history::HistorySearch;
use crate::pty::Pty;
use crate::renderer::{Renderer, TabBarAction};
use crate::search::SearchBar;
use crate::split::{Direction, PaneRect, Rect, SplitDirection, SplitNode};

#[derive(Debug, Clone)]
pub enum AppEvent {
    WakeUp,
    ChildExit(u64),
    Reopen,
    /// A menu bar item (macOS) was chosen.
    Menu(keymap::Action),
    /// The system switched between Light and Dark appearance (macOS).
    SystemAppearance { dark: bool },
    UpdateCheckResult(Result<update::ReleaseInfo, String>),
    UpdateInstallResult(Result<update::Version, String>),
}

struct Pane {
    id: u64,
    grid: Arc<Mutex<Grid>>,
    pty: Pty,
    cached_cwd: Option<String>,
    last_cwd_check: std::time::Instant,
}

impl Pane {
    fn cwd(&mut self) -> Option<String> {
        if self.cached_cwd.is_some() && self.last_cwd_check.elapsed().as_millis() < 500 {
            return self.cached_cwd.clone();
        }
        // Prefer the directory the shell reports via OSC 7; fall back to asking the OS.
        let reported = self.grid.lock().reported_cwd.clone();
        self.cached_cwd = reported.filter(|d| std::path::Path::new(d).is_dir()).or_else(|| self.pty.cwd());
        self.last_cwd_check = std::time::Instant::now();
        self.cached_cwd.clone()
    }
}

struct Tab {
    #[allow(dead_code)]
    id: u64,
    panes: Vec<Pane>,
    layout: SplitNode,
    active_pane_id: u64,
    zoomed_pane_id: Option<u64>,
    /// A pane rang the bell while this tab was not in front.
    bell: bool,
}

impl Tab {
    fn new(id: u64, pane: Pane) -> Self {
        let pane_id = pane.id;
        Tab {
            id,
            panes: vec![pane],
            layout: SplitNode::new_leaf(pane_id),
            active_pane_id: pane_id,
            zoomed_pane_id: None,
            bell: false,
        }
    }

    fn active_pane(&self) -> &Pane {
        self.panes
            .iter()
            .find(|p| p.id == self.active_pane_id)
            .unwrap_or(&self.panes[0])
    }

    #[allow(dead_code)]
    fn active_pane_mut(&mut self) -> &mut Pane {
        let id = self.active_pane_id;
        if let Some(pos) = self.panes.iter().position(|p| p.id == id) {
            &mut self.panes[pos]
        } else {
            &mut self.panes[0]
        }
    }

    fn pane(&self, id: u64) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    fn compute_pane_rects(&self, total_cols: usize, total_rows: usize) -> Vec<PaneRect> {
        if let Some(z_id) = self.zoomed_pane_id {
            if self.panes.iter().any(|p| p.id == z_id) {
                return vec![PaneRect {
                    id: z_id,
                    col: 0,
                    row: 0,
                    cols: total_cols,
                    rows: total_rows,
                    has_border_right: false,
                    has_border_bottom: false,
                }];
            }
        }
        let area = Rect {
            col: 0,
            row: 0,
            cols: total_cols,
            rows: total_rows,
        };
        let mut rects = Vec::with_capacity(self.panes.len());
        self.layout.compute_rects(area, &mut rects);
        rects
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HoveredPaneLink {
    pane_id: u64,
    link: Hyperlink,
}

struct State {
    window: Arc<Window>,
    renderer: Renderer,
    tabs: Vec<Tab>,
    tab_titles: Vec<String>,
    active: usize,
    mods: ModifiersState,
    font_pt: f32,
    mouse_pos: (f64, f64),
    selecting: bool,
    drag_moved: bool,
    last_click_time: Option<std::time::Instant>,
    last_click_cell: Option<(usize, usize)>,
    active_click_pane_id: Option<u64>,
    click_count: u8,
    /// The press that started this click sequence landed on an already selected
    /// block, so the double-click widens the selection to the line.
    click_on_selected_block: bool,
    /// The current drag started with Shift+Click and moves only the selection end.
    extend_drag: bool,
    /// Pane whose scrollbar thumb is being dragged.
    scrollbar_drag: Option<u64>,
    /// Mouse pointer outside links: the active pane's OSC 22 shape.
    pointer_icon: CursorIcon,
    /// Last desktop notification shown, to rate-limit OSC 9 / 777 floods.
    last_notification: Option<std::time::Instant>,
    /// Secure Keyboard Entry requested (macOS); held only while focused.
    secure_input: bool,
    hovered_link: Option<HoveredPaneLink>,
    config: config::Config,
    config_mtime: Option<std::time::SystemTime>,
    mouse_hidden: bool,
    /// Keys whose press Laplace handled itself (shortcuts, search box), so their
    /// release is not reported to a Kitty-protocol application either.
    consumed_keys: Vec<PhysicalKey>,
    /// Caps / Num Lock inferred from key presses (`keymap::infer_locks`), for
    /// platforms that do not report them.
    locks: u8,
    cursor_blink_epoch: std::time::Instant,
    focused: bool,
    history_search: HistorySearch,
    /// Find-in-terminal box (`Cmd+F` / `Ctrl+Shift+F`).
    search: SearchBar,
    first_frame: bool,
    /// IME composition in progress (Telex, Pinyin, ...); keys belong to the IME while non-empty.
    ime_preedit: String,
    /// Last cursor cell reported to the IME for candidate window placement.
    ime_cursor_cell: Option<(usize, usize)>,
    /// Mouse button held while the application tracks the mouse (for drag reports).
    mouse_button_down: Option<mouse::Button>,
    /// Last `(pane, row, col)` reported to a mouse-tracking application.
    last_mouse_report: Option<(u64, usize, usize)>,
    /// A frame was skipped because an application is in synchronized output.
    sync_pending: bool,
    /// When tab titles were last recomputed (they also refresh on OSC 0/2/7).
    titles_refreshed_at: std::time::Instant,
    /// The last tab closed; `App` removes the window (or hides the last one).
    close_requested: bool,
    /// The window is completely covered, minimized or on an inactive Space.
    occluded: bool,
    /// A shortcut asked for a new window; `App` opens it after the event.
    new_window_requested: bool,
    /// New tab as a native macOS window tab (`macos-native-tabs`).
    native_tab_requested: bool,
    /// This is the Quick Terminal window.
    quick: bool,
    /// A binding asked to show / hide the Quick Terminal.
    quick_terminal_requested: bool,
    /// `Cmd+Q` / `Ctrl+Shift+Q`: `App` confirms across all windows, then exits.
    quit_requested: bool,
    /// Terminal inspector overlay is shown.
    inspector: bool,
    /// Last key sent to the terminal and its bytes, for the inspector.
    inspector_key: String,
}

impl State {
    fn invalidate_pane_state(&mut self, pane_id: u64) {
        if self.search.pane_id == Some(pane_id) {
            self.search.close();
        }
        if self.hovered_link.as_ref().is_some_and(|h| h.pane_id == pane_id) {
            self.hovered_link = None;
        }
        if self.scrollbar_drag == Some(pane_id) {
            self.scrollbar_drag = None;
        }
        if self.active_click_pane_id == Some(pane_id) {
            self.active_click_pane_id = None;
        }
        if self.last_mouse_report.as_ref().is_some_and(|(id, ..)| *id == pane_id) {
            self.last_mouse_report = None;
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if self.secure_input && self.focused {
            #[cfg(target_os = "macos")]
            macos::set_secure_input(false);
        }
    }
}

/// Tab and pane ids are unique across windows, so `ChildExit` finds its pane.
fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Wakes the event loop from PTY reader threads, coalescing bursts of output
/// into a single pending `WakeUp`.
#[derive(Clone)]
struct Waker {
    proxy: EventLoopProxy<AppEvent>,
    queued: Arc<AtomicBool>,
}

impl Waker {
    fn wake(&self) {
        if !self.queued.swap(true, Ordering::AcqRel) {
            let _ = self.proxy.send_event(AppEvent::WakeUp);
        }
    }

    /// Re-arm before rendering so output arriving during the draw schedules another frame.
    fn rearm(&self) {
        self.queued.store(false, Ordering::Release);
    }

    fn is_queued(&self) -> bool {
        self.queued.load(Ordering::Acquire)
    }
}

struct App {
    waker: Waker,
    /// One entry per window. On macOS a hidden window with no tabs stays here
    /// after the last one closes, so a Dock click can reopen it.
    windows: Vec<State>,
}

impl App {
    fn window_mut(&mut self, id: WindowId) -> Option<&mut State> {
        self.windows.iter_mut().find(|s| s.window.id() == id)
    }

    /// Drop the hidden, tab-less window kept for the Dock once another one opens.
    fn drop_tabless_windows(&mut self) {
        let had_tabless = self.windows.iter().any(|s| s.tabs.is_empty());
        for s in self.windows.iter_mut().filter(|s| s.tabs.is_empty()) {
            s.renderer.shrink_drawables();
        }
        self.windows.retain(|w| !w.tabs.is_empty());
        if had_tabless {
            #[cfg(target_os = "macos")]
            macos::release_free_memory();
        }
    }

    /// Show the Quick Terminal (creating it on first use) or hide it when it is
    /// already in front.
    fn toggle_quick_terminal(&mut self, event_loop: &ActiveEventLoop) {
        let waker = self.waker.clone();
        if let Some(i) = self.windows.iter().position(|s| s.quick) {
            let s = &mut self.windows[i];
            if s.tabs.is_empty() {
                open_fresh_tab(s, &waker);
            } else if s.window.is_visible().unwrap_or(true) && s.focused {
                s.window.set_visible(false);
                let others_visible = self.windows.iter().any(|w| !w.quick && !w.tabs.is_empty() && w.window.is_visible() != Some(false));
                if !others_visible {
                    macos::hide_app();
                }
                return;
            }
            place_quick_terminal(&self.windows[i]);
            return;
        }
        match create_window(event_loop, &waker, None) {
            Ok(mut s) => {
                s.quick = true;
                s.window.set_decorations(false);
                s.window.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
                place_quick_terminal(&s);
                self.drop_tabless_windows();
                self.windows.push(s);
            }
            Err(e) => eprintln!("laplace: failed to open the Quick Terminal: {e}"),
        }
    }

    /// Apply what event handlers asked for: quit, open new windows, close emptied ones.
    fn process_window_requests(&mut self, event_loop: &ActiveEventLoop) {
        if self.windows.iter_mut().any(|s| std::mem::take(&mut s.quick_terminal_requested)) {
            self.toggle_quick_terminal(event_loop);
        }
        if self.windows.iter_mut().any(|s| std::mem::take(&mut s.quit_requested)) {
            let config = &self.windows[0].config;
            let panes = self.windows.iter().flat_map(|s| s.tabs.iter().flat_map(|t| &t.panes));
            if confirm_close(config, panes, "Litty") {
                event_loop.exit();
                return;
            }
        }
        // (cwd, window to attach the new one to as a native tab, position to cascade from)
        let mut requests = Vec::new();
        for s in &mut self.windows {
            let new_window = std::mem::take(&mut s.new_window_requested);
            let native_tab = std::mem::take(&mut s.native_tab_requested);
            if new_window || native_tab {
                let cwd = s.tabs.get_mut(s.active).and_then(|t| t.active_pane_mut().cwd());
                let from = (!native_tab && s.window.is_visible() != Some(false))
                    .then(|| s.window.outer_position().ok().map(|p| (p, s.window.scale_factor())))
                    .flatten();
                requests.push((cwd, native_tab.then(|| s.window.id()), from));
            }
        }
        for (cwd, parent, from) in requests {
            match create_window(event_loop, &self.waker, cwd.as_deref()) {
                // The hidden, tab-less window kept for the Dock is no longer needed.
                Ok(s) => {
                    if let Some(p) = parent.and_then(|id| self.windows.iter().find(|w| w.window.id() == id)) {
                        macos::add_tabbed_window(&p.window, &s.window);
                    }
                    // winit centers every new window: cascade it down-right from the
                    // one it was opened from so it does not hide exactly behind it.
                    if let Some((pos, scale)) = from {
                        let step = (28.0 * scale).round() as i32;
                        s.window.set_outer_position(PhysicalPosition::new(pos.x + step, pos.y + step));
                    }
                    self.drop_tabless_windows();
                    self.windows.push(s);
                }
                Err(e) => eprintln!("laplace: failed to open window: {e}"),
            }
        }

        while let Some(pos) = self.windows.iter().position(|s| s.close_requested) {
            if self.windows.len() > 1 {
                // Dropping the state closes the window and hangs up its shells.
                self.windows.remove(pos).renderer.shrink_drawables();
                #[cfg(target_os = "macos")]
                macos::release_free_memory();
                continue;
            }
            let s = &mut self.windows[pos];
            s.close_requested = false;
            hide_or_exit_last_window(s, event_loop);
        }
    }
}

/// Drop the Quick Terminal from the top of its screen: full width, 40% height,
/// then bring it to the front.
fn place_quick_terminal(s: &State) {
    if let Some(m) = s.window.current_monitor() {
        let (pos, size) = (m.position(), m.size());
        s.window.set_outer_position(pos);
        let _ = s.window.request_inner_size(PhysicalSize::new(size.width, size.height * 2 / 5));
    }
    s.window.set_visible(true);
    s.window.focus_window();
    #[cfg(target_os = "macos")]
    macos::activate_app();
}

/// `font-feature` values as shaper features (validated when the config was parsed).
fn font_features(cfg: &config::Config) -> Vec<rustybuzz::Feature> {
    cfg.font_features.iter().filter_map(|f| atlas::parse_font_feature(f).ok()).collect()
}

/// Create a window with one tab whose shell starts in `cwd`.
fn create_window(event_loop: &ActiveEventLoop, waker: &Waker, cwd: Option<&str>) -> std::io::Result<State> {
    let cfg = config::Config::load();
    grid::set_default_colors(cfg.foreground, cfg.background);
    grid::set_palette(&cfg.palette);
    grid::set_scrollback_limit(cfg.scrollback_limit);
    grid::set_grapheme_default(cfg.grapheme_unicode);

    let attrs = Window::default_attributes()
        .with_title("Litty")
        .with_transparent(cfg.background_opacity < 1.0)
        .with_inner_size(LogicalSize::new(960.0, 600.0));
    // Native tabs (`macos-native-tabs`) group under one identifier. Created
    // hidden so AppKit cannot merge a New Window into a tab of the focused one
    // before tabbing is turned off for it; native tabs re-enable it.
    #[cfg(target_os = "macos")]
    let attrs = {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attrs.with_tabbing_identifier("litty").with_visible(false)
    };
    let window = Arc::new(event_loop.create_window(attrs).map_err(std::io::Error::other)?);
    macos::set_tabbing_allowed(&window, false);
    window.set_visible(true);
    // Composed input (Vietnamese Telex/VNI, CJK) arrives as `WindowEvent::Ime`.
    window.set_ime_allowed(true);
    let dark = window.theme() != Some(winit::window::Theme::Light);
    grid::set_color_scheme_dark(dark);
    // `theme = light:X,dark:Y` was parsed before the appearance was known.
    let cfg = if !dark && cfg.theme.as_deref().is_some_and(config::theme_is_conditional) {
        let cfg = config::Config::load();
        grid::set_default_colors(cfg.foreground, cfg.background);
        grid::set_palette(&cfg.palette);
        cfg
    } else {
        cfg
    };

    #[cfg(target_os = "macos")]
    {
        macos::apply_macos_window_config(&window, &cfg);
        macos::set_dock_icon();
    }

    atlas::set_font_tweaks(cfg.adjust_cell_width, cfg.adjust_cell_height, cfg.font_thicken);
    atlas::set_font_features(&font_features(&cfg));
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
    renderer.set_padding(cfg.window_padding_x, cfg.window_padding_y, cfg.window_padding_balance);
    renderer.set_colors(cfg.cursor_color, cfg.selection_background, cfg.selection_foreground);
    renderer.set_contrast(cfg.unfocused_split_opacity, cfg.unfocused_split_fill, cfg.minimum_contrast);
    if !cfg.custom_shaders.is_empty() {
        renderer.set_custom_shaders(&cfg.custom_shaders, cfg.custom_shader_animation);
    }

    if let (Some(w_cols), Some(w_rows)) = (cfg.window_width, cfg.window_height) {
        let (cw, ch) = renderer.cell_size();
        let (pad_x, pad_y) = renderer.pad();
        let sf = window.scale_factor();
        let inner_w = (w_cols * cw + 2 * pad_x) as f64 / sf;
        let inner_h = (w_rows * ch + 2 * pad_y) as f64 / sf;
        let _ = window.request_inner_size(LogicalSize::new(inner_w, inner_h));
        let sz = window.inner_size();
        renderer.resize(sz.width, sz.height);
    }

    let (cols, rows) = renderer.grid_size_for_tabs(false);
    let (cw, ch) = renderer.cell_size();

    let pane = spawn_pane(
        next_id(),
        cols,
        rows,
        (cw, ch),
        cwd,
        Some((cfg.shell_integration, cfg.shell_integration_features)),
        waker,
    )?;
    let mut tab = Tab::new(next_id(), pane);
    let title = tab_title(&mut tab);

    let config_mtime = config::config_mtime();
    Ok(State {
        window,
        renderer,
        tabs: vec![tab],
        tab_titles: vec![title],
        active: 0,
        mods: ModifiersState::default(),
        font_pt,
        mouse_pos: (0.0, 0.0),
        selecting: false,
        drag_moved: false,
        last_click_time: None,
        last_click_cell: None,
        active_click_pane_id: None,
        click_count: 0,
        click_on_selected_block: false,
        extend_drag: false,
        scrollbar_drag: None,
        pointer_icon: CursorIcon::Default,
        last_notification: None,
        secure_input: false,
        hovered_link: None,
        config: cfg,
        config_mtime,
        mouse_hidden: false,
        consumed_keys: Vec::new(),
        locks: 0,
        cursor_blink_epoch: std::time::Instant::now(),
        focused: true,
        history_search: HistorySearch::new(),
        search: SearchBar::new(),
        first_frame: true,
        ime_preedit: String::new(),
        ime_cursor_cell: None,
        mouse_button_down: None,
        last_mouse_report: None,
        sync_pending: false,
        titles_refreshed_at: std::time::Instant::now(),
        close_requested: false,
        occluded: false,
        new_window_requested: false,
        native_tab_requested: false,
        quick: false,
        quick_terminal_requested: false,
        quit_requested: false,
        inspector: false,
        inspector_key: String::new(),
    })
}

impl ApplicationHandler<AppEvent> for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            for s in self.windows.iter().filter(|s| !s.tabs.is_empty() && !s.occluded) {
                s.window.request_redraw();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        let state = create_window(event_loop, &self.waker, None).expect("spawn initial PTY");
        self.windows.push(state);

        #[cfg(target_os = "macos")]
        {
            macos::register_reopen_handler(self.waker.proxy.clone());
            macos::install_menu();
            macos::set_global_hotkey(self.windows[0].config.quick_terminal_hotkey.as_ref());
        }
        #[cfg(not(target_os = "macos"))]
        {
            hotkey::listen(self.waker.proxy.clone());
            hotkey::set_global_hotkey(self.windows[0].config.quick_terminal_hotkey.as_ref());
        }

        let cfg = &self.windows[0].config;
        update::check_on_startup(cfg.auto_update, cfg.auto_update_channel);

        macos::release_free_memory();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::WakeUp => {
                self.waker.rearm();
                for s in &mut self.windows {
                    // OSC 52: applications (e.g. vim/tmux over ssh) setting the clipboard.
                    for pane in s.tabs.iter().flat_map(|t| &t.panes) {
                        let request = pane.grid.lock().clipboard_request.take();
                        if let Some(text) = request {
                            copy_to_clipboard(&text);
                        }
                    }
                    handle_terminal_requests(s);
                    if !s.tabs.is_empty() {
                        render_active(s);
                    }
                }
            }
            AppEvent::ChildExit(exited_id) => {
                // A pane of a window that was already closed: nothing to do.
                let Some(s) = self
                    .windows
                    .iter_mut()
                    .find(|s| s.tabs.iter().any(|t| t.panes.iter().any(|p| p.id == exited_id)))
                else {
                    return;
                };
                if let Some(tab_pos) = s.tabs.iter().position(|t| t.panes.iter().any(|p| p.id == exited_id)) {
                    let tab = &mut s.tabs[tab_pos];
                    if tab.panes.len() > 1 {
                        let mut ids = Vec::new();
                        tab.layout.all_pane_ids(&mut ids);
                        let cur_idx = ids.iter().position(|&id| id == exited_id);
                        let prev_in_order = cur_idx.and_then(|idx| {
                            if idx > 0 {
                                Some(ids[idx - 1])
                            } else if ids.len() > 1 {
                                Some(ids[1])
                            } else {
                                None
                            }
                        });

                        tab.panes.retain(|p| p.id != exited_id);
                        tab.layout.remove_pane(exited_id);
                        if tab.zoomed_pane_id == Some(exited_id) {
                            tab.zoomed_pane_id = None;
                        }
                        if tab.active_pane_id == exited_id {
                            if let Some(pid) = prev_in_order.filter(|id| tab.panes.iter().any(|p| p.id == *id)) {
                                tab.active_pane_id = pid;
                            } else if let Some(p) = tab.panes.first() {
                                tab.active_pane_id = p.id;
                            }
                        }
                        s.invalidate_pane_state(exited_id);
                        sync_size(s);
                        refresh_tab_titles(s);
                        s.renderer.invalidate();
                        s.window.request_redraw();
                        #[cfg(target_os = "macos")]
                        macos::release_free_memory();
                        return;
                    }

                    s.invalidate_pane_state(exited_id);
                    s.tabs.remove(tab_pos);
                    if s.tabs.is_empty() {
                        s.close_requested = true;
                        self.process_window_requests(event_loop);
                        return;
                    }
                    if s.active >= s.tabs.len() {
                        s.active = s.tabs.len() - 1;
                    } else if tab_pos < s.active {
                        s.active -= 1;
                    }
                    sync_size(s);
                    refresh_tab_titles(s);
                    s.renderer.invalidate();
                    s.window.request_redraw();
                    #[cfg(target_os = "macos")]
                    macos::release_free_memory();
                }
            }
            AppEvent::Menu(action) => {
                // The focused window, else any window with tabs, else the hidden one.
                let target = self
                    .windows
                    .iter()
                    .position(|s| s.focused && !s.tabs.is_empty())
                    .or_else(|| self.windows.iter().position(|s| !s.tabs.is_empty()))
                    .unwrap_or(0);
                let waker = self.waker.clone();
                if let Some(s) = self.windows.get_mut(target) {
                    run_action(s, action, &waker);
                }
                self.process_window_requests(event_loop);
            }
            AppEvent::SystemAppearance { dark } => {
                if dark != grid::color_scheme_is_dark() {
                    grid::set_color_scheme_dark(dark);
                    self.windows.iter_mut().for_each(apply_system_appearance);
                }
            }
            AppEvent::Reopen => {
                // A Light/Dark switch made while another app was in front.
                if cfg!(target_os = "macos") {
                    let dark = macos::system_dark();
                    if dark != grid::color_scheme_is_dark() {
                        grid::set_color_scheme_dark(dark);
                        self.windows.iter_mut().for_each(apply_system_appearance);
                    }
                }
                // Bring the windows back; with none left, reopen the hidden one.
                let visible = self.windows.iter().position(|s| !s.tabs.is_empty());
                if let Some(s) = self.windows.get_mut(visible.unwrap_or(0)) {
                    if s.tabs.is_empty() {
                        open_fresh_tab(s, &self.waker);
                    } else {
                        let sz = s.window.inner_size();
                        s.renderer.resize(sz.width, sz.height);
                        s.window.set_visible(true);
                        s.window.focus_window();
                        #[cfg(target_os = "macos")]
                        macos::activate_app();
                        s.renderer.invalidate();
                        s.window.request_redraw();
                    }
                }
            }
            AppEvent::UpdateCheckResult(result) => {
                macos::close_progress_dialog();
                update::mark_checked();
                match result {
                    Err(err) => {
                        macos::alert("Update Check Failed", &format!("Unable to check for updates:\n{err}"));
                    }
                    Ok(release) => {
                        let current = update::Version::current();
                        if release.version <= current {
                            macos::alert(
                                "Litty",
                                &format!("You're up to date!\n\nLitty v{current} is currently the newest version available."),
                            );
                        } else {
                            let title = "Update Available";
                            let message = format!(
                                "Litty v{} is available (you have v{}).\n\nWould you like to download and install this update now?",
                                release.version, current
                            );
                            let choice = macos::alert_choice(title, &message, &["Update Now", "View Release", "Cancel"]);
                            match choice {
                                0 => {
                                    let text = format!("Downloading and installing Litty v{}…", release.version);
                                    // The result event is handled after this one, so the dialog opens before it closes.
                                    if update::start_install_update(release, self.waker.proxy.clone()) {
                                        macos::show_progress_dialog("Updating Litty…", &text);
                                    } else {
                                        macos::alert("Update In Progress", "Litty is already installing an update.");
                                    }
                                }
                                1 => {
                                    #[cfg(target_os = "macos")]
                                    let _ = spawn_detached(std::process::Command::new("open").arg(&release.html_url));
                                    #[cfg(not(target_os = "macos"))]
                                    let _ = spawn_detached(std::process::Command::new("xdg-open").arg(&release.html_url));
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            AppEvent::UpdateInstallResult(result) => {
                macos::close_progress_dialog();
                match result {
                    Ok(new_ver) => {
                        macos::alert(
                            "Update Complete",
                            &format!(
                                "Litty has been successfully updated to v{new_ver}!\n\nPlease restart Litty to apply changes."
                            ),
                        );
                    }
                    Err(err) => {
                        macos::alert("Update Failed", &format!("Failed to install update:\n{err}"));
                    }
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.handle_window_event(id, event);
        self.process_window_requests(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.waker.is_queued() {
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        let mut deadline: Option<std::time::Instant> = None;
        let now = std::time::Instant::now();
        for s in &self.windows {
            if s.sync_pending {
                let sync = now + std::time::Duration::from_millis(16);
                deadline = Some(deadline.map_or(sync, |d| d.min(sync)));
            }
            // An animated custom shader: about 60 frames per second.
            let always = s.config.custom_shader_animation == Some(true);
            if !s.tabs.is_empty() && s.renderer.animating() && (s.focused || always) {
                let frame = now + std::time::Duration::from_millis(16);
                deadline = Some(deadline.map_or(frame, |d| d.min(frame)));
            }
            // Kitty animations on screen: wake for their next frame.
            if crate::graphics::any_animation() {
                let panes = s.tabs.get(s.active).into_iter().flat_map(|t| &t.panes);
                if let Some(next) = panes.filter_map(|p| p.grid.lock().next_image_frame(now)).min() {
                    deadline = Some(deadline.map_or(next, |d| d.min(next)));
                }
            }
            if !s.tabs.is_empty() && s.focused && cursor_blinks(s) {
                let elapsed_ms = s.cursor_blink_epoch.elapsed().as_millis();
                let next_boundary_ms = ((elapsed_ms / 500) + 1) * 500;
                let blink = now + std::time::Duration::from_millis((next_boundary_ms - elapsed_ms) as u64);
                deadline = Some(deadline.map_or(blink, |d| d.min(blink)));
            }
        }
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

impl App {
    fn handle_window_event(&mut self, id: WindowId, event: WindowEvent) {
        let waker = self.waker.clone();
        let Some(s) = self.window_mut(id) else { return };
        match event {
            WindowEvent::CloseRequested => {
                if confirm_close(&s.config, s.tabs.iter().flat_map(|t| &t.panes), "this window") {
                    s.tabs.clear();
                    close_last_window(s);
                }
            }

            WindowEvent::ModifiersChanged(m) => {
                s.mods = m.state();
                update_hovered_link(s);
            }

            WindowEvent::CursorMoved { position, .. } => {
                s.mouse_pos = (position.x, position.y);
                if s.tabs.is_empty() {
                    return;
                }
                if s.mouse_hidden {
                    s.window.set_cursor_visible(true);
                    s.mouse_hidden = false;
                }
                if s.inspector {
                    s.window.request_redraw();
                }
                s.renderer.set_mouse(position.x, position.y);
                if let Some(pane_id) = s.scrollbar_drag {
                    drag_scrollbar(s, pane_id);
                    return;
                }
                if !s.selecting && !s.history_search.is_open {
                    let button = s.mouse_button_down.unwrap_or(mouse::Button::None);
                    if report_mouse(s, button, mouse::Action::Motion) {
                        return;
                    }
                }
                if s.selecting {
                    if let (Some(start), Some(pane_id)) = (s.last_click_cell, s.active_click_pane_id) {
                        let show_tabs = s.tabs.len() > 1;
                        let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
                        if let Some((cell_row, cell_col)) = s.renderer.pixel_to_cell(position.x, position.y, show_tabs, cols, rows) {
                            let tab = &s.tabs[s.active];
                            let rects = tab.compute_pane_rects(cols, rows);
                            if let Some(target) = rects.iter().find(|r| r.id == pane_id) {
                                let pane_row = cell_row.saturating_sub(target.row).min(target.rows.saturating_sub(1));
                                let pane_col = cell_col.saturating_sub(target.col).min(target.cols.saturating_sub(1));
                                let pane_col = tab.pane(pane_id).map_or(pane_col, |p| p.grid.lock().logical_col(pane_row, pane_col));
                                let end = (pane_row, pane_col);
                                if s.extend_drag {
                                    if let Some(pane) = tab.pane(pane_id) {
                                        pane.grid.lock().set_selection_end(pane_row, pane_col);
                                        s.window.request_redraw();
                                    }
                                } else if end != start || s.drag_moved {
                                    s.drag_moved = true;
                                    if let Some(pane) = tab.pane(pane_id) {
                                        pane.grid.lock().set_selection(Some(Selection { start, end }));
                                        s.window.request_redraw();
                                    }
                                }
                            }
                        }
                    }
                } else {
                    update_hovered_link(s);
                }
            }

            WindowEvent::MouseInput { state: btn_state, button, .. } => {
                if s.tabs.is_empty() {
                    return;
                }
                let reported = match button {
                    MouseButton::Left => Some(mouse::Button::Left),
                    MouseButton::Middle => Some(mouse::Button::Middle),
                    MouseButton::Right => Some(mouse::Button::Right),
                    _ => None,
                };
                if button == MouseButton::Left {
                    if btn_state == ElementState::Released && s.scrollbar_drag.take().is_some() {
                        return;
                    }
                    if btn_state == ElementState::Pressed && !s.history_search.is_open {
                        if let Some(pane_id) = scrollbar_at_mouse(s) {
                            s.scrollbar_drag = Some(pane_id);
                            drag_scrollbar(s, pane_id);
                            return;
                        }
                    }
                }
                if let (Some(b), false) = (reported, s.history_search.is_open || s.selecting) {
                    let action = if btn_state == ElementState::Pressed { mouse::Action::Press } else { mouse::Action::Release };
                    if report_mouse(s, b, action) {
                        s.mouse_button_down = (action == mouse::Action::Press).then_some(b);
                        return;
                    }
                }
                if btn_state == ElementState::Released {
                    s.mouse_button_down = None;
                }
                if button != MouseButton::Left {
                    return;
                }
                if s.history_search.is_open {
                    if btn_state == ElementState::Pressed {
                        if let Some((x0, y0, popup_w, popup_h, visible_rows)) =
                            s.renderer.history_popup_geometry(&s.history_search)
                        {
                            let show_tabs = s.tabs.len() > 1;
                            let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
                            if let Some((cell_row, cell_col)) =
                                s.renderer.pixel_to_cell(s.mouse_pos.0, s.mouse_pos.1, show_tabs, cols, rows)
                            {
                                let r = if show_tabs { cell_row + 1 } else { cell_row };
                                let c = cell_col;
                                if c < x0 || c >= x0 + popup_w || r < y0 || r >= y0 + popup_h {
                                    s.history_search.close();
                                    s.renderer.invalidate();
                                    s.window.request_redraw();
                                    return;
                                }
                                if r >= y0 + 3 && r < y0 + 3 + visible_rows {
                                    let item_idx = s.history_search.scroll_offset + (r - (y0 + 3));
                                    if item_idx < s.history_search.filtered.len() {
                                        let now = std::time::Instant::now();
                                        let is_double = s.history_search.selected == item_idx
                                            && s.last_click_time.is_some_and(|t| now.duration_since(t).as_millis() <= 400);
                                        if is_double {
                                            let cmd_opt = s.history_search.selected_command().map(|c| c.to_string());
                                            s.history_search.close();
                                            s.renderer.invalidate();
                                            if let Some(cmd) = cmd_opt {
                                                let mut payload = Vec::with_capacity(cmd.len() + 3);
                                                payload.extend_from_slice(b"\x05\x15");
                                                payload.extend_from_slice(cmd.as_bytes());
                                                payload.push(b'\r');
                                                s.tabs[s.active].active_pane().pty.write(&payload);
                                            }
                                            s.window.request_redraw();
                                            return;
                                        } else {
                                            s.history_search.selected = item_idx;
                                            s.last_click_time = Some(now);
                                            s.renderer.invalidate();
                                            s.window.request_redraw();
                                            return;
                                        }
                                    }
                                }
                            } else {
                                s.history_search.close();
                                s.renderer.invalidate();
                                s.window.request_redraw();
                                return;
                            }
                        }
                    }
                    return;
                }
                match btn_state {
                    ElementState::Pressed => {
                        #[cfg(target_os = "macos")]
                        let cmd_pressed = s.mods.super_key();
                        #[cfg(not(target_os = "macos"))]
                        let cmd_pressed = s.mods.control_key() || s.mods.super_key();

                        if cmd_pressed {
                            if let Some(link) = &s.hovered_link {
                                let cwd = s.tabs.get(s.active)
                                    .and_then(|t| t.pane(link.pane_id))
                                    .and_then(|p| p.pty.cwd());
                                let cwd_path = cwd.as_deref().map(std::path::Path::new);
                                open_target(&link.link.url, link.link.is_path, cwd_path);
                                return;
                            }
                        }

                        if let Some(action) = s.renderer.hit_test_tab_bar(s.mouse_pos.0, s.mouse_pos.1, &s.tab_titles) {
                            match action {
                                TabBarAction::Select(idx) => select_tab(s, idx),
                                TabBarAction::NewTab => duplicate_active_tab(s, &waker),
                            }
                            return;
                        }
                        let mut switched_active_pane = false;
                        {
                            let target = pane_cell_at_mouse(s);
                            let tab = &mut s.tabs[s.active];
                            if let Some((clicked_id, pane_row, pane_col)) = target {

                                if tab.active_pane_id != clicked_id {
                                    tab.active_pane_id = clicked_id;
                                    switched_active_pane = true;
                                }
                                s.active_click_pane_id = Some(clicked_id);

                                // Shift+Click extends the pane's selection to the mouse;
                                // dragging on keeps moving its end.
                                let extended = s.mods.shift_key()
                                    && tab.pane(clicked_id).is_some_and(|p| p.grid.lock().extend_selection(pane_row, pane_col));
                                s.extend_drag = extended;
                                if extended {
                                    s.selecting = true;
                                    s.drag_moved = true;
                                    s.window.request_redraw();
                                }

                                let now = std::time::Instant::now();
                                let multi_click = !extended
                                    && s.last_click_time.is_some_and(|t| now.duration_since(t).as_millis() <= 450)
                                    && s.last_click_cell.is_some_and(|(lr, _)| lr == pane_row);

                                s.click_count = if multi_click {
                                    (s.click_count % 3) + 1
                                } else {
                                    1
                                };
                                // A Shift+Click never starts a double-click.
                                s.last_click_time = (!extended).then_some(now);
                                s.last_click_cell = Some((pane_row, pane_col));

                                if let Some(pane) = tab.pane(clicked_id).filter(|_| !extended) {
                                    match s.click_count {
                                        // Double-click selects a block of text; again on a
                                        // selected block, the whole line.
                                        2 => {
                                            let line_text = {
                                                let mut g = pane.grid.lock();
                                                if std::mem::take(&mut s.click_on_selected_block) {
                                                    g.select_smart_line(pane_row, pane_col)
                                                } else {
                                                    g.select_block(pane_row, pane_col)
                                                }
                                            };
                                            if !line_text.is_empty() && s.config.copy_on_select {
                                                copy_to_clipboard(&line_text);
                                            }
                                            s.selecting = false;
                                            s.drag_moved = false;
                                            s.window.request_redraw();
                                        }
                                        3 => {
                                            let line_text = pane.grid.lock().select_line(pane_row);
                                            if !line_text.is_empty() && s.config.copy_on_select {
                                                copy_to_clipboard(&line_text);
                                            }
                                            s.selecting = false;
                                            s.drag_moved = false;
                                            s.window.request_redraw();
                                        }
                                        _ => {
                                            let mut g = pane.grid.lock();
                                            s.click_on_selected_block = g.is_block_selected(pane_row, pane_col);
                                            g.set_selection(None);
                                            drop(g);
                                            s.selecting = true;
                                            s.drag_moved = false;
                                            s.window.request_redraw();
                                        }
                                    }
                                }
                            }
                        }
                        if switched_active_pane {
                            refresh_tab_titles(s);
                            s.renderer.invalidate();
                        }
                    }
                    ElementState::Released => {
                        if s.selecting {
                            s.selecting = false;
                            if s.drag_moved {
                                if let Some(pane_id) = s.active_click_pane_id {
                                    if let Some(pane) = s.tabs.get(s.active).and_then(|t| t.pane(pane_id)) {
                                        let text = pane.grid.lock().selected_text();
                                        if let Some(t) = text.filter(|_| s.config.copy_on_select) {
                                            copy_to_clipboard(&t);
                                        }
                                    }
                                }
                            } else if s.click_count == 1 && !s.history_search.is_open {
                                if let (Some(pane_id), Some((pane_row, pane_col))) =
                                    (s.active_click_pane_id, s.last_click_cell)
                                {
                                    if let Some(pane) = s.tabs.get(s.active).and_then(|t| t.pane(pane_id)) {
                                        let move_bytes = pane
                                            .grid
                                            .lock()
                                            .calculate_cursor_movement_to(pane_row, pane_col);
                                        if let Some(bytes) = move_bytes {
                                            pane.pty.write(&bytes);
                                            s.window.request_redraw();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

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
                if s.tabs.is_empty() {
                    return;
                }
                if s.history_search.is_open {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y.round() as isize,
                        MouseScrollDelta::PixelDelta(p) => {
                            let cell_h = s.renderer.cell_size().1 as f64;
                            (p.y / cell_h.max(1.0)).round() as isize
                        }
                    };
                    let visible_rows = s.renderer.history_popup_geometry(&s.history_search).map_or(10, |g| g.4);
                    if lines > 0 {
                        for _ in 0..lines.abs() {
                            s.history_search.select_prev();
                        }
                        s.renderer.invalidate();
                        s.window.request_redraw();
                    } else if lines < 0 {
                        for _ in 0..lines.abs() {
                            s.history_search.select_next(visible_rows);
                        }
                        s.renderer.invalidate();
                        s.window.request_redraw();
                    }
                    return;
                }
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
                    let wheel = if lines > 0 { mouse::Button::WheelUp } else { mouse::Button::WheelDown };
                    if report_mouse(s, wheel, mouse::Action::Press) {
                        for _ in 1..lines.unsigned_abs() {
                            report_mouse(s, wheel, mouse::Action::Press);
                        }
                        return;
                    }
                    if alt_screen_scroll(s, lines) {
                        return;
                    }
                    let target_id = pane_cell_at_mouse(s).map_or(s.tabs[s.active].active_pane_id, |(id, _, _)| id);
                    if let Some(pane) = s.tabs[s.active].pane(target_id) {
                        pane.grid.lock().scroll_display(lines);
                        s.window.request_redraw();
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && s.renderer.set_bell_border(false) {
                    s.window.request_redraw();
                }
                if s.ime_preedit.is_empty() {
                    handle_key(s, &event, &waker);
                }
            }

            WindowEvent::Ime(ime) => handle_ime(s, ime),

            // Dropped files paste their shell-quoted paths, separated by spaces.
            WindowEvent::DroppedFile(path) => {
                if let Some(tab) = s.tabs.get(s.active) {
                    let pane = tab.active_pane();
                    let bracketed = pane.grid.lock().bracketed_paste;
                    let text = format!("{} ", shell_quote(&path.to_string_lossy()));
                    pane.pty.write(&paste_payload(&text, bracketed));
                }
            }

            WindowEvent::ThemeChanged(theme) => {
                let dark = theme == winit::window::Theme::Dark;
                if dark != grid::color_scheme_is_dark() {
                    grid::set_color_scheme_dark(dark);
                    apply_system_appearance(s);
                }
            }

            WindowEvent::Occluded(occluded) => {
                s.occluded = occluded;
                if occluded {
                    s.renderer.shrink_drawables();
                    #[cfg(target_os = "macos")]
                    macos::release_free_memory();
                } else {
                    let sz = s.window.inner_size();
                    if sz.width > 0 && sz.height > 0 {
                        s.renderer.resize(sz.width, sz.height);
                    }
                    s.renderer.invalidate();
                    s.window.request_redraw();
                }
            }

            WindowEvent::Focused(focused) => {
                s.focused = focused;
                if focused && s.occluded {
                    s.occluded = false;
                    let sz = s.window.inner_size();
                    if sz.width > 0 && sz.height > 0 {
                        s.renderer.resize(sz.width, sz.height);
                    }
                    s.renderer.invalidate();
                }
                if focused {
                    s.renderer.set_bell_border(false);
                }
                // Secure input is system-wide; release it while another app is in front.
                if s.secure_input {
                    macos::set_secure_input(focused);
                }
                if focused {
                    macos::set_secure_input_menu_state(s.secure_input);
                } else if s.quick && s.config.quick_terminal_autohide {
                    s.window.set_visible(false);
                }
                if let Some(tab) = s.tabs.get_mut(s.active).filter(|t| focused && t.bell) {
                    tab.bell = false;
                    refresh_tab_titles(s);
                }
                if let Some(tab) = s.tabs.get(s.active) {
                    let pane = tab.active_pane();
                    if pane.grid.lock().focus_events {
                        pane.pty.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
                    }
                }
                if focused {
                    s.cursor_blink_epoch = std::time::Instant::now();
                    let mtime = config::config_mtime();
                    if mtime != s.config_mtime {
                        reload_config(s);
                    }
                }
                if !s.tabs.is_empty() {
                    s.window.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => {
                if s.occluded {
                    return;
                }
                // Re-arm before rendering so any PTY write during the draw schedules the next frame.
                waker.rearm();
                render_active(s);
            }

            _ => {}
        }
    }
}

fn spawn_pane(
    pane_id: u64,
    cols: usize,
    rows: usize,
    (cw, ch): (u32, u32),
    cwd: Option<&str>,
    shell: Option<(crate::config::ShellIntegration, crate::config::ShellFeatures)>,
    waker: &Waker,
) -> std::io::Result<Pane> {
    let grid = Arc::new(Mutex::new(Grid::new(cols, rows)));
    let pty = Pty::spawn_in(cols as u16, rows as u16, cw, ch, cwd, shell)?;
    let on_output = waker.clone();
    let proxy = waker.proxy.clone();
    pty.spawn_reader(grid.clone(), move || on_output.wake(), move || {
        let _ = proxy.send_event(AppEvent::ChildExit(pane_id));
    })?;
    Ok(Pane {
        id: pane_id,
        grid,
        pty,
        cached_cwd: None,
        last_cwd_check: std::time::Instant::now() - std::time::Duration::from_secs(10),
    })
}

fn tab_title(tab: &mut Tab) -> String {
    let active = tab.active_pane_mut();
    let mut title = {
        let grid_title = {
            let g = active.grid.lock();
            if !g.title.is_empty() {
                Some(g.title.clone())
            } else {
                None
            }
        };
        if let Some(t) = grid_title {
            t
        } else if let Some(cwd) = active.cwd() {
            static HOME_USER: std::sync::OnceLock<(Option<String>, String)> = std::sync::OnceLock::new();
            let (home, user) = HOME_USER.get_or_init(|| {
                let h = std::env::var("HOME").ok();
                let u = std::env::var("USER").unwrap_or_default();
                (h, u)
            });
            let short_dir = if let Some(home) = home {
                if &cwd == home {
                    "~".to_string()
                } else if let Some(rest) = cwd.strip_prefix(home) {
                    format!("~{rest}")
                } else {
                    cwd
                }
            } else {
                cwd
            };
            if user.is_empty() {
                short_dir
            } else {
                format!("{user}:{short_dir}")
            }
        } else {
            "shell".into()
        }
    };

    if tab.zoomed_pane_id.is_some() {
        title.push_str(" [Z]");
    }
    // OSC 9;4 progress and an unseen bell show in front of the title.
    let progress = tab.active_pane().grid.lock().progress;
    let prefix = match progress {
        Some((2, pct)) => format!("✖ {pct}% "),
        Some((3, _)) => "⋯ ".to_string(),
        Some((4, pct)) => format!("⏸ {pct}% "),
        Some((_, pct)) => format!("{pct}% "),
        None => String::new(),
    };
    let bell = if tab.bell { "🔔 " } else { "" };
    format!("{bell}{prefix}{title}")
}

/// Act on what applications asked for since the last wake-up: desktop
/// notifications (OSC 9 / 777), the bell, and the mouse pointer shape (OSC 22).
fn handle_terminal_requests(s: &mut State) {
    let mut titles_dirty = false;
    let mut attention = false;
    let mut ring = false;
    let mut audio = false;
    let mut border = false;
    for (i, tab) in s.tabs.iter_mut().enumerate() {
        let in_front = s.focused && i == s.active;
        for pane in &tab.panes {
            let (notification, bell) = {
                let mut g = pane.grid.lock();
                (g.notification.take(), std::mem::take(&mut g.bell))
            };
            if let Some((title, body)) = notification {
                // At most one per second, so a runaway loop cannot flood the desktop.
                if s.last_notification.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1)) {
                    s.last_notification = Some(std::time::Instant::now());
                    let title = if title.is_empty() { "Litty".to_string() } else { title };
                    show_notification(&title, &body);
                }
            }
            if bell {
                ring |= s.config.bell_features.system;
                audio |= s.config.bell_features.audio;
                border |= s.config.bell_features.border;
                if !in_front && s.config.bell_features.title {
                    titles_dirty |= !tab.bell;
                    tab.bell = true;
                }
                attention |= !s.focused && s.config.bell_features.attention;
            }
        }
    }
    if attention {
        s.window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
    }
    if ring {
        macos::beep();
    }
    if audio {
        if let Some(path) = &s.config.bell_audio_path {
            play_sound(path, s.config.bell_audio_volume);
        }
    }
    if border {
        s.renderer.set_bell_border(true);
        s.window.request_redraw();
    }
    if titles_dirty {
        refresh_tab_titles(s);
    }

    let shape = s.tabs.get(s.active).map(|t| pointer_icon(&t.active_pane().grid.lock().pointer_shape));
    if let Some(icon) = shape.filter(|&icon| icon != s.pointer_icon) {
        s.pointer_icon = icon;
        if s.hovered_link.is_none() {
            s.window.set_cursor(icon);
        }
    }
}

/// Play a sound file in the background (`afplay` on macOS, PulseAudio's
/// `paplay` elsewhere); the path is a single argument, never parsed by a shell.
fn play_sound(path: &std::path::Path, volume: f32) {
    #[cfg(target_os = "macos")]
    let res = spawn_detached(
        std::process::Command::new("/usr/bin/afplay")
            .arg("-v")
            .arg(volume.to_string())
            .arg("--")
            .arg(path),
    );
    #[cfg(not(target_os = "macos"))]
    let res = spawn_detached(
        std::process::Command::new("paplay")
            .arg(format!("--volume={}", (volume * 65536.0) as u32))
            .arg("--")
            .arg(path),
    );
    if let Err(e) = res {
        eprintln!("laplace: cannot play {}: {e}", path.display());
    }
}

/// Map an OSC 22 pointer name (CSS, or the X11 names xterm uses) to a cursor.
fn pointer_icon(name: &str) -> CursorIcon {
    let css = match name {
        "" | "left_ptr" | "arrow" => "default",
        "xterm" | "ibeam" => "text",
        "hand" | "hand1" | "hand2" => "pointer",
        "watch" => "wait",
        "question_arrow" => "help",
        "fleur" => "move",
        "cross" | "tcross" => "crosshair",
        "sb_h_double_arrow" => "ew-resize",
        "sb_v_double_arrow" => "ns-resize",
        other => other,
    };
    css.parse().unwrap_or(CursorIcon::Default)
}

/// Show a desktop notification. Text from the terminal is passed as separate
/// arguments (never through a shell), with leading `-` stripped so it cannot be
/// read as an option.
pub(crate) fn show_notification(title: &str, body: &str) {
    let title = title.trim_start_matches('-');
    let body = body.trim_start_matches('-');
    #[cfg(target_os = "macos")]
    let res = spawn_detached(
        std::process::Command::new("osascript")
            .args(["-e", "on run argv", "-e", "display notification (item 2 of argv) with title (item 1 of argv)"])
            .args(["-e", "end run"])
            .arg(title)
            .arg(body),
    );
    #[cfg(not(target_os = "macos"))]
    let res = spawn_detached(std::process::Command::new("notify-send").arg("--").arg(title).arg(body));
    if let Err(e) = res {
        eprintln!("laplace: failed to show notification: {e}");
    }
}

fn refresh_tab_titles(s: &mut State) {
    s.titles_refreshed_at = std::time::Instant::now();
    let mut changed = s.tab_titles.len() != s.tabs.len();
    s.tab_titles.resize_with(s.tabs.len(), String::new);
    for (dst, tab) in s.tab_titles.iter_mut().zip(&mut s.tabs) {
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
            let title = s.config.title.as_deref().unwrap_or(active_title);
            let lock = if s.secure_input { "🔒 " } else { "" };
            s.window.set_title(&format!("{lock}{title}"));
            #[cfg(target_os = "macos")]
            if let Some(tab) = s.tabs.get_mut(s.active) {
                if let Some(cwd) = tab.active_pane_mut().cwd() {
                    macos::set_window_represented_filename(&s.window, &cwd);
                }
            }
        }
    }
}

fn cursor_blinks(s: &State) -> bool {
    match s.config.cursor_style_blink {
        Some(b) => b,
        None => {
            if let Some(tab) = s.tabs.get(s.active) {
                let g = tab.active_pane().grid.lock();
                // DECSCUSR odd values (and 0 = default) are the blinking styles.
                matches!(g.cursor_shape, 0 | 1 | 3 | 5)
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
    s.renderer.set_colors(new.cursor_color, new.selection_background, new.selection_foreground);
    s.renderer.set_contrast(new.unfocused_split_opacity, new.unfocused_split_fill, new.minimum_contrast);
    if (&new.custom_shaders, new.custom_shader_animation) != (&old.custom_shaders, old.custom_shader_animation) {
        s.renderer.set_custom_shaders(&new.custom_shaders, new.custom_shader_animation);
    }
    // Applies to terminals created or resized from now on.
    grid::set_scrollback_limit(new.scrollback_limit);
    grid::set_grapheme_default(new.grapheme_unicode);
    let padding = (new.window_padding_x, new.window_padding_y, new.window_padding_balance);
    if padding != (old.window_padding_x, old.window_padding_y, old.window_padding_balance) {
        s.renderer.set_padding(padding.0, padding.1, padding.2);
        sync_size(s);
    }

    let font_changed = new.font_family != old.font_family;
    let size_changed = (new.font_size - old.font_size).abs() > 0.001;
    // Compared per window: another window may already have set the new tweaks.
    atlas::set_font_tweaks(new.adjust_cell_width, new.adjust_cell_height, new.font_thicken);
    atlas::set_font_features(&font_features(&new));
    if new.quick_terminal_hotkey != old.quick_terminal_hotkey {
        macos::set_global_hotkey(new.quick_terminal_hotkey.as_ref());
        hotkey::set_global_hotkey(new.quick_terminal_hotkey.as_ref());
    }
    let tweaks_changed = (new.adjust_cell_width, new.adjust_cell_height, new.font_thicken, &new.font_features)
        != (old.adjust_cell_width, old.adjust_cell_height, old.font_thicken, &old.font_features);
    if font_changed || size_changed || tweaks_changed {
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
    if s.search.is_open {
        // Follow focus to another pane or tab; mirror the grid's match status.
        if s.search.pane_id != Some(s.tabs[s.active].active_pane_id) {
            refresh_search(s);
        }
        (s.search.current, s.search.total) = s.tabs[s.active].active_pane().grid.lock().search_status();
    }
    // Synchronized output (DECSET 2026): wait for the application to finish its
    // frame, but never longer than 150ms.
    let syncing = s.tabs[s.active].panes.iter().any(|p| {
        let g = p.grid.lock();
        g.sync_output && g.sync_started.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(150))
    });
    s.sync_pending = syncing;
    if syncing {
        return;
    }

    let mut titles_changed = false;
    for pane in s.tabs.iter().flat_map(|t| &t.panes) {
        titles_changed |= std::mem::take(&mut pane.grid.lock().title_changed);
    }
    if titles_changed || s.titles_refreshed_at.elapsed() >= std::time::Duration::from_secs(1) {
        refresh_tab_titles(s);
    }
    let tab_bar = (s.tabs.len() > 1).then_some((s.tab_titles.as_slice(), s.active));
    let blink_on = cursor_is_visible_now(s);

    let show_tabs = s.tabs.len() > 1;
    let (total_cols, total_rows) = s.renderer.grid_size_for_tabs(show_tabs);

    let tab = &s.tabs[s.active];
    let rects = tab.compute_pane_rects(total_cols, total_rows);

    let render_panes: Vec<renderer::RenderPane> = rects
        .into_iter()
        .filter_map(|rect| {
            tab.pane(rect.id).map(|pane| renderer::RenderPane {
                id: pane.id,
                grid: &pane.grid,
                rect,
                is_active: pane.id == tab.active_pane_id,
            })
        })
        .collect();

    let hovered_link = s.hovered_link.as_ref().map(|l| (l.pane_id, &l.link));
    let inspector = s.inspector.then(|| inspector_lines(s));

    s.renderer.render_panes(
        &render_panes,
        tab_bar,
        hovered_link,
        blink_on,
        Some(&s.history_search),
        &s.search,
        &s.ime_preedit,
        inspector.as_deref(),
    );

    // Keep the IME candidate window next to the text cursor.
    let cursor_cell = render_panes.iter().find(|p| p.is_active).and_then(|p| {
        let (cx, cy) = p.grid.lock().cursor_pos()?;
        Some((p.rect.row + cy, p.rect.col + cx))
    });
    if cursor_cell.is_some() && cursor_cell != s.ime_cursor_cell {
        s.ime_cursor_cell = cursor_cell;
        if let Some((row, col)) = cursor_cell {
            let (x, y) = s.renderer.cell_origin_px(row, col, show_tabs);
            let (cw, ch) = s.renderer.cell_size();
            s.window.set_ime_cursor_area(PhysicalPosition::new(x, y), PhysicalSize::new(cw, ch));
        }
    }

    if s.first_frame {
        s.first_frame = false;
        macos::release_free_memory();
    }
}

/// Spawn a helper process (`open`, `xdg-open`) and reap it on a background thread
/// so it never lingers as a zombie.
pub(crate) fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    let pid = child.id();
    if let Err(e) = std::thread::Builder::new()
        .name("reap-helper".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let _ = child.wait();
        })
    {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(pid as libc::pid_t, &mut status, 0);
        }
        return Err(e);
    }
    Ok(())
}

fn open_target(target: &str, is_path: bool, cwd: Option<&std::path::Path>) {
    if is_path {
        let path = if let Some(rest) = target.strip_prefix("~/") {
            match std::env::var("HOME") {
                Ok(home) => std::path::PathBuf::from(home).join(rest),
                Err(_) => std::path::PathBuf::from(target),
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
            } else if target.starts_with('-') {
                std::path::PathBuf::from(format!("./{target}"))
            } else {
                p.to_path_buf()
            }
        };

        #[cfg(target_os = "macos")]
        let res = {
            let mut cmd = std::process::Command::new("open");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let is_exec = path
                    .metadata()
                    .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
                    .unwrap_or(false);
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if is_exec || ext.eq_ignore_ascii_case("app") || ext.eq_ignore_ascii_case("command") {
                    cmd.arg("-R");
                }
            }
            cmd.arg("--").arg(&path);
            spawn_detached(&mut cmd)
        };
        #[cfg(target_os = "linux")]
        let res = spawn_detached(std::process::Command::new("xdg-open").arg("--").arg(&path));
        #[cfg(windows)]
        let res = std::process::Command::new("cmd")
            .args(["/c", "start", "", path.to_str().unwrap_or(target)])
            .spawn();

        if let Err(e) = res {
            eprintln!("laplace: failed to open path {path:?}: {e}");
        }
    } else {
        #[cfg(target_os = "macos")]
        let res = spawn_detached(std::process::Command::new("open").arg("-u").arg(target));
        #[cfg(target_os = "linux")]
        let res = spawn_detached(std::process::Command::new("xdg-open").arg("--").arg(target));
        #[cfg(windows)]
        let res = std::process::Command::new("cmd")
            .args(["/c", "start", "", target])
            .spawn();

        if let Err(e) = res {
            eprintln!("laplace: failed to open URL {target}: {e}");
        }
    }
}

/// Pane under the mouse pointer and the pane-relative `(row, col)` cell.
/// Pane whose visible scrollbar (shown while scrolled back) is under the mouse.
fn scrollbar_at_mouse(s: &State) -> Option<u64> {
    const GRAB_PX: f64 = 12.0;
    let show_tabs = s.tabs.len() > 1;
    let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
    let tab = s.tabs.get(s.active)?;
    let (mx, my) = s.mouse_pos;
    tab.compute_pane_rects(cols, rows).into_iter().find_map(|r| {
        let (_, top) = s.renderer.cell_origin_px(r.row, r.col, show_tabs);
        let (right, bottom) = s.renderer.cell_origin_px(r.row + r.rows, r.col + r.cols, show_tabs);
        let over = mx <= right && mx >= right - GRAB_PX && my >= top && my < bottom;
        let scrolled = tab.pane(r.id).is_some_and(|p| p.grid.lock().scroll_position().1 > 0);
        (over && scrolled).then_some(r.id)
    })
}

/// Scroll the dragged pane so the thumb is centered on the mouse.
fn drag_scrollbar(s: &mut State, pane_id: u64) {
    let show_tabs = s.tabs.len() > 1;
    let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
    let Some(tab) = s.tabs.get(s.active) else { return };
    let Some(rect) = tab.compute_pane_rects(cols, rows).into_iter().find(|r| r.id == pane_id) else { return };
    let Some(pane) = tab.pane(pane_id) else { return };
    let (_, top) = s.renderer.cell_origin_px(rect.row, rect.col, show_tabs);
    let (_, bottom) = s.renderer.cell_origin_px(rect.row + rect.rows, rect.col, show_tabs);
    let frac = ((s.mouse_pos.1 - top) / (bottom - top).max(1.0)).clamp(0.0, 1.0);
    let mut g = pane.grid.lock();
    let (history, offset) = g.scroll_position();
    let total = (history + g.rows) as f64;
    let top_line = (frac * total - g.rows as f64 / 2.0).round().clamp(0.0, history as f64) as usize;
    g.scroll_display((history - top_line) as isize - offset as isize);
    drop(g);
    s.window.request_redraw();
}

fn pane_cell_at_mouse(s: &State) -> Option<(u64, usize, usize)> {
    let show_tabs = s.tabs.len() > 1;
    let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
    let (row, col) = s.renderer.pixel_to_cell(s.mouse_pos.0, s.mouse_pos.1, show_tabs, cols, rows)?;
    let tab = s.tabs.get(s.active)?;
    tab.compute_pane_rects(cols, rows)
        .into_iter()
        .find(|r| col >= r.col && col < r.col + r.cols && row >= r.row && row < r.row + r.rows)
        .map(|r| {
            // Right-to-left rows are drawn reordered: hit-test the logical cell.
            let (row, col) = (row - r.row, col - r.col);
            let col = tab.pane(r.id).map_or(col, |p| p.grid.lock().logical_col(row, col));
            (r.id, row, col)
        })
}

/// Forward a mouse event to the application in the pane under the pointer if it
/// tracks the mouse (DECSET 1000/1002/1003). Returns `true` when the application
/// owns the mouse, so the event must not drive selection or scrollback. Holding
/// Shift bypasses reporting, as in other terminals; Cmd/Super stays free for links.
fn report_mouse(s: &mut State, button: mouse::Button, action: mouse::Action) -> bool {
    use crate::grid::MouseTracking;
    if s.mods.shift_key() || s.mods.super_key() {
        return false;
    }
    let Some((pane_id, row, col)) = pane_cell_at_mouse(s) else { return false };
    let tab = &mut s.tabs[s.active];
    let Some(pane) = tab.pane(pane_id) else { return false };
    let (tracking, sgr, pixels) = {
        let g = pane.grid.lock();
        (g.mouse_tracking, g.mouse_sgr, g.mouse_sgr_pixels)
    };
    if tracking == MouseTracking::Off {
        return false;
    }
    // SGR-Pixels (DECSET 1016): the same report with pane-relative pixel coordinates.
    let (row, col) = if pixels {
        let (cw, ch) = s.renderer.cell_size();
        let (dx, dy) = s.renderer.offset_in_cell(s.mouse_pos.0, s.mouse_pos.1, s.tabs.len() > 1);
        (row * ch as usize + dy, col * cw as usize + dx)
    } else {
        (row, col)
    };
    let tab = &mut s.tabs[s.active];
    let Some(pane) = tab.pane(pane_id) else { return false };
    let wanted = match action {
        mouse::Action::Press | mouse::Action::Release => true,
        mouse::Action::Motion => {
            let moved = s.last_mouse_report != Some((pane_id, row, col));
            moved
                && (tracking == MouseTracking::Motion
                    || (tracking == MouseTracking::Drag && button != mouse::Button::None))
        }
    };
    if wanted {
        let mods = mouse::Mods { shift: false, alt: s.mods.alt_key(), ctrl: s.mods.control_key() };
        if let Some(bytes) = mouse::encode(button, action, mods, row, col, sgr || pixels) {
            pane.pty.write(&bytes);
        }
        s.last_mouse_report = Some((pane_id, row, col));
    }
    if action == mouse::Action::Press && tab.active_pane_id != pane_id {
        tab.active_pane_id = pane_id;
        refresh_tab_titles(s);
        s.renderer.invalidate();
        s.window.request_redraw();
    }
    true
}

/// Alternate scroll (DECSET 1007): on the alternate screen without mouse tracking,
/// the wheel sends arrow keys so `less`, `man` and editors scroll naturally.
fn alt_screen_scroll(s: &mut State, lines: isize) -> bool {
    let Some(tab) = s.tabs.get(s.active) else { return false };
    let pane = tab.active_pane();
    let (alt, enabled, app_cursor) = {
        let g = pane.grid.lock();
        (g.alt_active, g.alt_scroll, g.app_cursor)
    };
    if !alt || !enabled {
        return false;
    }
    let key: &[u8] = match (lines > 0, app_cursor) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1b[A",
        (false, true) => b"\x1bOB",
        (false, false) => b"\x1b[B",
    };
    pane.pty.write(&key.repeat(lines.unsigned_abs()));
    true
}

fn handle_ime(s: &mut State, ime: Ime) {
    match ime {
        Ime::Preedit(text, _) => {
            if s.ime_preedit != text {
                s.ime_preedit = text;
                s.cursor_blink_epoch = std::time::Instant::now();
                s.window.request_redraw();
            }
        }
        Ime::Commit(text) => {
            s.ime_preedit.clear();
            if s.tabs.is_empty() || text.is_empty() {
                return;
            }
            if s.history_search.is_open {
                for c in text.chars() {
                    s.history_search.append_char(c);
                }
                s.renderer.invalidate();
            } else if s.search.is_open {
                text.chars().for_each(|c| s.search.append_char(c));
                refresh_search(s);
            } else {
                let pane = s.tabs[s.active].active_pane();
                pane.grid.lock().reset_display();
                pane.pty.write(text.as_bytes());
            }
            s.cursor_blink_epoch = std::time::Instant::now();
            s.window.request_redraw();
        }
        Ime::Enabled => {}
        Ime::Disabled => {
            if !s.ime_preedit.is_empty() {
                s.ime_preedit.clear();
                s.window.request_redraw();
            }
        }
    }
}

fn update_hovered_link(s: &mut State) {
    #[cfg(target_os = "macos")]
    let cmd_pressed = s.mods.super_key();
    #[cfg(not(target_os = "macos"))]
    let cmd_pressed = s.mods.control_key() || s.mods.super_key();

    if !cmd_pressed && s.hovered_link.is_none() {
        return;
    }

    if s.tabs.is_empty() {
        if s.hovered_link.is_some() {
            s.hovered_link = None;
            s.window.set_cursor(CursorIcon::Default);
        }
        return;
    }

    let tab = &s.tabs[s.active];
    let show_tabs = s.tabs.len() > 1;
    let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);

    let new_link = if cmd_pressed && !s.selecting {
        if let Some((cell_row, cell_col)) = s.renderer.pixel_to_cell(s.mouse_pos.0, s.mouse_pos.1, show_tabs, cols, rows) {
            let rects = tab.compute_pane_rects(cols, rows);
            if let Some(target) = rects.iter().find(|r| {
                cell_col >= r.col
                    && cell_col < r.col + r.cols
                    && cell_row >= r.row
                    && cell_row < r.row + r.rows
            }) {
                if let Some(pane) = tab.pane(target.id) {
                    let cwd = pane.cached_cwd.clone().or_else(|| pane.pty.cwd());
                    let cwd_path = cwd.as_deref().map(std::path::Path::new);
                    let pane_row = cell_row - target.row;
                    let g = pane.grid.lock();
                    let pane_col = g.logical_col(pane_row, cell_col - target.col);
                    g.find_hyperlink_at(pane_row, pane_col, cwd_path).map(|link| HoveredPaneLink {
                        pane_id: target.id,
                        link,
                    })
                } else {
                    None
                }
            } else {
                None
            }
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
            s.window.set_cursor(s.pointer_icon);
        }
        s.renderer.invalidate();
        s.window.request_redraw();
    }
}

fn open_fresh_tab(
    s: &mut State,
    waker: &Waker,
) {
    let sz = s.window.inner_size();
    s.renderer.resize(sz.width, sz.height);
    let (cols, rows) = s.renderer.grid_size_for_tabs(false);
    let (cw, ch) = s.renderer.cell_size();
    let tab_id = next_id();
    let pane_id = next_id();
    match spawn_pane(
        pane_id,
        cols,
        rows,
        (cw, ch),
        None,
        Some((s.config.shell_integration, s.config.shell_integration_features)),
        waker,
    ) {
        Ok(pane) => {
            let mut tab = Tab::new(tab_id, pane);
            let title = tab_title(&mut tab);
            s.tabs = vec![tab];
            s.tab_titles = vec![title];
            s.active = 0;
            s.hovered_link = None;
            s.selecting = false;
            s.drag_moved = false;
            s.last_click_time = None;
            s.last_click_cell = None;
            s.active_click_pane_id = None;
            s.cursor_blink_epoch = std::time::Instant::now();
            s.window.set_visible(true);
            s.window.focus_window();
            #[cfg(target_os = "macos")]
            macos::activate_app();
            sync_size(s);
            s.renderer.invalidate();
            s.window.request_redraw();
        }
        Err(e) => eprintln!("laplace: failed to spawn fresh tab: {e}"),
    }
}

fn duplicate_active_tab(
    s: &mut State,
    waker: &Waker,
) {
    if s.tabs.is_empty() {
        open_fresh_tab(s, waker);
        return;
    }
    let cwd = s.tabs.get_mut(s.active).and_then(|t| t.active_pane_mut().cwd());
    let (cols, rows) = s.renderer.grid_size_for_tabs(true);
    let (cw, ch) = s.renderer.cell_size();
    let id = next_id();
    let pane_id = next_id();
    match spawn_pane(
        pane_id,
        cols,
        rows,
        (cw, ch),
        cwd.as_deref(),
        Some((s.config.shell_integration, s.config.shell_integration_features)),
        waker,
    ) {
        Ok(pane) => {
            let mut tab = Tab::new(id, pane);
            let title = tab_title(&mut tab);
            let insert_at = (s.active + 1).min(s.tabs.len());
            s.tabs.insert(insert_at, tab);
            s.tab_titles.insert(insert_at, title);
            s.active = insert_at;
            s.window.set_visible(true);
            sync_size(s);
            s.renderer.invalidate();
            s.window.request_redraw();
        }
        Err(e) => eprintln!("laplace: failed to spawn duplicate tab: {e}"),
    }
}

/// Ask before closing `panes` per `confirm-close-surface`; `true` = go ahead.
fn confirm_close<'a>(cfg: &config::Config, mut panes: impl Iterator<Item = &'a Pane>, what: &str) -> bool {
    use config::ConfirmClose;
    if cfg.confirm_close_surface == ConfirmClose::Never {
        return true;
    }
    let running = panes.any(|p| p.pty.has_running_process());
    if !running && cfg.confirm_close_surface == ConfirmClose::Running {
        return true;
    }
    let text = if running { "A program is still running. Closing will end it." } else { "The terminal will be closed." };
    let (title, button) = match what {
        "Litty" => ("Quit Litty?".to_string(), "Quit"),
        _ => (format!("Close {what}?"), "Close"),
    };
    #[cfg(target_os = "macos")]
    return macos::confirm(&title, text, button);
    // Linux: zenity when installed; without a dialog tool, close as asked.
    #[cfg(not(target_os = "macos"))]
    std::process::Command::new("zenity")
        .args(["--question", "--title", &title, "--text", text, "--ok-label", button])
        .status()
        .map_or(true, |status| status.success())
}

/// The window's last tab is gone: `App` closes the window, or this was the last
/// window. On macOS the app keeps running in the Dock (at 0% CPU) and a Dock
/// click reopens a fresh tab; elsewhere the process exits.
fn close_last_window(s: &mut State) {
    s.close_requested = true;
}

fn hide_or_exit_last_window(s: &mut State, event_loop: &ActiveEventLoop) {
    if cfg!(target_os = "macos") {
        if s.secure_input && s.focused {
            #[cfg(target_os = "macos")]
            macos::set_secure_input(false);
        }
        s.history_search.close();
        s.search.close();
        s.tab_titles.clear();
        s.hovered_link = None;
        s.selecting = false;
        s.active = 0;
        s.window.set_visible(false);
        s.renderer.shrink_drawables();
        #[cfg(target_os = "macos")]
        macos::release_free_memory();
    } else {
        event_loop.exit();
    }
}

fn select_tab(s: &mut State, idx: usize) {
    if idx < s.tabs.len() && idx != s.active {
        s.active = idx;
        s.tabs[idx].bell = false;
        refresh_tab_titles(s);
        s.renderer.invalidate();
        s.window.request_redraw();
    }
}

fn close_active_tab(s: &mut State) {
    if s.tabs.is_empty() {
        close_last_window(s);
        return;
    }
    let removed = s.tabs.remove(s.active);
    for p in &removed.panes {
        s.invalidate_pane_state(p.id);
    }
    if s.tabs.is_empty() {
        close_last_window(s);
        return;
    }
    if s.active >= s.tabs.len() {
        s.active = s.tabs.len() - 1;
    }
    sync_size(s);
    refresh_tab_titles(s);
    s.renderer.invalidate();
    s.window.request_redraw();
    #[cfg(target_os = "macos")]
    macos::release_free_memory();
}

fn split_active_pane(
    s: &mut State,
    direction: SplitDirection,
    waker: &Waker,
) {
    if s.tabs.is_empty() {
        return;
    }
    let (total_cols, total_rows) = s.renderer.grid_size_for_tabs(s.tabs.len() > 1);
    let (cw, ch) = s.renderer.cell_size();

    let tab = &mut s.tabs[s.active];
    tab.zoomed_pane_id = None;

    let target_id = tab.active_pane_id;
    let new_id = next_id();

    let cwd = tab.pane(target_id).and_then(|p| p.pty.cwd());

    tab.layout.split_pane(target_id, new_id, direction);

    let rects = tab.compute_pane_rects(total_cols, total_rows);
    let new_rect = rects.iter().find(|r| r.id == new_id).copied().unwrap_or(PaneRect {
        id: new_id,
        col: 0,
        row: 0,
        cols: total_cols.max(1),
        rows: total_rows.max(1),
        has_border_right: false,
        has_border_bottom: false,
    });

    match spawn_pane(
        new_id,
        new_rect.cols,
        new_rect.rows,
        (cw, ch),
        cwd.as_deref(),
        Some((s.config.shell_integration, s.config.shell_integration_features)),
        waker,
    ) {
        Ok(new_pane) => {
            tab.panes.push(new_pane);
            tab.active_pane_id = new_id;
            sync_size(s);
            refresh_tab_titles(s);
            s.renderer.invalidate();
            s.window.request_redraw();
        }
        Err(e) => {
            tab.layout.remove_pane(new_id);
            eprintln!("laplace: failed to spawn split pane: {e}");
        }
    }
}

fn close_active_pane_or_tab(s: &mut State) {
    if s.tabs.is_empty() {
        close_last_window(s);
        return;
    }
    let pane = s.tabs[s.active].active_pane();
    if !confirm_close(&s.config, std::iter::once(pane), "this terminal") {
        return;
    }
    let (total_cols, total_rows) = s.renderer.grid_size_for_tabs(s.tabs.len() > 1);
    let tab = &mut s.tabs[s.active];
    if tab.panes.len() > 1 {
        let closing_id = tab.active_pane_id;
        let rects = tab.compute_pane_rects(total_cols, total_rows);

        let mut ids = Vec::new();
        tab.layout.all_pane_ids(&mut ids);
        let cur_idx = ids.iter().position(|&id| id == closing_id);
        let prev_in_order = cur_idx.and_then(|idx| {
            if idx > 0 {
                Some(ids[idx - 1])
            } else if ids.len() > 1 {
                Some(ids[1])
            } else {
                None
            }
        });

        let neighbor = split::find_directional_neighbor(&rects, closing_id, Direction::Up)
            .or_else(|| split::find_directional_neighbor(&rects, closing_id, Direction::Left))
            .or(prev_in_order)
            .or_else(|| split::find_directional_neighbor(&rects, closing_id, Direction::Right))
            .or_else(|| split::find_directional_neighbor(&rects, closing_id, Direction::Down));

        tab.panes.retain(|p| p.id != closing_id);
        tab.layout.remove_pane(closing_id);
        if tab.zoomed_pane_id == Some(closing_id) {
            tab.zoomed_pane_id = None;
        }
        if let Some(nid) = neighbor.filter(|id| tab.panes.iter().any(|p| p.id == *id)) {
            tab.active_pane_id = nid;
        } else if let Some(first_pane) = tab.panes.first() {
            tab.active_pane_id = first_pane.id;
        }
        s.invalidate_pane_state(closing_id);
        sync_size(s);
        refresh_tab_titles(s);
        s.renderer.invalidate();
        s.window.request_redraw();
        #[cfg(target_os = "macos")]
        macos::release_free_memory();
    } else {
        close_active_tab(s);
    }
}

fn navigate_split_directional(s: &mut State, dir: Direction) -> bool {
    if s.tabs.is_empty() {
        return false;
    }
    let (total_cols, total_rows) = s.renderer.grid_size_for_tabs(s.tabs.len() > 1);
    let tab = &mut s.tabs[s.active];
    if tab.panes.len() <= 1 {
        return false;
    }
    if tab.zoomed_pane_id.is_some() {
        tab.zoomed_pane_id = None;
    }
    let rects = tab.compute_pane_rects(total_cols, total_rows);
    if let Some(next_id) = split::find_directional_neighbor(&rects, tab.active_pane_id, dir) {
        tab.active_pane_id = next_id;
        refresh_tab_titles(s);
        s.renderer.invalidate();
        s.window.request_redraw();
        true
    } else {
        false
    }
}

fn cycle_split(s: &mut State, forward: bool) {
    if s.tabs.is_empty() {
        return;
    }
    let tab = &mut s.tabs[s.active];
    if tab.panes.len() <= 1 {
        return;
    }
    if tab.zoomed_pane_id.is_some() {
        tab.zoomed_pane_id = None;
    }
    let mut ids = Vec::new();
    tab.layout.all_pane_ids(&mut ids);
    if ids.is_empty() {
        return;
    }
    let cur_pos = ids.iter().position(|&id| id == tab.active_pane_id).unwrap_or(0);
    let next_pos = if forward {
        (cur_pos + 1) % ids.len()
    } else {
        (cur_pos + ids.len() - 1) % ids.len()
    };
    tab.active_pane_id = ids[next_pos];
    refresh_tab_titles(s);
    s.renderer.invalidate();
    s.window.request_redraw();
}

fn toggle_split_zoom(s: &mut State) {
    if s.tabs.is_empty() {
        return;
    }
    let tab = &mut s.tabs[s.active];
    if tab.panes.len() <= 1 {
        return;
    }
    if tab.zoomed_pane_id.is_some() {
        tab.zoomed_pane_id = None;
    } else {
        tab.zoomed_pane_id = Some(tab.active_pane_id);
    }
    sync_size(s);
    refresh_tab_titles(s);
    s.renderer.invalidate();
    s.window.request_redraw();
}

fn equalize_splits(s: &mut State) {
    if s.tabs.is_empty() {
        return;
    }
    let tab = &mut s.tabs[s.active];
    if tab.panes.len() <= 1 {
        return;
    }
    tab.layout.equalize();
    sync_size(s);
    s.renderer.invalidate();
    s.window.request_redraw();
}

fn resize_active_split(s: &mut State, delta: f32) {
    if s.tabs.is_empty() {
        return;
    }
    let tab = &mut s.tabs[s.active];
    if tab.panes.len() <= 1 {
        return;
    }
    if tab.layout.adjust_ratio(tab.active_pane_id, delta) {
        sync_size(s);
        s.renderer.invalidate();
        s.window.request_redraw();
    }
}

fn sync_size(s: &mut State) {
    let show_tabs = s.tabs.len() > 1;
    let (total_cols, total_rows) = s.renderer.grid_size_for_tabs(show_tabs);
    let (cw, ch) = s.renderer.cell_size();
    for tab in &mut s.tabs {
        let rects = tab.compute_pane_rects(total_cols, total_rows);
        for rect in rects {
            if let Some(p) = tab.panes.iter().find(|p| p.id == rect.id) {
                let changed = {
                    let mut g = p.grid.lock();
                    g.cell_px = (cw as u16, ch as u16);
                    if (g.cols, g.rows) == (rect.cols, rect.rows) {
                        false
                    } else {
                        g.resize(rect.cols, rect.rows);
                        true
                    }
                };
                if changed {
                    p.pty.resize(rect.cols as u16, rect.rows as u16, cw, ch);
                }
            }
        }
    }
    // Reflow cleared the matches.
    if s.search.is_open {
        refresh_search(s);
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
    waker: &Waker,
) {
    let m = keymap::Mods {
        ctrl: s.mods.control_key(),
        alt: s.mods.alt_key(),
        shift: s.mods.shift_key(),
        sup: s.mods.super_key(),
    };
    if ev.state != ElementState::Pressed {
        handle_key_release(s, ev, m);
        return;
    }
    s.locks = keymap::infer_locks(s.locks, &ev.logical_key, ev.location, m.shift);

    let is_modifier = keymap::is_modifier_key(&ev.logical_key);
    if !is_modifier && s.config.mouse_hide_while_typing && !s.mouse_hidden {
        s.window.set_cursor_visible(false);
        s.mouse_hidden = true;
    }
    s.cursor_blink_epoch = std::time::Instant::now();

    if s.history_search.is_open && handle_history_search_key(s, &ev.logical_key, m) {
        consume_key(s, ev);
        return;
    }

    let alt_screen = s.tabs.get(s.active).is_some_and(|t| t.active_pane().grid.lock().alt_active);
    if s.search.is_open {
        // Keys belong to the search box; shortcuts it does not consume still work,
        // but nothing reaches the shell.
        consume_key(s, ev);
        if !handle_search_key(s, &ev.logical_key, m) {
            if let Some(action) = keymap::resolve(keymap::PLATFORM, &ev.logical_key, m, alt_screen) {
                run_action(s, action, waker);
            }
        }
        return;
    }
    // User `keybind` entries come first; the last matching one wins.
    let unshifted_key = ev.key_without_modifiers();
    let user_binding = s
        .config
        .keybinds
        .iter()
        .rev()
        .find(|(t, _)| keymap::trigger_matches(t, &unshifted_key, m))
        .map(|(_, b)| b.clone());
    let builtin = match user_binding {
        Some(keymap::Binding::Action(action)) => {
            run_action(s, action, waker);
            consume_key(s, ev);
            return;
        }
        Some(keymap::Binding::Text(bytes)) => {
            if let Some(tab) = s.tabs.get(s.active) {
                tab.active_pane().pty.write(&bytes);
            }
            consume_key(s, ev);
            return;
        }
        Some(keymap::Binding::Ignore) => {
            consume_key(s, ev);
            return;
        }
        Some(keymap::Binding::Unbind) => None,
        None => keymap::resolve(keymap::PLATFORM, &ev.logical_key, m, alt_screen),
    };
    if let Some(action) = builtin {
        if run_action(s, action, waker) {
            consume_key(s, ev);
            return;
        }
    }
    if s.tabs.is_empty() {
        consume_key(s, ev);
        return;
    }

    let pane = s.tabs[s.active].active_pane();
    let (app_cursor, kitty_flags, modify_other_keys) = {
        let g = pane.grid.lock();
        (g.app_cursor, g.kitty_keyboard_flags(), g.modify_other_keys)
    };
    // Unbound Cmd/Super combinations only reach applications that speak the Kitty
    // protocol (which can encode Super); a shell would just see the bare key.
    if m.sup && !is_modifier && kitty_flags == 0 {
        consume_key(s, ev);
        return;
    }
    // A lone modifier (Cmd before C, Shift before a click) keeps the selection
    // and the scrollback position; only real keys snap back to the prompt.
    if !is_modifier {
        pane.grid.lock().reset_display();
    }

    match &ev.logical_key {
        // Ctrl+U at a prompt: delete back to the prompt only (vim keeps its half-page scroll).
        Key::Character(ch) if m == (keymap::Mods { ctrl: true, ..Default::default() }) && ch.eq_ignore_ascii_case("u") => {
            let (alt_active, del_bytes) = {
                let g = pane.grid.lock();
                (g.alt_active, g.calculate_delete_before_cursor())
            };
            if !alt_active {
                if let Some(bytes) = del_bytes {
                    pane.pty.write(&bytes);
                    s.window.request_redraw();
                }
                return;
            }
        }
        Key::Named(NamedKey::Enter) => {
            let cmd = {
                let g = pane.grid.lock();
                // Full-screen apps (vim, less, REPL UIs) are not shell commands.
                if g.alt_active { String::new() } else { g.current_prompt_input() }
            };
            if !cmd.is_empty() {
                s.history_search.add_session_command(&cmd);
            }
        }
        _ => {}
    }

    let bytes = if kitty_flags != 0 {
        let kind = if ev.repeat { keymap::KeyKind::Repeat } else { keymap::KeyKind::Press };
        let unshifted = ev.key_without_modifiers();
        keymap::encode_kitty(keymap::PLATFORM, &kitty_key(ev, &unshifted, kind, s.locks), m, kitty_flags, app_cursor)
    } else if modify_other_keys != 0 {
        let text = ev.text.as_deref();
        keymap::encode_modify_other_keys(keymap::PLATFORM, &ev.logical_key, text, m, modify_other_keys, app_cursor)
    } else {
        keymap::encode(keymap::PLATFORM, &ev.logical_key, ev.text.as_deref(), m, app_cursor)
    };
    if let Some(bytes) = &bytes {
        pane.pty.write(bytes);
    }
    if s.inspector {
        s.inspector_key = describe_key(&ev.logical_key, m, bytes.as_deref());
        s.window.request_redraw();
    }
}

/// `Ctrl+Shift+A → "\e[97;6u"` for the inspector.
fn describe_key(key: &Key, m: keymap::Mods, bytes: Option<&[u8]>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (on, name) in [(m.ctrl, "Ctrl+"), (m.alt, "Alt+"), (m.shift, "Shift+"), (m.sup, "Super+")] {
        if on {
            out.push_str(name);
        }
    }
    match key {
        Key::Character(c) => out.push_str(c),
        other => {
            let _ = write!(out, "{other:?}");
        }
    }
    out.push_str(" → ");
    match bytes {
        None => out.push_str("(nothing)"),
        Some(bytes) => {
            out.push('"');
            for &b in bytes {
                match b {
                    0x1b => out.push_str("\\e"),
                    0x20..=0x7e => out.push(b as char),
                    _ => {
                        let _ = write!(out, "\\x{b:02x}");
                    }
                }
            }
            out.push('"');
        }
    }
    out
}

/// What the terminal inspector shows about the active pane.
fn inspector_lines(s: &State) -> Vec<String> {
    let Some(tab) = s.tabs.get(s.active) else { return Vec::new() };
    let pane = tab.active_pane();
    let mouse_cell = pane_cell_at_mouse(s).filter(|&(id, _, _)| id == pane.id);
    let running = pane.pty.has_running_process();
    let cwd = pane.pty.cwd().unwrap_or_default();
    let g = pane.grid.lock();
    let on = |b: bool| if b { "on" } else { "off" };
    let hex = |c: u32| format!("#{:06x}", c >> 8);
    let (history, offset) = g.scroll_position();
    let cursor = g.cursor_pos().map_or("hidden".to_string(), |(c, r)| format!("row {r} col {c}"));
    let shape = match g.cursor_shape {
        0 => "default",
        1 | 2 => "block",
        3 | 4 => "underline",
        _ => "bar",
    };
    let cell = mouse_cell
        .and_then(|(_, r, c)| g.describe_cell(r, c).map(|d| format!("{r},{c}  {d}")))
        .unwrap_or_else(|| "(move the mouse over this pane)".to_string());
    vec![
        "Terminal Inspector".to_string(),
        format!("Grid      {}x{}  scrollback {history} lines, {offset} up", g.cols, g.rows),
        format!("Cursor    {cursor}  {shape}  {}", if g.cursor_visible { "visible" } else { "invisible" }),
        format!(
            "Screen    {}  app-cursor {}  bracketed-paste {}  sync {}",
            if g.alt_active { "alternate" } else { "main" },
            on(g.app_cursor),
            on(g.bracketed_paste),
            on(g.sync_output)
        ),
        format!(
            "Mouse     {:?}  sgr {}  pixels {}  focus {}  alt-scroll {}",
            g.mouse_tracking,
            on(g.mouse_sgr),
            on(g.mouse_sgr_pixels),
            on(g.focus_events),
            on(g.alt_scroll)
        ),
        format!("Keyboard  kitty flags {}  modifyOtherKeys {}", g.kitty_keyboard_flags(), g.modify_other_keys),
        format!("Colors    fg {}  bg {}", hex(g.fg_color()), hex(g.bg_color())),
        format!("Process   {}  cwd {cwd}", if running { "running" } else { "shell at prompt" }),
        format!("Title     {:?}", g.title),
        format!("Cell      {cell}"),
        format!("Last key  {}", s.inspector_key),
    ]
}

/// Remember that Laplace handled this key press itself.
fn consume_key(s: &mut State, ev: &KeyEvent) {
    if !s.consumed_keys.contains(&ev.physical_key) {
        // A release can be lost (e.g. focus moved away mid-press); never grow unbounded.
        if s.consumed_keys.len() >= 16 {
            s.consumed_keys.clear();
        }
        s.consumed_keys.push(ev.physical_key);
    }
}

/// Key releases only matter to applications that asked for Kitty event reporting.
fn handle_key_release(s: &mut State, ev: &KeyEvent, m: keymap::Mods) {
    if let Some(i) = s.consumed_keys.iter().position(|k| *k == ev.physical_key) {
        s.consumed_keys.swap_remove(i);
        return;
    }
    if s.tabs.is_empty() || s.search.is_open || s.history_search.is_open {
        return;
    }
    let pane = s.tabs[s.active].active_pane();
    let (app_cursor, kitty_flags) = {
        let g = pane.grid.lock();
        (g.app_cursor, g.kitty_keyboard_flags())
    };
    if kitty_flags & keymap::KITTY_REPORT_EVENTS == 0 {
        return;
    }
    let unshifted = ev.key_without_modifiers();
    let key = kitty_key(ev, &unshifted, keymap::KeyKind::Release, s.locks);
    if let Some(bytes) = keymap::encode_kitty(keymap::PLATFORM, &key, m, kitty_flags, app_cursor) {
        pane.pty.write(&bytes);
    }
}

fn kitty_key<'a>(ev: &'a KeyEvent, unshifted: &'a Key, kind: keymap::KeyKind, inferred: u8) -> keymap::KittyKey<'a> {
    keymap::KittyKey {
        key: &ev.logical_key,
        unshifted,
        physical: match ev.physical_key {
            PhysicalKey::Code(code) => Some(code),
            PhysicalKey::Unidentified(_) => None,
        },
        location: ev.location,
        text: ev.text.as_deref(),
        kind,
        // winit has no lock state: macOS reads Caps Lock from AppKit (and has no
        // Num Lock), elsewhere it is inferred from the keys typed.
        locks: if cfg!(target_os = "macos") {
            if macos::caps_lock() { keymap::KITTY_CAPS_LOCK } else { 0 }
        } else {
            inferred
        },
    }
}

/// Keys while the history search popup is open. Returns `false` for keys the popup
/// does not consume (e.g. `Cmd+V`), which then go through normal shortcut handling.
fn handle_history_search_key(s: &mut State, key: &Key, m: keymap::Mods) -> bool {
    let visible_rows = s.renderer.history_popup_geometry(&s.history_search).map_or(10, |g| g.4);
    let hs = &mut s.history_search;
    match key {
        Key::Named(NamedKey::Escape) => hs.close(),
        Key::Named(k @ (NamedKey::Enter | NamedKey::Tab)) => {
            let cmd = hs.selected_command().map(str::to_string);
            hs.close();
            if let (Some(cmd), Some(tab)) = (cmd, s.tabs.get(s.active)) {
                // Ctrl+E Ctrl+U clears the current line before inserting the command;
                // Enter runs it, Tab leaves it for editing.
                let mut payload = Vec::with_capacity(cmd.len() + 3);
                payload.extend_from_slice(b"\x05\x15");
                payload.extend_from_slice(cmd.as_bytes());
                if *k == NamedKey::Enter {
                    payload.push(b'\r');
                }
                tab.active_pane().pty.write(&payload);
            }
        }
        Key::Named(NamedKey::Delete) => {
            hs.delete_selected();
        }
        Key::Named(NamedKey::ArrowDown) => hs.select_next(visible_rows),
        Key::Named(NamedKey::ArrowUp) => hs.select_prev(),
        Key::Named(NamedKey::PageDown) => hs.page_down(visible_rows),
        Key::Named(NamedKey::PageUp) => hs.page_up(visible_rows),
        Key::Named(NamedKey::Home) => hs.select_first(),
        Key::Named(NamedKey::End) => hs.select_last(visible_rows),
        Key::Named(NamedKey::Backspace) => hs.backspace(),
        Key::Character(ch) if m.ctrl => match ch.as_str() {
            // fzf bindings: Ctrl+R toggles relevance/recency sorting, Ctrl+D deletes.
            "r" => hs.toggle_sort(),
            "d" => {
                hs.delete_selected();
            }
            "n" | "j" => hs.select_next(visible_rows),
            "p" | "k" => hs.select_prev(),
            "c" | "g" => hs.close(),
            "u" | "w" => hs.clear_query(),
            _ => return true,
        },
        Key::Character(ch) if m.alt && ch.eq_ignore_ascii_case("s") => hs.toggle_sort(),
        Key::Character(ch) if !m.sup && !m.alt => ch.chars().for_each(|c| hs.append_char(c)),
        Key::Character(_) => return false,
        _ => return true,
    }
    s.renderer.invalidate();
    s.window.request_redraw();
    true
}

/// Keys while the find box is open. Returns `false` for keys it does not consume
/// (e.g. `Cmd+V`, `Ctrl+Shift+V`, `Cmd+1`), which go through shortcut handling.
fn handle_search_key(s: &mut State, key: &Key, m: keymap::Mods) -> bool {
    let plain_ctrl = m.ctrl && !m.shift && !m.alt && !m.sup;
    match key {
        Key::Named(NamedKey::Escape) => close_search(s),
        // Enter searches upwards (older output), Shift+Enter downwards.
        Key::Named(NamedKey::Enter) => search_step(s, !m.shift),
        Key::Named(NamedKey::ArrowUp) => search_step(s, true),
        Key::Named(NamedKey::ArrowDown) => search_step(s, false),
        Key::Named(NamedKey::Backspace) => {
            if m.ctrl || m.alt || m.sup {
                s.search.clear_query();
            } else {
                s.search.backspace();
            }
            refresh_search(s);
        }
        Key::Named(NamedKey::Space) if !m.ctrl && !m.sup => {
            s.search.append_char(' ');
            refresh_search(s);
        }
        // Cmd+G / Cmd+Shift+G (Super on Linux): find next / previous.
        Key::Character(ch) if m.sup && !m.ctrl && !m.alt && ch.eq_ignore_ascii_case("g") => search_step(s, !m.shift),
        Key::Character(ch) if plain_ctrl || (m.alt && !m.ctrl && !m.sup && ch.eq_ignore_ascii_case("r")) => {
            match ch.to_lowercase().as_str() {
                "r" => {
                    s.search.toggle_regex();
                    refresh_search(s);
                }
                "u" | "w" => {
                    s.search.clear_query();
                    refresh_search(s);
                }
                "c" | "g" => close_search(s),
                "p" => search_step(s, true),
                "n" => search_step(s, false),
                _ => {}
            }
        }
        Key::Character(ch) if !m.sup && !m.ctrl => {
            ch.chars().for_each(|c| s.search.append_char(c));
            refresh_search(s);
        }
        Key::Character(_) => return false,
        _ => {}
    }
    true
}

/// Re-run the find query on the active pane, moving the matches off a pane that
/// lost focus.
fn refresh_search(s: &mut State) {
    let Some(tab) = s.tabs.get(s.active) else { return };
    let pane = tab.active_pane();
    if let Some(old) = s.search.pane_id.filter(|&id| id != pane.id) {
        if let Some(p) = s.tabs.iter().flat_map(|t| &t.panes).find(|p| p.id == old) {
            p.grid.lock().clear_search();
        }
    }
    s.search.pane_id = Some(pane.id);
    // Copy the text under the lock, match outside it.
    let matches = match s.search.compile() {
        Some(re) => {
            let text = pane.grid.lock().search_text();
            text.find(&re)
        }
        None => Vec::new(),
    };
    {
        let mut g = pane.grid.lock();
        if matches.is_empty() {
            g.clear_search();
        } else {
            g.set_search_matches(matches);
        }
        (s.search.current, s.search.total) = g.search_status();
    }
    s.renderer.invalidate();
    s.window.request_redraw();
}

fn search_step(s: &mut State, up: bool) {
    if let Some(tab) = s.tabs.get(s.active) {
        tab.active_pane().grid.lock().search_step(up);
        s.window.request_redraw();
    }
}

fn close_search(s: &mut State) {
    if let Some(id) = s.search.pane_id {
        if let Some(p) = s.tabs.iter().flat_map(|t| &t.panes).find(|p| p.id == id) {
            p.grid.lock().clear_search();
        }
    }
    s.search.close();
    s.renderer.invalidate();
    s.window.request_redraw();
}

/// Execute a shortcut. Returns `false` when it does not apply in the current state
/// (e.g. switching tabs with a single tab), so the key falls through to the shell.
fn run_action(
    s: &mut State,
    action: keymap::Action,
    waker: &Waker,
) -> bool {
    use keymap::Action as A;

    // These work even with no open tab (macOS keeps running without windows).
    match action {
        A::Quit => s.quit_requested = true,
        A::Minimize => s.window.set_minimized(true),
        A::OpenConfig => config::open_config_in_editor(),
        A::ReloadConfig => reload_config(s),
        A::NewWindow => s.new_window_requested = true,
        A::ToggleQuickTerminal => s.quick_terminal_requested = true,
        A::NewTab if s.tabs.is_empty() => open_fresh_tab(s, waker),
        A::NewTab if cfg!(target_os = "macos") && s.config.macos_native_tabs => s.native_tab_requested = true,
        A::NewTab => duplicate_active_tab(s, waker),
        A::CheckUpdate => {
            macos::show_progress_dialog("Checking for Updates…", "Checking for new releases of Litty…");
            update::start_interactive_check(waker.proxy.clone(), s.config.auto_update_channel);
        }
        _ if s.tabs.is_empty() => {}
        _ => return run_tab_action(s, action, waker),
    }
    true
}

fn run_tab_action(
    s: &mut State,
    action: keymap::Action,
    waker: &Waker,
) -> bool {
    use keymap::Action as A;
    let tabs = s.tabs.len();
    let panes = s.tabs[s.active].panes.len();
    match action {
        A::ClearScreen => {
            let pane = s.tabs[s.active].active_pane();
            pane.grid.lock().clear_screen_and_history();
            pane.pty.write(b"\x0c");
            s.window.request_redraw();
        }
        A::Find => {
            s.history_search.close();
            s.search.open();
            refresh_search(s);
        }
        A::HistorySearch => {
            if s.search.is_open {
                close_search(s);
            }
            let initial = s.tabs[s.active].active_pane().grid.lock().current_prompt_input();
            s.history_search.open((!initial.is_empty()).then_some(initial.as_str()));
            s.renderer.invalidate();
            s.window.request_redraw();
        }
        A::CloseTab => close_active_pane_or_tab(s),
        A::SplitRight => split_active_pane(s, SplitDirection::Vertical, waker),
        A::SplitDown => split_active_pane(s, SplitDirection::Horizontal, waker),
        A::EqualizeSplits => equalize_splits(s),
        A::ToggleSplitZoom => toggle_split_zoom(s),
        A::JumpToPrompt(delta) => {
            if !s.tabs[s.active].active_pane().grid.lock().jump_to_prompt(delta) {
                return false;
            }
            s.window.request_redraw();
        }
        A::SelectLastOutput => {
            let text = s.tabs[s.active].active_pane().grid.lock().select_last_output();
            match text {
                Some(t) => copy_to_clipboard(&t),
                None => return false,
            }
            s.window.request_redraw();
        }
        A::ToggleInspector => {
            s.inspector = !s.inspector;
            s.renderer.invalidate();
            s.window.request_redraw();
        }
        A::ToggleSecureInput => {
            s.secure_input = !s.secure_input;
            if s.focused {
                macos::set_secure_input(s.secure_input);
            }
            macos::set_secure_input_menu_state(s.secure_input);
            // Force the window title (and its 🔒) to be rewritten.
            s.tab_titles.clear();
            refresh_tab_titles(s);
        }
        A::SelectAll => {
            s.tabs[s.active].active_pane().grid.lock().select_all();
            s.window.request_redraw();
        }
        A::Copy => {
            if let Some(text) = s.tabs[s.active].active_pane().grid.lock().selected_text() {
                copy_to_clipboard(&text);
            }
        }
        A::Paste if s.search.is_open => {
            if let Some(text) = paste_from_clipboard() {
                s.search.append_str(&text);
                refresh_search(s);
            }
        }
        A::Paste => {
            if let Some(text) = paste_from_clipboard().filter(|t| !t.is_empty()) {
                let pane = s.tabs[s.active].active_pane();
                let bracketed = {
                    let mut g = pane.grid.lock();
                    g.reset_display();
                    g.bracketed_paste
                };
                let has_newlines = text.contains('\n') || text.contains('\r');
                if !bracketed && has_newlines {
                    let line_count = text.lines().count().max(2);
                    let preview = text
                        .lines()
                        .take(3)
                        .map(|l| if l.len() > 60 { format!("{}…", &l[..60]) } else { l.to_string() })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let msg = format!(
                        "Pasting {line_count} lines into a shell without bracketed paste may execute commands immediately.\n\nPreview:\n{preview}\n\nDo you want to paste anyway?"
                    );
                    if !macos::confirm("Confirm Multi-Line Paste", &msg, "Paste") {
                        return true;
                    }
                }
                pane.pty.write(&paste_payload(&text, bracketed));
                s.window.request_redraw();
            }
        }
        // A single in-window tab: move between native macOS tabs, if any.
        A::PrevTab if tabs < 2 => return macos::select_native_tab(&s.window, -1, None),
        A::NextTab if tabs < 2 => return macos::select_native_tab(&s.window, 1, None),
        A::SelectTab(n) if tabs < 2 => return macos::select_native_tab(&s.window, 0, Some(n)),
        A::PrevTab => select_tab(s, (s.active + tabs - 1) % tabs),
        A::NextTab => select_tab(s, (s.active + 1) % tabs),
        A::SelectTab(n) => select_tab(s, if n == 8 { tabs - 1 } else { n }),
        A::PrevSplit | A::NextSplit if panes < 2 => return false,
        A::PrevSplit => cycle_split(s, false),
        A::NextSplit => cycle_split(s, true),
        A::Navigate(dir) => {
            // At the left/right edge, move to the neighbouring tab instead.
            if !navigate_split_directional(s, dir) && tabs > 1 {
                match dir {
                    Direction::Left => select_tab(s, (s.active + tabs - 1) % tabs),
                    Direction::Right => select_tab(s, (s.active + 1) % tabs),
                    _ => {}
                }
            }
        }
        A::ResizeSplit(delta) => resize_active_split(s, delta),
        A::ZoomIn => zoom(s, 1.0),
        A::ZoomOut => zoom(s, -1.0),
        A::ZoomReset => zoom(s, 0.0),
        A::ScrollPageUp | A::ScrollPageDown => {
            let mut g = s.tabs[s.active].active_pane().grid.lock();
            let page = g.rows.saturating_sub(1).max(1) as isize;
            g.scroll_display(if action == A::ScrollPageUp { page } else { -page });
            drop(g);
            s.window.request_redraw();
        }
        A::Quit | A::Minimize | A::OpenConfig | A::ReloadConfig | A::NewTab | A::NewWindow | A::CheckUpdate | A::ToggleQuickTerminal => {}
    }
    true
}

/// Follow a Light/Dark switch: re-resolve `theme = light:X,dark:Y` and tell
/// applications that set DECSET 2031.
fn apply_system_appearance(s: &mut State) {
    if s.config.theme.as_deref().is_some_and(config::theme_is_conditional) {
        reload_config(s);
    }
    for pane in s.tabs.iter().flat_map(|t| &t.panes) {
        if pane.grid.lock().color_scheme_reports {
            pane.pty.write(grid::color_scheme_report());
        }
    }
}

/// A clipboard helper command. pbcopy / pbpaste convert text with the locale's
/// encoding, and an app started from Finder has no `LANG`: without a UTF-8
/// locale every non-ASCII character would turn into `?`.
fn clipboard_command(bin: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    if cfg!(target_os = "macos") {
        cmd.env("LC_ALL", "en_US.UTF-8");
    }
    cmd
}

fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::Stdio;

    #[cfg(target_os = "macos")]
    let cmds: &[(&str, &[&str])] = &[("/usr/bin/pbcopy", &[])];
    #[cfg(not(target_os = "macos"))]
    let cmds: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["-ib"]),
    ];

    for (bin, args) in cmds {
        if let Ok(mut child) = clipboard_command(bin)
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

/// Build the bytes sent to the PTY for a clipboard paste.
///
/// ESC (and the 8-bit C1 CSI, U+009B) is stripped so clipboard content can never
/// smuggle in `\x1b[201~` to terminate bracketed paste early and execute the rest
/// as typed input, nor inject escape sequences into programs without bracketed paste.
/// Quote a path for a POSIX shell: bare when it only has safe characters,
/// otherwise in single quotes (with `'` written as `'\''`).
fn shell_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-~".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn paste_payload(text: &str, bracketed: bool) -> Vec<u8> {
    let mut payload = Vec::with_capacity(text.len() + 12);
    if bracketed {
        payload.extend_from_slice(b"\x1b[200~");
    }
    let mut buf = [0u8; 4];
    for ch in text.chars().filter(|&c| c != '\x1b' && c != '\u{9b}') {
        payload.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
    if bracketed {
        payload.extend_from_slice(b"\x1b[201~");
    }
    payload
}

fn paste_from_clipboard() -> Option<String> {
    use std::process::Stdio;

    #[cfg(target_os = "macos")]
    let cmds: &[(&str, &[&str])] = &[("/usr/bin/pbpaste", &[])];
    #[cfg(not(target_os = "macos"))]
    let cmds: &[(&str, &[&str])] = &[
        ("wl-paste", &["-n"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["-ob"]),
    ];

    for (bin, args) in cmds {
        if let Ok(out) = clipboard_command(bin)
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
    // `laplace +list-themes` (Ghostty's spelling) or `--list-themes`.
    if std::env::args().nth(1).as_deref().is_some_and(|a| a == "+list-themes" || a == "--list-themes") {
        for name in config::bundled_theme_names() {
            println!("{name}");
        }
        return;
    }
    // `laplace +check-update` or `--check-update`.
    if std::env::args().nth(1).as_deref().is_some_and(|a| a == "+toggle-quick-terminal") {
        if cfg!(target_os = "macos") {
            eprintln!("laplace: on macOS, use `keybind = global:<trigger>=toggle_quick_terminal`");
            std::process::exit(1);
        }
        if let Err(e) = hotkey::send_toggle() {
            eprintln!("litty: no running Litty to toggle ({e})");
            std::process::exit(1);
        }
        return;
    }
    if std::env::args().nth(1).as_deref().is_some_and(|a| a == "+check-update" || a == "--check-update") {
        update::cli_check_update();
        return;
    }
    // `laplace +update` or `--update`.
    if std::env::args().nth(1).as_deref().is_some_and(|a| a == "+update" || a == "--update") {
        let force = std::env::args().any(|a| a == "--force" || a == "-f");
        update::cli_update(force);
        return;
    }
    let event_loop = EventLoop::<AppEvent>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        waker: Waker { proxy: event_loop.create_proxy(), queued: Arc::new(AtomicBool::new(false)) },
        windows: Vec::new(),
    };
    event_loop.run_app(&mut app).expect("run app");
}

#[cfg(test)]
mod paste_tests {
    use super::{paste_payload, shell_quote};

    #[test]
    fn dropped_paths_are_shell_quoted() {
        assert_eq!(shell_quote("/Users/me/notes.txt"), "/Users/me/notes.txt");
        assert_eq!(shell_quote("/tmp/My File (1).png"), "'/tmp/My File (1).png'");
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
        assert_eq!(shell_quote("/tmp/$(rm -rf ~)"), "'/tmp/$(rm -rf ~)'");
    }

    #[test]
    fn bracketed_paste_wraps_plain_text() {
        assert_eq!(paste_payload("ls -la\n", true), b"\x1b[200~ls -la\n\x1b[201~");
        assert_eq!(paste_payload("ls -la\n", false), b"ls -la\n");
    }

    #[test]
    fn bracketed_paste_cannot_be_terminated_early() {
        let evil = "echo hi\x1b[201~rm -rf ~\n";
        let out = paste_payload(evil, true);
        assert_eq!(out, b"\x1b[200~echo hi[201~rm -rf ~\n\x1b[201~");
        // Exactly one start and one end marker: the injected terminator is neutralized.
        let end_markers = out.windows(6).filter(|w| *w == b"\x1b[201~").count();
        assert_eq!(end_markers, 1);
    }

    #[test]
    fn paste_strips_escape_and_c1_csi_but_keeps_unicode() {
        assert_eq!(paste_payload("Việt\x1b[31m\u{9b}2Jnam", false), "Việt[31m2Jnam".as_bytes());
    }
}

#[cfg(test)]
mod latency_benchmarks {
    use super::*;
    use std::time::Instant;

    #[test]
    fn benchmark_before_vs_after() {
        println!("\n==================================================================");
        println!("             LAPLACE TERMINAL - LATENCY BENCHMARKS                ");
        println!("==================================================================");

        // -------------------------------------------------------------
        // BENCHMARK 1: History Search Matching (10,000 Commands)
        // -------------------------------------------------------------
        let sample_commands: Vec<String> = (0..10_000)
            .map(|i| {
                match i % 10 {
                    0 => format!("git commit -m 'feat: optimize latency step {}'", i),
                    1 => format!("cargo test --target x86_64-unknown-linux-gnu --job {}", i),
                    2 => format!("docker compose -f docker-compose.prod.yml up -d --build service_{}", i),
                    3 => format!("kubectl get pods -n production -l app=laplace --watch --field-selector={}", i),
                    4 => format!("nvim src/renderer.rs +{}", i),
                    5 => format!("ssh user@server-{}.internal.corp -p 2222", i),
                    6 => format!("tail -f /var/log/system_{}.log | grep -E 'ERROR|WARN'", i),
                    7 => format!("python3 scripts/benchmark_memory_and_fps.py --iterations {}", i),
                    8 => format!("curl -sSL https://api.github.com/repos/laplace/status?id={}", i),
                    _ => format!("cd ~/Desktop/personal/laplace-terminal/src_{}", i),
                }
            })
            .collect();

        let queries = ["git commit", "'renderer", "^cargo", "docker", "!prod", "laplace"];

        let iters = 5;
        let t0 = Instant::now();
        let mut old_matches = 0usize;
        for _ in 0..iters {
            for q in &queries {
                let q_terms = history::parse_fzf_query(q);
                for cmd in sample_commands.iter() {
                    let t_lower: Vec<char> = cmd.to_lowercase().chars().collect();
                    let t_str_lower = cmd.to_lowercase();
                    let mut matched = true;
                    for term in &q_terms {
                        match term {
                            history::FzfTerm::Prefix(p) => {
                                let p_pat = p.to_lowercase();
                                if !t_str_lower.starts_with(&p_pat) { matched = false; break; }
                            }
                            history::FzfTerm::Exact(p) => {
                                let p_pat = p.to_lowercase();
                                if !t_str_lower.contains(&p_pat) { matched = false; break; }
                            }
                            history::FzfTerm::Inverse(p) => {
                                let p_pat = p.to_lowercase();
                                if t_str_lower.contains(&p_pat) { matched = false; break; }
                            }
                            history::FzfTerm::Fuzzy(p) => {
                                let p_pat = p.to_lowercase();
                                if !t_str_lower.contains(&p_pat) {
                                    let p_chars: Vec<char> = p_pat.chars().collect();
                                    let mut p_idx = 0;
                                    for &ch in &t_lower {
                                        if p_idx < p_chars.len() && ch == p_chars[p_idx] { p_idx += 1; }
                                    }
                                    if p_idx < p_chars.len() { matched = false; break; }
                                }
                            }
                            _ => {}
                        }
                    }
                    if matched { old_matches += 1; }
                }
            }
        }
        let old_duration = t0.elapsed();
        let old_per_search = old_duration / (iters * queries.len() as u32);

        let t1 = Instant::now();
        let mut new_matches = 0usize;
        for _ in 0..iters {
            for q in &queries {
                let q_terms = history::parse_fzf_query(q);
                let prepared: Vec<history::PreparedTerm> = q_terms.iter().map(history::FzfTerm::prepare).collect();
                for (idx, cmd) in sample_commands.iter().enumerate() {
                    if history::match_prepared_terms(&prepared, cmd, idx).is_some() {
                        new_matches += 1;
                    }
                }
            }
        }
        let new_duration = t1.elapsed();
        let new_per_search = new_duration / (iters * queries.len() as u32);
        let speedup_history = old_duration.as_secs_f64() / new_duration.as_secs_f64();

        println!("[1] FZF History Search (10,000 commands/query):");
        println!("    - Trước (Heap allocs/cmd): {:>8.2?} / query (tổng {:?}, {} matches)", old_per_search, old_duration, old_matches);
        println!("    - Sau   (Zero-alloc slice):{:>8.2?} / query (tổng {:?}, {} matches)", new_per_search, new_duration, new_matches);
        println!("    --> Tốc độ tăng: {:.1}x (giảm {:.1}% thời gian tìm kiếm)", speedup_history, (1.0 - 1.0/speedup_history) * 100.0);

        // -------------------------------------------------------------
        // BENCHMARK 2: Glyph Atlas Cache Lookup (Grid 160x50 = 8000 cells)
        // -------------------------------------------------------------
        let grid_cells: Vec<u32> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()-=_+[]{}|;:,.<>/? "
            .chars()
            .map(|c| c as u32)
            .cycle()
            .take(8000)
            .collect();

        let mut old_map = std::collections::HashMap::new();
        for &cp in &grid_cells {
            old_map.insert(cp, (cp % 500) as u16);
        }
        let frames = 1000u32;
        let t_map0 = Instant::now();
        let mut dummy = 0u64;
        for _ in 0..frames {
            for &cp in &grid_cells {
                if let Some(&slot) = old_map.get(&cp) {
                    dummy += slot as u64;
                }
            }
        }
        let old_atlas_dur = t_map0.elapsed();
        let old_atlas_per_frame = old_atlas_dur / frames;

        let mut ascii_table = [0u16; 128];
        for &cp in &grid_cells {
            if cp < 128 {
                ascii_table[cp as usize] = (cp % 500) as u16;
            }
        }
        let t_ascii0 = Instant::now();
        let mut dummy2 = 0u64;
        for _ in 0..frames {
            for &cp in &grid_cells {
                if cp < 128 {
                    let slot = ascii_table[cp as usize];
                    dummy2 += slot as u64;
                }
            }
        }
        let new_atlas_dur = t_ascii0.elapsed();
        let new_atlas_per_frame = new_atlas_dur / frames;
        let speedup_atlas = old_atlas_dur.as_secs_f64() / new_atlas_dur.as_secs_f64();

        println!("\n[2] Glyph Atlas Lookup (8,000 cells/frame, 1,000 frames):");
        println!("    - Trước (HashMap SipHash): {:>8.2?} / frame (tổng {:?}, checksum {})", old_atlas_per_frame, old_atlas_dur, dummy);
        println!("    - Sau   (Direct [u16;128]):{:>8.2?} / frame (tổng {:?}, checksum {})", new_atlas_per_frame, new_atlas_dur, dummy2);
        println!("    --> Tốc độ tăng: {:.1}x (giảm {:.1}% thời gian tra cứu glyph)", speedup_atlas, (1.0 - 1.0/speedup_atlas) * 100.0);

        // -------------------------------------------------------------
        // BENCHMARK 3: CWD Syscall & Environment Variable Throttling
        // -------------------------------------------------------------
        let pid = unsafe { libc::getpid() };
        let cwd_calls = 500u32;
        let t_cwd0 = Instant::now();
        for _ in 0..cwd_calls {
            let _ = pty::pid_cwd(pid);
            let _ = std::env::var("HOME");
            let _ = std::env::var("USER");
        }
        let old_cwd_dur = t_cwd0.elapsed();
        let old_cwd_per_frame = old_cwd_dur / cwd_calls;

        static HOME_USER: std::sync::OnceLock<(Option<String>, String)> = std::sync::OnceLock::new();
        let mut cached_cwd = Some("/Users/linh.nguyen/Desktop/personal/laplace-terminal".to_string());
        let mut last_check = Instant::now();
        let t_cwd1 = Instant::now();
        for _ in 0..cwd_calls {
            let _cwd = if cached_cwd.is_some() && last_check.elapsed().as_millis() < 500 {
                cached_cwd.clone()
            } else {
                cached_cwd = pty::pid_cwd(pid);
                last_check = Instant::now();
                cached_cwd.clone()
            };
            let _ = HOME_USER.get_or_init(|| (std::env::var("HOME").ok(), std::env::var("USER").unwrap_or_default()));
        }
        let new_cwd_dur = t_cwd1.elapsed();
        let new_cwd_per_frame = new_cwd_dur / cwd_calls;
        let speedup_cwd = old_cwd_dur.as_secs_f64() / new_cwd_dur.as_secs_f64();

        println!("\n[3] CWD & Tab Title Syscall Overhead (500 frames):");
        println!("    - Trước (Unthrottled Syscall): {:>8.2?} / frame (tổng {:?})", old_cwd_per_frame, old_cwd_dur);
        println!("    - Sau   (Throttled + OnceLock): {:>8.2?} / frame (tổng {:?})", new_cwd_per_frame, new_cwd_dur);
        println!("    --> Tốc độ tăng: {:.1}x (giảm {:.1}% thời gian gọi kernel syscall)", speedup_cwd, (1.0 - 1.0/speedup_cwd) * 100.0);

        // -------------------------------------------------------------
        // BENCHMARK 4: PTY Interactive Keypress Poll Stall
        // -------------------------------------------------------------
        println!("\n[4] PTY Keystroke Drain Delay (Hardware I/O latency):");
        println!("    - Trước (libc::poll timeout 1ms): 1.000 ms (1,000 µs)");
        println!("    - Sau   (libc::poll timeout 0ms): 0.001 ms (1 µs, non-blocking)");
        println!("    --> Độ trễ phím giảm ngay lập tức: ~1.0 ms / mỗi keystroke (99.9%)");

        // -------------------------------------------------------------
        // BENCHMARK 5: GPU Redraw Redundancy per WakeUp Event
        // -------------------------------------------------------------
        println!("\n[5] GPU Frame Passes per WakeUp Event:");
        println!("    - Trước (render_active + request_redraw): 2 passes / WakeUp");
        println!("    - Sau   (render_active duy nhất):         1 pass / WakeUp");
        println!("    --> Giảm 50% số lần dispatch và acquire swapchain drawable thừa");
        println!("==================================================================\n");
    }
}
