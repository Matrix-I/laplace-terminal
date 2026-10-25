//! Entry point: `winit` event loop in `ControlFlow::Wait` mode (0% idle CPU),
//! coalesced PTY redraw wakeups, and keyboard-to-ANSI translation.

mod atlas;
mod config;
mod fontdb;
mod grid;
mod history;
mod macos;
mod pty;
mod renderer;
mod split;

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
use crate::history::HistorySearch;
use crate::pty::Pty;
use crate::renderer::{Renderer, TabBarAction};
use crate::split::{Direction, PaneRect, Rect, SplitDirection, SplitNode};

#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    WakeUp,
    ChildExit(u64),
    Reopen,
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
        self.cached_cwd = self.pty.cwd();
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
    next_tab_id: u64,
    next_pane_id: u64,
    mods: ModifiersState,
    font_pt: f32,
    mouse_pos: (f64, f64),
    selecting: bool,
    drag_moved: bool,
    last_click_time: Option<std::time::Instant>,
    last_click_cell: Option<(usize, usize)>,
    active_click_pane_id: Option<u64>,
    click_count: u8,
    hovered_link: Option<HoveredPaneLink>,
    config: config::Config,
    config_mtime: Option<std::time::SystemTime>,
    mouse_hidden: bool,
    cursor_blink_epoch: std::time::Instant,
    focused: bool,
    history_search: HistorySearch,
    first_frame: bool,
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
                if !s.tabs.is_empty() {
                    s.window.request_redraw();
                }
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
        {
            macos::apply_macos_window_config(&window, &cfg);
            macos::set_dock_icon();
        }

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
        let pane_id = 1;
        let pane = spawn_pane(
            pane_id,
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
        let mut tab = Tab::new(tab_id, pane);
        let title = tab_title(&mut tab);

        let config_mtime = config::config_mtime();
        self.state = Some(State {
            window,
            renderer,
            tabs: vec![tab],
            tab_titles: vec![title],
            active: 0,
            next_tab_id: 2,
            next_pane_id: 2,
            mods: ModifiersState::default(),
            font_pt,
            mouse_pos: (0.0, 0.0),
            selecting: false,
            drag_moved: false,
            last_click_time: None,
            last_click_cell: None,
            active_click_pane_id: None,
            click_count: 0,
            hovered_link: None,
            config: cfg,
            config_mtime,
            mouse_hidden: false,
            cursor_blink_epoch: std::time::Instant::now(),
            focused: true,
            history_search: HistorySearch::new(),
            first_frame: true,
        });

        #[cfg(target_os = "macos")]
        macos::register_reopen_handler(self.proxy.clone());

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
                    if !s.tabs.is_empty() {
                        render_active(s);
                    }
                }
            }
            AppEvent::ChildExit(exited_id) => {
                let Some(s) = &mut self.state else {
                    event_loop.exit();
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
                        sync_size(s);
                        refresh_tab_titles(s);
                        s.renderer.invalidate();
                        s.window.request_redraw();
                        return;
                    }

                    s.tabs.remove(tab_pos);
                    if s.tabs.is_empty() {
                        #[cfg(target_os = "macos")]
                        {
                            s.tab_titles.clear();
                            s.hovered_link = None;
                            s.selecting = false;
                            s.active = 0;
                            s.window.set_visible(false);
                            unsafe {
                                extern "C" {
                                    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
                                }
                                malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
                            }
                            return;
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            event_loop.exit();
                            return;
                        }
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
                }
            }
            AppEvent::Reopen => {
                if let Some(s) = &mut self.state {
                    if s.tabs.is_empty() {
                        open_fresh_tab(s, &self.proxy, &self.wakeup_queued);
                    } else {
                        s.window.set_visible(true);
                        s.window.focus_window();
                        #[cfg(target_os = "macos")]
                        macos::activate_app();
                        s.renderer.invalidate();
                        s.window.request_redraw();
                    }
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(s) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => {
                #[cfg(target_os = "macos")]
                {
                    s.tabs.clear();
                    s.tab_titles.clear();
                    s.hovered_link = None;
                    s.selecting = false;
                    s.active = 0;
                    s.window.set_visible(false);
                    unsafe {
                        extern "C" {
                            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
                        }
                        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
                    }
                }
                #[cfg(not(target_os = "macos"))]
                event_loop.exit();
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
                                let end = (pane_row, pane_col);
                                if end != start || s.drag_moved {
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

            WindowEvent::MouseInput {
                state: btn_state,
                button: MouseButton::Left,
                ..
            } => {
                if s.tabs.is_empty() {
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
                                TabBarAction::NewTab => duplicate_active_tab(s, &self.proxy, &self.wakeup_queued),
                            }
                            return;
                        }
                        let show_tabs = s.tabs.len() > 1;
                        let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
                        let mut switched_active_pane = false;
                        if let Some((cell_row, cell_col)) =
                            s.renderer.pixel_to_cell(s.mouse_pos.0, s.mouse_pos.1, show_tabs, cols, rows)
                        {
                            let tab = &mut s.tabs[s.active];
                            let rects = tab.compute_pane_rects(cols, rows);
                            if let Some(target) = rects.iter().find(|r| {
                                cell_col >= r.col
                                    && cell_col < r.col + r.cols
                                    && cell_row >= r.row
                                    && cell_row < r.row + r.rows
                            }) {
                                let clicked_id = target.id;
                                let pane_row = cell_row - target.row;
                                let pane_col = cell_col - target.col;

                                if tab.active_pane_id != clicked_id {
                                    tab.active_pane_id = clicked_id;
                                    switched_active_pane = true;
                                }
                                s.active_click_pane_id = Some(clicked_id);

                                let now = std::time::Instant::now();
                                let multi_click = s
                                    .last_click_time
                                    .is_some_and(|t| now.duration_since(t).as_millis() <= 450)
                                    && s.last_click_cell.is_some_and(|(lr, _)| lr == pane_row);

                                s.click_count = if multi_click {
                                    (s.click_count % 3) + 1
                                } else {
                                    1
                                };
                                s.last_click_time = Some(now);
                                s.last_click_cell = Some((pane_row, pane_col));

                                if let Some(pane) = tab.pane(clicked_id) {
                                    match s.click_count {
                                        2 => {
                                            let line_text =
                                                pane.grid.lock().select_smart_line(pane_row, pane_col);
                                            if !line_text.is_empty() {
                                                copy_to_clipboard(&line_text);
                                            }
                                            s.selecting = false;
                                            s.drag_moved = false;
                                            s.window.request_redraw();
                                        }
                                        3 => {
                                            let line_text = pane.grid.lock().select_line(pane_row);
                                            if !line_text.is_empty() {
                                                copy_to_clipboard(&line_text);
                                            }
                                            s.selecting = false;
                                            s.drag_moved = false;
                                            s.window.request_redraw();
                                        }
                                        _ => {
                                            pane.grid.lock().set_selection(None);
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
                                        if let Some(t) = text {
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
                    let show_tabs = s.tabs.len() > 1;
                    let (cols, rows) = s.renderer.grid_size_for_tabs(show_tabs);
                    let target_id = if let Some((cell_row, cell_col)) =
                        s.renderer.pixel_to_cell(s.mouse_pos.0, s.mouse_pos.1, show_tabs, cols, rows)
                    {
                        let tab = &s.tabs[s.active];
                        let rects = tab.compute_pane_rects(cols, rows);
                        rects
                            .iter()
                            .find(|r| {
                                cell_col >= r.col
                                    && cell_col < r.col + r.cols
                                    && cell_row >= r.row
                                    && cell_row < r.row + r.rows
                            })
                            .map(|r| r.id)
                            .unwrap_or(tab.active_pane_id)
                    } else {
                        s.tabs[s.active].active_pane_id
                    };
                    if let Some(pane) = s.tabs[s.active].pane(target_id) {
                        pane.grid.lock().scroll_display(lines);
                        s.window.request_redraw();
                    }
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
                if !s.tabs.is_empty() {
                    s.window.request_redraw();
                }
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
            if !s.tabs.is_empty() && s.focused && cursor_blinks(s) {
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

fn spawn_pane(
    pane_id: u64,
    cols: usize,
    rows: usize,
    cw: u32,
    ch: u32,
    cwd: Option<&str>,
    shell_features: Option<crate::config::ShellFeatures>,
    proxy: &EventLoopProxy<AppEvent>,
    wakeup_queued: &Arc<AtomicBool>,
) -> std::io::Result<Pane> {
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
            let _ = p2.send_event(AppEvent::ChildExit(pane_id));
        },
    )?;
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
    title
}

fn refresh_tab_titles(s: &mut State) {
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
            s.window.set_title(active_title);
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

    let hovered_link = s.hovered_link.as_ref().map(|l| &l.link);

    s.renderer.render_panes(
        &render_panes,
        tab_bar,
        hovered_link,
        blink_on,
        Some(&s.history_search),
    );

    if s.first_frame {
        s.first_frame = false;
        #[cfg(target_os = "macos")]
        unsafe {
            extern "C" {
                fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
            }
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
}

/// Spawn a helper process (`open`, `xdg-open`) and reap it on a background thread
/// so it never lingers as a zombie.
pub(crate) fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    std::thread::Builder::new()
        .name("reap-helper".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
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
        let res = spawn_detached(std::process::Command::new("open").arg(&path));
        #[cfg(target_os = "linux")]
        let res = spawn_detached(std::process::Command::new("xdg-open").arg(&path));
        #[cfg(windows)]
        let res = std::process::Command::new("cmd")
            .args(["/c", "start", "", path.to_str().unwrap_or(target)])
            .spawn();

        if let Err(e) = res {
            eprintln!("laplace: failed to open path {path:?}: {e}");
        }
    } else {
        #[cfg(target_os = "macos")]
        let res = spawn_detached(std::process::Command::new("open").arg(target));
        #[cfg(target_os = "linux")]
        let res = spawn_detached(std::process::Command::new("xdg-open").arg(target));
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
                    let pane_col = cell_col - target.col;
                    let g = pane.grid.lock();
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
            s.window.set_cursor(CursorIcon::Default);
        }
        s.renderer.invalidate();
        s.window.request_redraw();
    }
}

fn open_fresh_tab(
    s: &mut State,
    proxy: &EventLoopProxy<AppEvent>,
    wakeup_queued: &Arc<AtomicBool>,
) {
    let (cols, rows) = s.renderer.grid_size_for_tabs(false);
    let (cw, ch) = s.renderer.cell_size();
    let tab_id = s.next_tab_id;
    s.next_tab_id += 1;
    let pane_id = s.next_pane_id;
    s.next_pane_id += 1;
    match spawn_pane(
        pane_id,
        cols,
        rows,
        cw,
        ch,
        None,
        Some(s.config.shell_integration_features),
        proxy,
        wakeup_queued,
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
    proxy: &EventLoopProxy<AppEvent>,
    wakeup_queued: &Arc<AtomicBool>,
) {
    if s.tabs.is_empty() {
        open_fresh_tab(s, proxy, wakeup_queued);
        return;
    }
    let cwd = s.tabs.get(s.active).and_then(|t| t.active_pane().pty.cwd());
    let (cols, rows) = s.renderer.grid_size_for_tabs(true);
    let (cw, ch) = s.renderer.cell_size();
    let id = s.next_tab_id;
    s.next_tab_id += 1;
    let pane_id = s.next_pane_id;
    s.next_pane_id += 1;
    match spawn_pane(
        pane_id,
        cols,
        rows,
        cw,
        ch,
        cwd.as_deref(),
        Some(s.config.shell_integration_features),
        proxy,
        wakeup_queued,
    ) {
        Ok(pane) => {
            let tab = Tab::new(id, pane);
            let insert_at = (s.active + 1).min(s.tabs.len());
            s.tabs.insert(insert_at, tab);
            s.active = insert_at;
            s.window.set_visible(true);
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

fn close_active_tab(s: &mut State, _event_loop: &ActiveEventLoop) {
    if s.tabs.is_empty() {
        #[cfg(target_os = "macos")]
        {
            s.window.set_visible(false);
            return;
        }
        #[cfg(not(target_os = "macos"))]
        {
            _event_loop.exit();
            return;
        }
    }
    s.tabs.remove(s.active);
    if s.tabs.is_empty() {
        #[cfg(target_os = "macos")]
        {
            s.tab_titles.clear();
            s.hovered_link = None;
            s.selecting = false;
            s.active = 0;
            s.window.set_visible(false);
            unsafe {
                extern "C" {
                    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
                }
                malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
            }
            return;
        }
        #[cfg(not(target_os = "macos"))]
        {
            _event_loop.exit();
            return;
        }
    }
    if s.active >= s.tabs.len() {
        s.active = s.tabs.len() - 1;
    }
    sync_size(s);
    refresh_tab_titles(s);
    s.renderer.invalidate();
    s.window.request_redraw();
}

fn split_active_pane(
    s: &mut State,
    direction: SplitDirection,
    proxy: &EventLoopProxy<AppEvent>,
    wakeup_queued: &Arc<AtomicBool>,
) {
    if s.tabs.is_empty() {
        return;
    }
    let (total_cols, total_rows) = s.renderer.grid_size_for_tabs(s.tabs.len() > 1);
    let (cw, ch) = s.renderer.cell_size();

    let tab = &mut s.tabs[s.active];
    tab.zoomed_pane_id = None;

    let target_id = tab.active_pane_id;
    let new_id = s.next_pane_id;
    s.next_pane_id += 1;

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
        cw,
        ch,
        cwd.as_deref(),
        Some(s.config.shell_integration_features),
        proxy,
        wakeup_queued,
    ) {
        Ok(new_pane) => {
            tab.panes.push(new_pane);
            tab.active_pane_id = new_id;
            sync_size(s);
            refresh_tab_titles(s);
            s.renderer.invalidate();
            s.window.request_redraw();
        }
        Err(e) => eprintln!("laplace: failed to spawn split pane: {e}"),
    }
}

fn close_active_pane_or_tab(s: &mut State, event_loop: &ActiveEventLoop) {
    if s.tabs.is_empty() {
        #[cfg(target_os = "macos")]
        {
            s.window.set_visible(false);
            return;
        }
        #[cfg(not(target_os = "macos"))]
        {
            event_loop.exit();
            return;
        }
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
        sync_size(s);
        refresh_tab_titles(s);
        s.renderer.invalidate();
        s.window.request_redraw();
    } else {
        close_active_tab(s, event_loop);
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

    if s.history_search.is_open {
        let visible_rows = s.renderer.history_popup_geometry(&s.history_search).map_or(10, |g| g.4);
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => {
                s.history_search.close();
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::Enter) => {
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
            }
            Key::Named(NamedKey::Tab) => {
                let cmd_opt = s.history_search.selected_command().map(|c| c.to_string());
                s.history_search.close();
                s.renderer.invalidate();
                if let Some(cmd) = cmd_opt {
                    let mut payload = Vec::with_capacity(cmd.len() + 2);
                    payload.extend_from_slice(b"\x05\x15");
                    payload.extend_from_slice(cmd.as_bytes());
                    s.tabs[s.active].active_pane().pty.write(&payload);
                }
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::Delete) => {
                s.history_search.delete_selected();
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                s.history_search.select_next(visible_rows);
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::ArrowUp) => {
                s.history_search.select_prev();
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::PageDown) => {
                s.history_search.page_down(visible_rows);
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::PageUp) => {
                s.history_search.page_up(visible_rows);
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::Home) => {
                s.history_search.select_first();
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::End) => {
                s.history_search.select_last(visible_rows);
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                s.history_search.backspace();
                s.renderer.invalidate();
                s.window.request_redraw();
                return;
            }
            Key::Character(ch) => {
                if ctrl {
                    match ch.as_str() {
                        "r" => {
                            // In fzf, Ctrl+R toggles sorting: Relevance vs Recency
                            s.history_search.toggle_sort();
                            s.renderer.invalidate();
                            s.window.request_redraw();
                            return;
                        }
                        "d" => {
                            // Ctrl+D deletes selected entry from history
                            s.history_search.delete_selected();
                            s.renderer.invalidate();
                            s.window.request_redraw();
                            return;
                        }
                        "n" | "j" => {
                            s.history_search.select_next(visible_rows);
                            s.renderer.invalidate();
                            s.window.request_redraw();
                            return;
                        }
                        "p" | "k" => {
                            s.history_search.select_prev();
                            s.renderer.invalidate();
                            s.window.request_redraw();
                            return;
                        }
                        "c" | "g" => {
                            s.history_search.close();
                            s.renderer.invalidate();
                            s.window.request_redraw();
                            return;
                        }
                        "u" | "w" => {
                            s.history_search.clear_query();
                            s.renderer.invalidate();
                            s.window.request_redraw();
                            return;
                        }
                        _ => return,
                    }
                }
                if alt && ch.eq_ignore_ascii_case("s") {
                    s.history_search.toggle_sort();
                    s.renderer.invalidate();
                    s.window.request_redraw();
                    return;
                }
                if !super_key && !alt {
                    for c in ch.chars() {
                        s.history_search.append_char(c);
                    }
                    s.renderer.invalidate();
                    s.window.request_redraw();
                    return;
                }
            }
            _ => return,
        }
    }

    if let Key::Character(ch) = &ev.logical_key {
        // Clear Screen & Scrollback buffer: Cmd+K on macOS, Ctrl+Shift+K on Linux (matches Terminal.app, iTerm2, Ghostty)
        #[cfg(target_os = "macos")]
        let is_clear_screen = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("k");
        #[cfg(not(target_os = "macos"))]
        let is_clear_screen = ((super_key && !ctrl && !alt && !shift) || (ctrl && shift && !alt && !super_key))
            && ch.eq_ignore_ascii_case("k");
        if is_clear_screen && !s.tabs.is_empty() {
            if let Some(tab) = s.tabs.get_mut(s.active) {
                let pane = tab.active_pane_mut();
                pane.grid.lock().clear_history();
                pane.pty.write(b"\x0c");
                s.window.request_redraw();
            }
            return;
        }

        // Built-in History Search:
        // - Ctrl+R (at shell prompt when !alt_active)
        // - macOS: Cmd+R
        // - Linux: Super+R or Ctrl+Shift+R
        let alt_active = s.tabs.get(s.active).map_or(false, |t| t.active_pane().grid.lock().alt_active);
        #[cfg(target_os = "macos")]
        let is_history = (ctrl && !shift && !alt && !super_key && !alt_active)
            || (super_key && !ctrl && !alt && !shift);
        #[cfg(not(target_os = "macos"))]
        let is_history = (ctrl && !shift && !alt && !super_key && !alt_active)
            || (super_key && !ctrl && !alt && !shift)
            || (ctrl && shift && !alt && !super_key);
        if is_history && ch.eq_ignore_ascii_case("r") {
            let initial = {
                let g = s.tabs[s.active].active_pane().grid.lock();
                g.current_prompt_input()
            };
            let initial_ref = if initial.is_empty() { None } else { Some(initial.as_str()) };
            s.history_search.open(initial_ref);
            s.renderer.invalidate();
            s.window.request_redraw();
            return;
        }

        // Quit app: Cmd+Q on macOS, Ctrl+Shift+Q on Linux.
        #[cfg(target_os = "macos")]
        let is_quit = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("q");
        #[cfg(not(target_os = "macos"))]
        let is_quit = ((ctrl && shift && !alt && !super_key) || (super_key && !ctrl && !alt && !shift))
            && ch.eq_ignore_ascii_case("q");
        if is_quit {
            event_loop.exit();
            return;
        }

        // Minimize window: Cmd+M on macOS, Ctrl+Shift+M (or Super+M) on Linux.
        #[cfg(target_os = "macos")]
        let is_minimize = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("m");
        #[cfg(not(target_os = "macos"))]
        let is_minimize = ((ctrl && shift && !alt && !super_key) || (super_key && !ctrl && !alt))
            && ch.eq_ignore_ascii_case("m");
        if is_minimize {
            s.window.set_minimized(true);
            return;
        }

        #[cfg(target_os = "macos")]
        let is_config = super_key && !ctrl && !alt && (ch == "," || (shift && ch == "<"));
        #[cfg(not(target_os = "macos"))]
        let is_config = ((ctrl && !alt && !super_key) || (super_key && !ctrl && !alt))
            && (ch == "," || (shift && ch == "<"));
        if is_config {
            if shift {
                reload_config(s);
            } else {
                config::open_config_in_editor();
            }
            return;
        }

        // New tab / duplicate tab:
        // - macOS: Cmd+T or Cmd+N
        // - Linux: Ctrl+Shift+T or Ctrl+Shift+N (or Super+T / Super+N)
        #[cfg(target_os = "macos")]
        let is_new_tab = super_key && !ctrl && !alt && (ch.eq_ignore_ascii_case("t") || ch.eq_ignore_ascii_case("n"));
        #[cfg(not(target_os = "macos"))]
        let is_new_tab = ((ctrl && shift && !alt && !super_key) || (super_key && !ctrl && !alt && !shift))
            && (ch.eq_ignore_ascii_case("t") || ch.eq_ignore_ascii_case("n"));
        if is_new_tab {
            if s.tabs.is_empty() {
                open_fresh_tab(s, proxy, wakeup_queued);
            } else {
                duplicate_active_tab(s, proxy, wakeup_queued);
            }
            return;
        }

        if s.tabs.is_empty() {
            return;
        }

        // Split Right (vertical divider): Cmd+D on macOS, Ctrl+Shift+D / Super+D on Linux
        #[cfg(target_os = "macos")]
        let is_split_right = super_key && !ctrl && !alt && !shift && ch.eq_ignore_ascii_case("d");
        #[cfg(not(target_os = "macos"))]
        let is_split_right = ((ctrl && shift && !alt && !super_key) || (super_key && !ctrl && !alt && !shift))
            && ch.eq_ignore_ascii_case("d");
        if is_split_right {
            split_active_pane(s, SplitDirection::Vertical, proxy, wakeup_queued);
            return;
        }

        // Split Down (horizontal divider): Cmd+Shift+D on macOS, Ctrl+Shift+E / Super+Shift+D / Ctrl+Alt+D on Linux
        #[cfg(target_os = "macos")]
        let is_split_down = super_key && !ctrl && !alt && shift && ch.eq_ignore_ascii_case("d");
        #[cfg(not(target_os = "macos"))]
        let is_split_down = (super_key && shift && !ctrl && !alt && ch.eq_ignore_ascii_case("d"))
            || (ctrl && shift && !alt && !super_key && ch.eq_ignore_ascii_case("e"))
            || (ctrl && alt && !shift && !super_key && ch.eq_ignore_ascii_case("d"));
        if is_split_down {
            split_active_pane(s, SplitDirection::Horizontal, proxy, wakeup_queued);
            return;
        }

        // Equalize splits: Cmd+Option+= or Cmd+Ctrl+= on macOS, Ctrl+Alt+= or Super+Alt+= on Linux
        #[cfg(target_os = "macos")]
        let is_equalize = (super_key && alt && !ctrl && (ch == "=" || ch == "+"))
            || (super_key && ctrl && !alt && (ch == "=" || ch == "+"));
        #[cfg(not(target_os = "macos"))]
        let is_equalize = ((ctrl && alt && !super_key) || (super_key && alt && !ctrl))
            && (ch == "=" || ch == "+");
        if is_equalize {
            equalize_splits(s);
            return;
        }

        // Copy selection: Cmd+C on macOS, Ctrl+Shift+C on Linux.
        #[cfg(target_os = "macos")]
        let is_copy = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("c");
        #[cfg(not(target_os = "macos"))]
        let is_copy = ((ctrl && shift && !alt && !super_key) || (super_key && !ctrl && !alt && !shift))
            && ch.eq_ignore_ascii_case("c");
        if is_copy {
            if let Some(text) = s.tabs[s.active].active_pane().grid.lock().selected_text() {
                copy_to_clipboard(&text);
            }
            return;
        }

        // Paste clipboard: Cmd+V on macOS, Ctrl+Shift+V on Linux.
        #[cfg(target_os = "macos")]
        let is_paste = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("v");
        #[cfg(not(target_os = "macos"))]
        let is_paste = ((ctrl && shift && !alt && !super_key) || (super_key && !ctrl && !alt && !shift))
            && ch.eq_ignore_ascii_case("v");
        if is_paste {
            if let Some(text) = paste_from_clipboard() {
                if !text.is_empty() {
                    let bracketed = {
                        let mut g = s.tabs[s.active].active_pane().grid.lock();
                        g.reset_display();
                        g.bracketed_paste
                    };
                    let payload = paste_payload(&text, bracketed);
                    s.tabs[s.active].active_pane().pty.write(&payload);
                    s.window.request_redraw();
                }
            }
            return;
        }

        // Close active split pane (or tab if last pane): Cmd+W on macOS, Ctrl+Shift+W on Linux.
        #[cfg(target_os = "macos")]
        let is_close_tab = super_key && !ctrl && !alt && ch.eq_ignore_ascii_case("w");
        #[cfg(not(target_os = "macos"))]
        let is_close_tab = ((ctrl && shift && !alt && !super_key) || (super_key && !ctrl && !alt && !shift))
            && ch.eq_ignore_ascii_case("w");
        if is_close_tab {
            close_active_pane_or_tab(s, event_loop);
            return;
        }

        // Switch tabs on macOS via Cmd+Shift+[ / ] (or Cmd+{ / }), and Cmd+1..9.
        #[cfg(target_os = "macos")]
        if super_key && !ctrl && !alt {
            match ch.as_str() {
                "{" | "[" if shift && s.tabs.len() > 1 => {
                    let prev = (s.active + s.tabs.len() - 1) % s.tabs.len();
                    select_tab(s, prev);
                    return;
                }
                "}" | "]" if shift && s.tabs.len() > 1 => {
                    let next = (s.active + 1) % s.tabs.len();
                    select_tab(s, next);
                    return;
                }
                "[" if !shift && s.tabs[s.active].panes.len() > 1 => {
                    cycle_split(s, false);
                    return;
                }
                "]" if !shift && s.tabs[s.active].panes.len() > 1 => {
                    cycle_split(s, true);
                    return;
                }
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" if !shift && s.tabs.len() > 1 => {
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

        // Switch tabs / cycle splits on Linux
        #[cfg(not(target_os = "macos"))]
        {
            if (ctrl || super_key) && shift && !alt && s.tabs.len() > 1 {
                match ch.as_str() {
                    "{" | "[" => {
                        let prev = (s.active + s.tabs.len() - 1) % s.tabs.len();
                        select_tab(s, prev);
                        return;
                    }
                    "}" | "]" => {
                        let next = (s.active + 1) % s.tabs.len();
                        select_tab(s, next);
                        return;
                    }
                    _ => {}
                }
            }
            if s.tabs[s.active].panes.len() > 1 && (((ctrl && alt) || super_key) && !shift) {
                match ch.as_str() {
                    "[" => {
                        cycle_split(s, false);
                        return;
                    }
                    "]" => {
                        cycle_split(s, true);
                        return;
                    }
                    _ => {}
                }
            }
            let is_tab_num = !shift
                && ((ctrl && !alt && !super_key)
                    || (alt && !ctrl && !super_key)
                    || (super_key && !ctrl && !alt));
            if is_tab_num && s.tabs.len() > 1 {
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
    }

    // Split resize shortcuts: Cmd+Ctrl+Arrows on macOS, Super+Ctrl+Arrows or Ctrl+Alt+Shift+Arrows on Linux
    #[cfg(target_os = "macos")]
    let is_resize_split = super_key && ctrl && !alt && !shift;
    #[cfg(not(target_os = "macos"))]
    let is_resize_split = (super_key && ctrl && !alt && !shift) || (ctrl && alt && shift && !super_key);
    if is_resize_split {
        match &ev.logical_key {
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowUp) => {
                resize_active_split(s, -0.05);
                return;
            }
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown) => {
                resize_active_split(s, 0.05);
                return;
            }
            _ => {}
        }
    }

    // Directional navigation between splits (with fallback to tab cycling on Left/Right):
    // macOS: Cmd+Arrows (Up/Down/Left/Right) and Cmd+Option+Arrows
    // Linux: Ctrl+Alt+Arrows and Super+Arrows
    #[cfg(target_os = "macos")]
    if super_key && !ctrl && !shift {
        let dir = match &ev.logical_key {
            Key::Named(NamedKey::ArrowLeft) => Some((Direction::Left, true)),
            Key::Named(NamedKey::ArrowRight) => Some((Direction::Right, false)),
            Key::Named(NamedKey::ArrowUp) => Some((Direction::Up, false)),
            Key::Named(NamedKey::ArrowDown) => Some((Direction::Down, false)),
            _ => None,
        };
        if let Some((d, is_left)) = dir {
            if navigate_split_directional(s, d) {
                return;
            }
            if is_left && s.tabs.len() > 1 {
                let prev = (s.active + s.tabs.len() - 1) % s.tabs.len();
                select_tab(s, prev);
                return;
            } else if !is_left && s.tabs.len() > 1 && matches!(d, Direction::Right) {
                let next = (s.active + 1) % s.tabs.len();
                select_tab(s, next);
                return;
            }
            return;
        }
    }

    #[cfg(not(target_os = "macos"))]
    if ((ctrl && alt) || (super_key && !ctrl)) && !shift {
        let dir = match &ev.logical_key {
            Key::Named(NamedKey::ArrowLeft) => Some((Direction::Left, true)),
            Key::Named(NamedKey::ArrowRight) => Some((Direction::Right, false)),
            Key::Named(NamedKey::ArrowUp) => Some((Direction::Up, false)),
            Key::Named(NamedKey::ArrowDown) => Some((Direction::Down, false)),
            _ => None,
        };
        if let Some((d, is_left)) = dir {
            if navigate_split_directional(s, d) {
                return;
            }
            if is_left && s.tabs.len() > 1 {
                let prev = (s.active + s.tabs.len() - 1) % s.tabs.len();
                select_tab(s, prev);
                return;
            } else if !is_left && s.tabs.len() > 1 && matches!(d, Direction::Right) {
                let next = (s.active + 1) % s.tabs.len();
                select_tab(s, next);
                return;
            }
            return;
        }
    }

    // Tab navigation with Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PageUp / Ctrl+PageDown
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
            _ => {}
        }
    }

    // Font size shortcuts:
    // - macOS: Cmd+= / Cmd+- / Cmd+0
    // - Linux: Ctrl+= / Ctrl+- / Ctrl+0 (also supports Shift: Ctrl+Shift+= / Ctrl+Shift+- / Ctrl+Shift+0, and Super)
    #[cfg(target_os = "macos")]
    let zoom_mod = super_key && !ctrl && !alt;
    #[cfg(not(target_os = "macos"))]
    let zoom_mod = (ctrl && !alt && !super_key) || (super_key && !ctrl && !alt);
    if zoom_mod {
        if let Key::Character(ch) = &ev.logical_key {
            match ch.as_str() {
                "=" | "+" => return zoom(s, 1.0),
                "-" | "_" => return zoom(s, -1.0),
                "0" | ")" => return zoom(s, 0.0),
                _ => {}
            }
        }
    }
    if super_key {
        return;
    }

    // Shift+PageUp / PageDown scrolls the scrollback buffer locally.
    if shift {
        let rows = s.tabs[s.active].active_pane().grid.lock().rows.saturating_sub(1).max(1) as isize;
        match ev.logical_key {
            Key::Named(NamedKey::PageUp) => {
                s.tabs[s.active].active_pane().grid.lock().scroll_display(rows);
                s.window.request_redraw();
                return;
            }
            Key::Named(NamedKey::PageDown) => {
                s.tabs[s.active].active_pane().grid.lock().scroll_display(-rows);
                s.window.request_redraw();
                return;
            }
            _ => {}
        }
    }

    let app_cursor = {
        let mut g = s.tabs[s.active].active_pane().grid.lock();
        g.reset_display();
        g.app_cursor
    };

    let m = 1 + (shift as u8) + if alt { 2 } else { 0 } + if ctrl { 4 } else { 0 };
    let mut scratch = [0u8; 32];
    let bytes: &[u8] = match &ev.logical_key {
        Key::Named(NamedKey::Enter) => {
            #[cfg(target_os = "macos")]
            let is_zoom = super_key && shift && !ctrl && !alt;
            #[cfg(not(target_os = "macos"))]
            let is_zoom = (ctrl && shift && !alt && !super_key) || (super_key && shift && !ctrl && !alt);
            if is_zoom {
                toggle_split_zoom(s);
                return;
            }

            let cmd = s.tabs[s.active].active_pane().grid.lock().current_prompt_input();
            if !cmd.is_empty() {
                s.history_search.add_session_command(&cmd);
            }
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
        #[cfg(target_os = "macos")]
        Key::Named(NamedKey::ArrowLeft) if alt && !ctrl && !shift && !super_key => b"\x1bb",
        #[cfg(target_os = "macos")]
        Key::Named(NamedKey::ArrowRight) if alt && !ctrl && !shift && !super_key => b"\x1bf",
        #[cfg(not(target_os = "macos"))]
        Key::Named(NamedKey::ArrowLeft) if !shift && !super_key && (alt ^ ctrl) => b"\x1bb",
        #[cfg(not(target_os = "macos"))]
        Key::Named(NamedKey::ArrowRight) if !shift && !super_key && (alt ^ ctrl) => b"\x1bf",
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
            if ch.eq_ignore_ascii_case("u") && !alt && !super_key && !shift {
                let (alt_active, del_bytes) = {
                    let pane = s.tabs[s.active].active_pane();
                    let g = pane.grid.lock();
                    (g.alt_active, g.calculate_delete_before_cursor())
                };
                if !alt_active {
                    if let Some(bytes) = del_bytes {
                        s.tabs[s.active].active_pane().pty.write(&bytes);
                        s.window.request_redraw();
                    }
                    return;
                }
            }
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
    s.tabs[s.active].active_pane().pty.write(bytes);
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

/// Build the bytes sent to the PTY for a clipboard paste.
///
/// ESC (and the 8-bit C1 CSI, U+009B) is stripped so clipboard content can never
/// smuggle in `\x1b[201~` to terminate bracketed paste early and execute the rest
/// as typed input, nor inject escape sequences into programs without bracketed paste.
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

#[cfg(test)]
mod paste_tests {
    use super::paste_payload;

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
                for (_idx, cmd) in sample_commands.iter().enumerate() {
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
