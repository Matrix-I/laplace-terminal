//! Tab and Split-Pane models, pane rect calculations and working directory tracking.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;

use crate::grid::{Grid, Hyperlink};
use crate::pty::Pty;
use crate::split::{PaneRect, Rect, SplitNode};

/// Tab and pane ids are unique across windows, so `ChildExit` finds its pane.
pub fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct Pane {
    pub id: u64,
    pub grid: Arc<Mutex<Grid>>,
    pub pty: Pty,
    pub cached_cwd: Option<String>,
    pub last_cwd_check: std::time::Instant,
    pub cached_running: bool,
    pub last_running_check: std::time::Instant,
}

impl Pane {
    pub fn new(id: u64, grid: Arc<Mutex<Grid>>, pty: Pty) -> Self {
        let past = std::time::Instant::now() - std::time::Duration::from_secs(10);
        Pane {
            id,
            grid,
            pty,
            cached_cwd: None,
            last_cwd_check: past,
            cached_running: false,
            last_running_check: past,
        }
    }

    pub fn cwd(&mut self) -> Option<String> {
        if self.last_cwd_check.elapsed().as_millis() < 500 {
            return self.cached_cwd.clone();
        }
        // Prefer the directory the shell reports via OSC 7; fall back to asking the OS.
        let reported = self.grid.lock().reported_cwd.clone();
        self.cached_cwd = reported.filter(|d| std::path::Path::new(d).is_dir()).or_else(|| self.pty.cwd());
        self.last_cwd_check = std::time::Instant::now();
        self.cached_cwd.clone()
    }

    pub fn has_running_process(&mut self) -> bool {
        if self.last_running_check.elapsed().as_millis() < 500 {
            return self.cached_running;
        }
        self.cached_running = self.pty.has_running_process();
        self.last_running_check = std::time::Instant::now();
        self.cached_running
    }
}

pub struct Tab {
    #[allow(dead_code)]
    pub id: u64,
    pub panes: Vec<Pane>,
    pub layout: SplitNode,
    pub active_pane_id: u64,
    pub zoomed_pane_id: Option<u64>,
    /// A pane rang the bell while this tab was not in front.
    pub bell: bool,
}

impl Tab {
    pub fn new(id: u64, pane: Pane) -> Self {
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

    pub fn active_pane(&self) -> Option<&Pane> {
        self.panes
            .iter()
            .find(|p| p.id == self.active_pane_id)
            .or_else(|| self.panes.first())
    }

    #[allow(dead_code)]
    pub fn active_pane_mut(&mut self) -> Option<&mut Pane> {
        let id = self.active_pane_id;
        if let Some(pos) = self.panes.iter().position(|p| p.id == id) {
            self.panes.get_mut(pos)
        } else {
            self.panes.first_mut()
        }
    }

    pub fn pane(&self, id: u64) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn pane_mut(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub fn compute_pane_rects(&self, total_cols: usize, total_rows: usize) -> Vec<PaneRect> {
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
pub struct HoveredPaneLink {
    pub pane_id: u64,
    pub link: Hyperlink,
}
