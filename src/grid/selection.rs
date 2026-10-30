//! Grid selection, hyperlink, and search hits representations.

/// A text selection. The public API uses visible `(row, col)` coordinates; the grid
/// stores rows as absolute line numbers so the selection stays on its text while
/// output scrolls and while the viewport moves through history.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub start: (usize, usize), // (row, col)
    pub end: (usize, usize),   // (row, col)
}

impl Selection {
    #[inline]
    pub fn ordered(self) -> ((usize, usize), (usize, usize)) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }
}

/// Search matches on one visible row (see `Grid::search_hits`).
pub struct SearchHits<'a> {
    pub line: usize,
    /// Index of the focused match within `hits`, if it is on this row.
    pub current: Option<usize>,
    pub hits: &'a [Selection],
}

impl<'a> SearchHits<'a> {
    #[allow(dead_code)]
    pub fn new(line: usize, current: Option<usize>, hits: &'a [Selection]) -> Self {
        SearchHits { line, current, hits }
    }

    #[inline]
    pub fn at(&self, col: usize) -> SearchHit {
        let pos = (self.line, col);
        match self.hits.iter().position(|m| m.start <= pos && pos <= m.end) {
            None => SearchHit::None,
            Some(i) if Some(i) == self.current => SearchHit::Current,
            Some(_) => SearchHit::Match,
        }
    }
}

/// How a cell is highlighted by find-in-terminal.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SearchHit {
    None,
    Match,
    Current,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
    pub is_path: bool,
}

/// Unresolved hyperlink candidate extracted while holding `grid.lock()`.
/// Implicit paths are validated on the filesystem (`resolve`) outside the lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HyperlinkCandidate {
    Resolved(Hyperlink),
    ImplicitPath {
        row: usize,
        start_col: usize,
        end_col: usize,
        token: String,
    },
}

impl HyperlinkCandidate {
    /// Validate and resolve this candidate into a [`Hyperlink`], performing any
    /// filesystem `Path::exists()` checks without holding `grid.lock()`.
    pub fn resolve(self, cwd: Option<&std::path::Path>) -> Option<Hyperlink> {
        match self {
            HyperlinkCandidate::Resolved(link) => Some(link),
            HyperlinkCandidate::ImplicitPath { row, start_col, end_col, token } => {
                let is_valid_path = if token.starts_with("~/") || token == "~" {
                    if let Ok(home) = std::env::var("HOME") {
                        let rest = token.strip_prefix("~/").unwrap_or("");
                        let full = std::path::Path::new(&home).join(rest);
                        full.exists() || !rest.is_empty()
                    } else {
                        true
                    }
                } else if token.starts_with('/') {
                    token.len() > 1 || std::path::Path::new(&token).exists()
                } else if let Some(base) = cwd {
                    base.join(&token).exists() || std::path::Path::new(&token).exists()
                } else {
                    std::path::Path::new(&token).exists()
                };

                if is_valid_path {
                    Some(Hyperlink {
                        row,
                        start_col,
                        end_col,
                        url: token,
                        is_path: true,
                    })
                } else {
                    None
                }
            }
        }
    }
}

