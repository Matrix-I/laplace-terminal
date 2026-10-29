//! Built-in CLI command history search with zero-latency FZF matching engine.
//!
//! Provides the exact same search power and syntax as `fzf --scheme=history`:
//! - Multi-word AND matching (terms separated by space)
//! - Exact substring match: `'pattern` or `"pattern"`
//! - Prefix exact match: `^pattern`
//! - Suffix exact match: `pattern$`
//! - Inverse / negation: `!pattern`, `!'exact`, `!^prefix`, `!suffix$`
//! - Fuzzy subsequence match: `pattern`
//! - Smart-case sensitivity (case-insensitive unless an uppercase char is used)
//! - Toggle sort modes: Relevance (best score) vs Recency (chronological)
//! - Real-time in-session command recording (searches commands run seconds ago)
//! - Deletion of history entries (`Shift+Delete` / `Ctrl+D`)
//!
//! Zero shell plugins required; works natively in any shell (zsh, bash, fish, sh).

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Maximum number of unique history commands to keep in memory.
const MAX_HISTORY_ITEMS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Relevance,
    Recency,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Relevance => "Relevance",
            SortMode::Recency => "Recent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FzfTerm {
    /// Exact substring match: `'pattern`
    Exact(String),
    /// Prefix match: `^pattern`
    Prefix(String),
    /// Suffix match: `pattern$`
    Suffix(String),
    /// Inverse match: `!pattern` (does not contain pattern)
    Inverse(String),
    /// Inverse prefix match: `!^pattern` (does not start with pattern)
    InversePrefix(String),
    /// Inverse suffix match: `!pattern$` (does not end with pattern)
    InverseSuffix(String),
    /// Fuzzy subsequence match: `pattern`
    Fuzzy(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    /// Index into `HistorySearch::items`.
    pub item_index: usize,
    /// Sorting score (higher = better match).
    pub score: i32,
    /// Character indices in the command string that matched positive query terms.
    pub match_indices: Vec<usize>,
}

#[derive(Debug)]
pub struct HistorySearch {
    pub is_open: bool,
    pub query: String,
    pub selected: usize,
    pub scroll_offset: usize,
    pub sort_mode: SortMode,
    pub items: Vec<String>,
    pub session_commands: Vec<String>,
    pub filtered: Vec<MatchResult>,
    file_mtimes: Vec<(PathBuf, Option<SystemTime>)>,
}

impl Default for HistorySearch {
    fn default() -> Self {
        Self::new()
    }
}

impl HistorySearch {
    pub fn new() -> Self {
        Self {
            is_open: false,
            query: String::new(),
            selected: 0,
            scroll_offset: 0,
            sort_mode: SortMode::Relevance,
            items: Vec::new(),
            session_commands: Vec::new(),
            filtered: Vec::new(),
            file_mtimes: Vec::new(),
        }
    }

    /// Open or reopen the history search popup.
    ///
    /// If `initial_query` is provided (e.g. from the current shell prompt buffer),
    /// the search query is pre-populated and filtered immediately, matching fzf's `--query`.
    pub fn open(&mut self, initial_query: Option<&str>) {
        self.reload_if_changed();
        self.is_open = true;
        self.query = initial_query.unwrap_or("").trim().to_string();
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    /// Close the history search popup.
    pub fn close(&mut self) {
        self.is_open = false;
        self.query.clear();
        self.filtered = Vec::new();
    }

    /// Toggle visibility of the history search popup.
    #[allow(dead_code)]
    pub fn toggle(&mut self) {
        if self.is_open {
            self.close();
        } else {
            self.open(None);
        }
    }

    /// Toggle sorting between Relevance (score) and Recency (chronological).
    pub fn toggle_sort(&mut self) {
        self.sort_mode = match self.sort_mode {
            SortMode::Relevance => SortMode::Recency,
            SortMode::Recency => SortMode::Relevance,
        };
        self.refilter();
    }

    /// Record a newly executed command from the active shell session.
    pub fn add_session_command(&mut self, cmd: &str) {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return;
        }
        // Remove prior duplicate in session commands if present
        self.session_commands.retain(|c| c != trimmed);
        self.session_commands.insert(0, trimmed.to_string());
        if self.session_commands.len() > 500 {
            self.session_commands.pop();
        }
        // Insert into items if not present or move to top
        if let Some(pos) = self.items.iter().position(|c| c == trimmed) {
            self.items.remove(pos);
        }
        self.items.insert(0, trimmed.to_string());
        if self.items.len() > MAX_HISTORY_ITEMS {
            self.items.pop();
        }
    }

    /// Delete the currently selected command from memory and disk history.
    pub fn delete_selected(&mut self) -> Option<String> {
        let item = self.filtered.get(self.selected)?;
        let item_idx = item.item_index;
        if item_idx >= self.items.len() {
            return None;
        }
        let removed_cmd = self.items.remove(item_idx);
        self.session_commands.retain(|c| c != &removed_cmd);

        // Remove from primary disk history file
        remove_from_disk_history(&removed_cmd);

        self.refilter();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        Some(removed_cmd)
    }

    /// Append a character to the search query and update results.
    pub fn append_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    /// Remove the last character of the search query.
    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.selected = 0;
            self.scroll_offset = 0;
            self.refilter();
        }
    }

    /// Clear the entire search query.
    pub fn clear_query(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
            self.selected = 0;
            self.scroll_offset = 0;
            self.refilter();
        }
    }

    /// Select the next matching result item.
    pub fn select_next(&mut self, visible_rows: usize) {
        if self.filtered.is_empty() {
            return;
        }
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
            if visible_rows > 0 && self.selected >= self.scroll_offset + visible_rows {
                self.scroll_offset = self.selected + 1 - visible_rows;
            }
        }
    }

    /// Select the previous matching result item.
    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    /// Jump forward by a page of visible results.
    pub fn page_down(&mut self, visible_rows: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let step = visible_rows.max(1);
        self.selected = (self.selected + step).min(self.filtered.len() - 1);
        if visible_rows > 0 && self.selected >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.selected + 1 - visible_rows;
        }
    }

    /// Jump backward by a page of visible results.
    pub fn page_up(&mut self, visible_rows: usize) {
        let step = visible_rows.max(1);
        self.selected = self.selected.saturating_sub(step);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
    }

    /// Jump to the first (top) result item.
    pub fn select_first(&mut self) {
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Jump to the last result item.
    pub fn select_last(&mut self, visible_rows: usize) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = self.filtered.len() - 1;
        if visible_rows > 0 && self.selected >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.selected + 1 - visible_rows;
        }
    }

    /// Return the currently selected command string, if any.
    pub fn selected_command(&self) -> Option<&str> {
        let item = self.filtered.get(self.selected)?;
        self.items.get(item.item_index).map(|s| s.as_str())
    }

    /// Total number of filtered matches for the current query.
    pub fn match_count(&self) -> usize {
        self.filtered.len()
    }

    /// Total number of history items loaded.
    pub fn total_items(&self) -> usize {
        self.items.len()
    }

    /// Reload history files if modification times on disk changed.
    pub fn reload_if_changed(&mut self) {
        let mut changed = false;
        for (path, last_mtime) in &self.file_mtimes {
            let cur_mtime = get_mtime(path);
            if cur_mtime != *last_mtime {
                changed = true;
                break;
            }
        }
        if changed || self.file_mtimes.is_empty() || self.items.is_empty() {
            self.reload();
        }
    }

    /// Force reload history from disk files.
    pub fn reload(&mut self) {
        let files = candidate_history_files();
        let mut new_mtimes = Vec::new();
        let mut raw_commands = Vec::new();

        // Include current in-session commands at the top
        raw_commands.extend(self.session_commands.clone());

        for file in &files {
            let mtime = get_mtime(file);
            new_mtimes.push((file.clone(), mtime));
            if file.exists() {
                parse_history_file(file, &mut raw_commands);
            }
        }

        self.file_mtimes = new_mtimes;

        // Deduplicate while preserving reverse-chronological order (newest first).
        let mut seen = HashSet::with_capacity(raw_commands.len());
        let mut unique_items = Vec::with_capacity(raw_commands.len().min(MAX_HISTORY_ITEMS));

        for cmd in raw_commands {
            if seen.insert(cmd.clone()) {
                unique_items.push(cmd);
                if unique_items.len() >= MAX_HISTORY_ITEMS {
                    break;
                }
            }
        }

        self.items = unique_items;
        self.refilter();
    }

    /// Recompute matching results against the current search query using FZF rules.
    pub fn refilter(&mut self) {
        let q = self.query.trim();
        if q.is_empty() {
            self.filtered = (0..self.items.len())
                .map(|i| MatchResult {
                    item_index: i,
                    score: 0,
                    match_indices: Vec::new(),
                })
                .collect();
            return;
        }

        let terms = parse_fzf_query(q);
        let prepared: Vec<PreparedTerm> = terms.iter().map(FzfTerm::prepare).collect();
        let mut matches = Vec::new();

        for (idx, cmd) in self.items.iter().enumerate() {
            if let Some(m) = match_prepared_terms(&prepared, cmd, idx) {
                matches.push(m);
            }
        }

        if self.sort_mode == SortMode::Relevance {
            matches.sort_by_key(|m| -m.score);
        } else {
            // Recency: original history order (newest first)
            matches.sort_by_key(|m| m.item_index);
        }

        self.filtered = matches;

        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
}

/// Retrieve file modification time if accessible.
fn get_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Candidate history file locations ordered by priority.
fn candidate_history_files() -> Vec<PathBuf> {
    let mut files = Vec::new();

    // 1. $HISTFILE if set and non-empty
    if let Ok(histfile) = std::env::var("HISTFILE") {
        if !histfile.trim().is_empty() {
            files.push(PathBuf::from(histfile));
        }
    }

    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let shell = std::env::var("SHELL").unwrap_or_default();

    if let Some(ref h) = home {
        // 2. Shell-specific primary history file
        if shell.ends_with("zsh") {
            files.push(h.join(".zsh_history"));
            files.push(h.join(".bash_history"));
        } else if shell.ends_with("fish") {
            files.push(h.join(".local/share/fish/fish_history"));
            files.push(h.join(".zsh_history"));
            files.push(h.join(".bash_history"));
        } else {
            files.push(h.join(".bash_history"));
            files.push(h.join(".zsh_history"));
        }

        // 3. XDG standard paths
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
            files.push(PathBuf::from(xdg_data).join("fish/fish_history"));
        } else {
            files.push(h.join(".local/share/fish/fish_history"));
        }
    }

    // Deduplicate candidate paths
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for f in files {
        if seen.insert(f.clone()) {
            unique.push(f);
        }
    }
    unique
}

/// One logical history entry: its exact on-disk byte range plus the decoded command.
#[derive(Debug)]
struct RawEntry {
    /// Byte range in the file, including the trailing newline(s).
    span: std::ops::Range<usize>,
    /// Decoded command, or `None` for blank lines and bash timestamp comments.
    command: Option<String>,
    /// Bash `HISTTIMEFORMAT` comment line (`#1729835432`) that belongs to the next entry.
    is_timestamp: bool,
}

fn is_fish_history(path: &Path) -> bool {
    path.to_string_lossy().contains("fish_history")
}

/// Zsh history files are "metafied": bytes `0x83..` are escaped as `0x83, byte ^ 0x20`.
fn is_zsh_history(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.contains("zsh") || name.contains("zhistory") {
        return true;
    }
    let shell_is_zsh = std::env::var("SHELL").is_ok_and(|s| s.ends_with("zsh"));
    shell_is_zsh && std::env::var_os("HISTFILE").is_some_and(|h| Path::new(&h) == path)
}

/// Reverse zsh's `metafy()`: `0x83, b` decodes to `b ^ 0x20`.
fn unmetafy(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter();
    while let Some(&b) = it.next() {
        if b == 0x83 {
            if let Some(&next) = it.next() {
                out.push(next ^ 0x20);
                continue;
            }
        }
        out.push(b);
    }
    out
}

fn decode_history_bytes(bytes: &[u8], zsh: bool) -> String {
    if zsh && bytes.contains(&0x83) {
        if let Ok(s) = String::from_utf8(unmetafy(bytes)) {
            return s;
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Split a bash / zsh history file into entries. A line ending in `\` continues
/// onto the next line (zsh multi-line commands); the backslash is dropped.
fn split_line_entries(bytes: &[u8], zsh: bool) -> Vec<RawEntry> {
    let mut entries = Vec::new();
    let mut start = 0;
    let mut pos = 0;
    let mut content: Vec<u8> = Vec::new();
    while pos < bytes.len() {
        let line_end = bytes[pos..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |i| pos + i);
        let next = (line_end + 1).min(bytes.len());
        let line = &bytes[pos..line_end];
        if !content.is_empty() {
            content.push(b'\n');
        }
        if let Some(stripped) = line.strip_suffix(b"\\") {
            if next < bytes.len() {
                content.extend_from_slice(stripped);
                pos = next;
                continue;
            }
        }
        content.extend_from_slice(line);
        entries.push(line_entry(start..next, &content, zsh));
        content.clear();
        start = next;
        pos = next;
    }
    entries
}

fn line_entry(span: std::ops::Range<usize>, content: &[u8], zsh: bool) -> RawEntry {
    let text = decode_history_bytes(content, zsh);
    let line = text.trim();
    if line.starts_with('#') && line.chars().skip(1).all(|c| c.is_ascii_digit()) {
        return RawEntry { span, command: None, is_timestamp: true };
    }
    // Zsh extended format: `: <timestamp>:<duration>;<command>`
    let cmd = match line.strip_prefix(": ").and_then(|_| line.find(';')) {
        Some(pos) => line[pos + 1..].trim(),
        None => line,
    };
    let command = (!cmd.is_empty()).then(|| cmd.to_string());
    RawEntry { span, command, is_timestamp: false }
}

/// Split a fish history file into `- cmd: <command>` blocks (each block runs until
/// the next `- cmd:` line and includes its `when:` / `paths:` lines).
fn split_fish_entries(bytes: &[u8]) -> Vec<RawEntry> {
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let line_end = bytes[pos..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |i| pos + i);
        let next = (line_end + 1).min(bytes.len());
        let line = String::from_utf8_lossy(&bytes[pos..line_end]);
        if let Some(rest) = line.trim().strip_prefix("- cmd:") {
            let cmd = rest.trim().replace("\\n", "\n").replace("\\\\", "\\");
            let command = (!cmd.is_empty()).then_some(cmd);
            entries.push(RawEntry { span: pos..next, command, is_timestamp: false });
        } else if let Some(last) = entries.last_mut() {
            last.span.end = next;
        } else {
            // Preamble before the first entry.
            entries.push(RawEntry { span: pos..next, command: None, is_timestamp: false });
        }
        pos = next;
    }
    entries
}

fn split_history_entries(path: &Path, bytes: &[u8]) -> Vec<RawEntry> {
    if is_fish_history(path) {
        split_fish_entries(bytes)
    } else {
        split_line_entries(bytes, is_zsh_history(path))
    }
}

/// Parse shell history file into commands (appends in reverse-chronological order).
fn parse_history_file(path: &Path, out: &mut Vec<String>) {
    let Ok(bytes) = std::fs::read(path) else { return };
    // File bottom has the most recent commands.
    out.extend(split_history_entries(path, &bytes).into_iter().rev().filter_map(|e| e.command));
}

/// Return `bytes` with every entry equal to `cmd` removed (along with a bash
/// timestamp comment directly preceding it), or `None` if nothing matched.
/// All other entries are kept byte-for-byte, so metafied zsh data is never re-encoded.
fn strip_command_entries(path: &Path, bytes: &[u8], cmd: &str) -> Option<Vec<u8>> {
    let mut kept: Vec<&RawEntry> = Vec::new();
    let mut removed = false;
    let entries = split_history_entries(path, bytes);
    for entry in &entries {
        if entry.command.as_deref() == Some(cmd) {
            removed = true;
            if kept.last().is_some_and(|e| e.is_timestamp) {
                kept.pop();
            }
            continue;
        }
        kept.push(entry);
    }
    if !removed {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    for entry in kept {
        out.extend_from_slice(&bytes[entry.span.clone()]);
    }
    Some(out)
}

/// Remove `cmd` from one history file: write the result to a private temp file in
/// the same directory and atomically `rename` it over the original. If the shell
/// appends to the file meanwhile, start over from the newer content.
fn remove_command_from_file(path: &Path, cmd: &str) -> std::io::Result<bool> {
    use std::os::unix::fs::OpenOptionsExt;

    // Rewrite the real file, not a symlink pointing at it (e.g. dotfiles repos).
    // The format is still detected from the configured path (`HISTFILE`, `.zsh_history`).
    let format_path = path;
    let path = std::fs::canonicalize(path)?;
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.litty-{}.tmp", std::process::id()));

    for _ in 0..3 {
        let before = std::fs::metadata(&path)?;
        let original = std::fs::read(&path)?;
        let Some(updated) = strip_command_entries(format_path, &original, cmd) else {
            return Ok(false);
        };

        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        let written = file
            .write_all(&updated)
            .and_then(|_| file.sync_all())
            .and_then(|_| std::fs::set_permissions(&tmp, before.permissions()));
        drop(file);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        let after = std::fs::metadata(&path)?;
        if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        return Ok(true);
    }
    Ok(false)
}

/// Remove a deleted command from every shell history file that contains it.
fn remove_from_disk_history(cmd_to_remove: &str) {
    for path in candidate_history_files() {
        if !path.exists() {
            continue;
        }
        if let Err(e) = remove_command_from_file(&path, cmd_to_remove) {
            eprintln!("litty: failed to update history file {path:?}: {e}");
        }
    }
}

/// Parse an FZF query string into individual match terms.
///
/// Supports:
/// - `'pattern` or `"pattern"`: exact substring match
/// - `^pattern`: prefix match
/// - `pattern$`: suffix match
/// - `!pattern`: inverse match (must not contain)
/// - `!^pattern`: inverse prefix
/// - `!pattern$`: inverse suffix
/// - `pattern`: fuzzy subsequence match
pub fn parse_fzf_query(query: &str) -> Vec<FzfTerm> {
    let mut terms = Vec::new();
    for raw in query.split_whitespace() {
        if raw.is_empty() {
            continue;
        }
        if let Some(rest) = raw.strip_prefix('!') {
            if rest.starts_with('\'') || rest.starts_with('"') {
                let pat = rest[1..].trim_end_matches('"');
                if !pat.is_empty() {
                    terms.push(FzfTerm::Inverse(pat.to_string()));
                }
            } else if let Some(pat) = rest.strip_prefix('^') {
                if !pat.is_empty() {
                    terms.push(FzfTerm::InversePrefix(pat.to_string()));
                }
            } else if rest.ends_with('$') && rest.len() > 1 {
                let pat = &rest[..rest.len() - 1];
                if !pat.is_empty() {
                    terms.push(FzfTerm::InverseSuffix(pat.to_string()));
                }
            } else if !rest.is_empty() {
                terms.push(FzfTerm::Inverse(rest.to_string()));
            }
        } else if raw.starts_with('\'') || (raw.starts_with('"') && raw.ends_with('"') && raw.len() > 1) {
            let pat = raw.trim_matches('\'').trim_matches('"');
            if !pat.is_empty() {
                terms.push(FzfTerm::Exact(pat.to_string()));
            }
        } else if let Some(pat) = raw.strip_prefix('^') {
            if !pat.is_empty() {
                terms.push(FzfTerm::Prefix(pat.to_string()));
            }
        } else if raw.ends_with('$') && raw.len() > 1 {
            let pat = &raw[..raw.len() - 1];
            if !pat.is_empty() {
                terms.push(FzfTerm::Suffix(pat.to_string()));
            }
        } else {
            terms.push(FzfTerm::Fuzzy(raw.to_string()));
        }
    }
    terms
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermKind {
    Exact,
    Prefix,
    Suffix,
    Inverse,
    InversePrefix,
    InverseSuffix,
    Fuzzy,
}

#[derive(Debug, Clone)]
pub struct PreparedTerm {
    pub kind: TermKind,
    pub pattern: String,
    pub pattern_lower: String,
    pub pattern_chars: Vec<char>,
    pub pattern_bytes: Vec<u8>,
    pub pattern_bytes_lower: Vec<u8>,
    pub is_smart_case: bool,
    pub is_ascii: bool,
}

impl FzfTerm {
    pub fn prepare(&self) -> PreparedTerm {
        let (kind, p) = match self {
            FzfTerm::Exact(s) => (TermKind::Exact, s),
            FzfTerm::Prefix(s) => (TermKind::Prefix, s),
            FzfTerm::Suffix(s) => (TermKind::Suffix, s),
            FzfTerm::Inverse(s) => (TermKind::Inverse, s),
            FzfTerm::InversePrefix(s) => (TermKind::InversePrefix, s),
            FzfTerm::InverseSuffix(s) => (TermKind::InverseSuffix, s),
            FzfTerm::Fuzzy(s) => (TermKind::Fuzzy, s),
        };
        let is_smart_case = p.chars().any(|c| c.is_uppercase());
        let pattern_lower = p.to_lowercase();
        let pattern_chars: Vec<char> = if is_smart_case {
            p.chars().collect()
        } else {
            pattern_lower.chars().collect()
        };
        let is_ascii = p.is_ascii();
        let pattern_bytes = p.as_bytes().to_vec();
        let pattern_bytes_lower = pattern_lower.as_bytes().to_vec();

        PreparedTerm {
            kind,
            pattern: p.clone(),
            pattern_lower,
            pattern_chars,
            pattern_bytes,
            pattern_bytes_lower,
            is_smart_case,
            is_ascii,
        }
    }
}

/// Match command text against pre-compiled FZF terms with smart-case and scoring.
pub fn match_prepared_terms(terms: &[PreparedTerm], text: &str, recency_rank: usize) -> Option<MatchResult> {
    if terms.is_empty() {
        return Some(MatchResult {
            item_index: recency_rank,
            score: 0,
            match_indices: Vec::new(),
        });
    }

    let is_text_ascii = text.is_ascii();
    let all_terms_ascii = terms.iter().all(|t| t.is_ascii);

    if is_text_ascii && all_terms_ascii {
        match_ascii_terms(terms, text, recency_rank)
    } else {
        match_unicode_terms(terms, text, recency_rank)
    }
}

/// Fast-path zero-allocation matcher for ASCII shell commands and search queries.
fn match_ascii_terms(terms: &[PreparedTerm], text: &str, recency_rank: usize) -> Option<MatchResult> {
    let t_bytes = text.as_bytes();
    let t_len = t_bytes.len();

    let mut scratch = [0u8; 256];
    let lower_storage: Vec<u8>;
    let t_lower: &[u8] = if t_len <= scratch.len() {
        for (dst, &b) in scratch.iter_mut().zip(t_bytes) {
            *dst = b.to_ascii_lowercase();
        }
        &scratch[..t_len]
    } else {
        lower_storage = t_bytes.iter().map(|&b| b.to_ascii_lowercase()).collect();
        &lower_storage
    };

    let mut total_score = 0i32;
    let mut all_match_indices = Vec::new();

    for term in terms {
        let p_pat = if term.is_smart_case {
            &term.pattern_bytes
        } else {
            &term.pattern_bytes_lower
        };
        let target = if term.is_smart_case { t_bytes } else { t_lower };
        let p_len = p_pat.len();

        match term.kind {
            TermKind::Inverse => {
                if target.windows(p_len).any(|w| w == p_pat.as_slice()) {
                    return None;
                }
            }
            TermKind::InversePrefix => {
                if target.starts_with(p_pat) {
                    return None;
                }
            }
            TermKind::InverseSuffix => {
                if target.ends_with(p_pat) {
                    return None;
                }
            }
            TermKind::Prefix => {
                if target.starts_with(p_pat) {
                    all_match_indices.extend(0..p_len);
                    total_score += 3000;
                } else {
                    return None;
                }
            }
            TermKind::Suffix => {
                if target.ends_with(p_pat) {
                    let start = t_len.saturating_sub(p_len);
                    all_match_indices.extend(start..t_len);
                    total_score += 2500;
                } else {
                    return None;
                }
            }
            TermKind::Exact => {
                let byte_pos = target.windows(p_len).position(|w| w == p_pat.as_slice())?;
                all_match_indices.extend(byte_pos..byte_pos + p_len);
                total_score += 2000 - (byte_pos as i32 * 3);
                if byte_pos == 0 {
                    total_score += 500;
                } else {
                    let prev = t_bytes[byte_pos - 1];
                    if prev == b' ' || prev == b'-' || prev == b'_' || prev == b'/' || prev == b'.' {
                        total_score += 300;
                    }
                }
            }
            TermKind::Fuzzy => {
                if let Some(byte_pos) = target.windows(p_len).position(|w| w == p_pat.as_slice()) {
                    all_match_indices.extend(byte_pos..byte_pos + p_len);
                    total_score += 1500 - (byte_pos as i32 * 2);
                    if byte_pos == 0 {
                        total_score += 400;
                    } else {
                        let prev = t_bytes[byte_pos - 1];
                        if prev == b' ' || prev == b'-' || prev == b'_' || prev == b'/' || prev == b'.' {
                            total_score += 250;
                        }
                    }
                } else {
                    let mut p_idx = 0;
                    let mut last_match: Option<usize> = None;
                    let mut term_score = 600;
                    let start_len = all_match_indices.len();

                    for (t_idx, &b) in target.iter().enumerate() {
                        if p_idx < p_len && b == p_pat[p_idx] {
                            all_match_indices.push(t_idx);
                            if let Some(last) = last_match {
                                if last + 1 == t_idx {
                                    term_score += 50;
                                }
                            }
                            if t_idx == 0 {
                                term_score += 100;
                            } else {
                                let prev = t_bytes[t_idx - 1];
                                if prev == b' ' || prev == b'-' || prev == b'_' || prev == b'/' || prev == b'.' {
                                    term_score += 80;
                                }
                            }
                            last_match = Some(t_idx);
                            p_idx += 1;
                        }
                    }
                    if p_idx == p_len {
                        total_score += term_score;
                    } else {
                        all_match_indices.truncate(start_len);
                        return None;
                    }
                }
            }
        }
    }

    all_match_indices.sort_unstable();
    all_match_indices.dedup();

    total_score -= (recency_rank as i32).min(500);

    Some(MatchResult {
        item_index: recency_rank,
        score: total_score,
        match_indices: all_match_indices,
    })
}

/// Unicode fallback matcher for multilingual commands and search queries.
fn match_unicode_terms(terms: &[PreparedTerm], text: &str, recency_rank: usize) -> Option<MatchResult> {
    let t_lower: Vec<char> = text.to_lowercase().chars().collect();
    let t_str_lower = text.to_lowercase();
    let t_chars: Vec<char> = text.chars().collect();

    let mut total_score = 0i32;
    let mut all_match_indices = Vec::new();

    for term in terms {
        let is_smart_case = term.is_smart_case;
        let p_pat = if is_smart_case { &term.pattern } else { &term.pattern_lower };
        let target = if is_smart_case { text } else { &t_str_lower };
        let p_chars = &term.pattern_chars;

        match term.kind {
            TermKind::Inverse => {
                if target.contains(p_pat.as_str()) {
                    return None;
                }
            }
            TermKind::InversePrefix => {
                if target.starts_with(p_pat.as_str()) {
                    return None;
                }
            }
            TermKind::InverseSuffix => {
                if target.ends_with(p_pat.as_str()) {
                    return None;
                }
            }
            TermKind::Prefix => {
                if target.starts_with(p_pat.as_str()) {
                    let p_len = p_chars.len();
                    all_match_indices.extend(0..p_len);
                    total_score += 3000;
                } else {
                    return None;
                }
            }
            TermKind::Suffix => {
                if target.ends_with(p_pat.as_str()) {
                    let p_len = p_chars.len();
                    let start = t_chars.len().saturating_sub(p_len);
                    all_match_indices.extend(start..t_chars.len());
                    total_score += 2500;
                } else {
                    return None;
                }
            }
            TermKind::Exact => {
                let byte_pos = target.find(p_pat.as_str())?;
                let char_pos = target[..byte_pos].chars().count();
                let p_len = p_chars.len();
                all_match_indices.extend(char_pos..char_pos + p_len);
                total_score += 2000 - (char_pos as i32 * 3);
                if char_pos == 0 {
                    total_score += 500;
                } else {
                    let prev = t_chars[char_pos - 1];
                    if prev == ' ' || prev == '-' || prev == '_' || prev == '/' || prev == '.' {
                        total_score += 300;
                    }
                }
            }
            TermKind::Fuzzy => {
                if let Some(byte_pos) = target.find(p_pat.as_str()) {
                    let char_pos = target[..byte_pos].chars().count();
                    let p_len = p_chars.len();
                    all_match_indices.extend(char_pos..char_pos + p_len);
                    total_score += 1500 - (char_pos as i32 * 2);
                    if char_pos == 0 {
                        total_score += 400;
                    } else {
                        let prev = t_chars[char_pos - 1];
                        if prev == ' ' || prev == '-' || prev == '_' || prev == '/' || prev == '.' {
                            total_score += 250;
                        }
                    }
                } else {
                    let mut p_idx = 0;
                    let mut last_match: Option<usize> = None;
                    let mut term_score = 600;
                    let search_target: &[char] = if is_smart_case { &t_chars } else { &t_lower };
                    let start_len = all_match_indices.len();

                    for (t_idx, &tch) in search_target.iter().enumerate() {
                        if p_idx < p_chars.len() && tch == p_chars[p_idx] {
                            all_match_indices.push(t_idx);
                            if let Some(last) = last_match {
                                if last + 1 == t_idx {
                                    term_score += 50;
                                }
                            }
                            if t_idx == 0 {
                                term_score += 100;
                            } else {
                                let prev = t_chars[t_idx - 1];
                                if prev == ' ' || prev == '-' || prev == '_' || prev == '/' || prev == '.' {
                                    term_score += 80;
                                }
                            }
                            last_match = Some(t_idx);
                            p_idx += 1;
                        }
                    }
                    if p_idx == p_chars.len() {
                        total_score += term_score;
                    } else {
                        all_match_indices.truncate(start_len);
                        return None;
                    }
                }
            }
        }
    }

    all_match_indices.sort_unstable();
    all_match_indices.dedup();

    total_score -= (recency_rank as i32).min(500);

    Some(MatchResult {
        item_index: recency_rank,
        score: total_score,
        match_indices: all_match_indices,
    })
}

/// Match command text against parsed FZF terms with smart-case and scoring.
#[allow(dead_code)]
pub fn match_fzf_terms(terms: &[FzfTerm], text: &str, recency_rank: usize) -> Option<MatchResult> {
    if terms.is_empty() {
        return Some(MatchResult {
            item_index: recency_rank,
            score: 0,
            match_indices: Vec::new(),
        });
    }
    let prepared: Vec<PreparedTerm> = terms.iter().map(FzfTerm::prepare).collect();
    match_prepared_terms(&prepared, text, recency_rank)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fzf_query_tokens() {
        let terms = parse_fzf_query("^git 'status !commit feat$");
        assert_eq!(
            terms,
            vec![
                FzfTerm::Prefix("git".into()),
                FzfTerm::Exact("status".into()),
                FzfTerm::Inverse("commit".into()),
                FzfTerm::Suffix("feat".into()),
            ]
        );
    }

    #[test]
    fn fzf_prefix_and_suffix_match() {
        let terms = parse_fzf_query("^git main$");
        let res = match_fzf_terms(&terms, "git push origin main", 0).expect("should match prefix & suffix");
        assert!(res.match_indices.contains(&0));
        assert!(res.match_indices.contains(&19));
    }

    #[test]
    fn fzf_inverse_match() {
        let terms = parse_fzf_query("git !commit");
        assert!(match_fzf_terms(&terms, "git status", 0).is_some());
        assert!(match_fzf_terms(&terms, "git commit -m initial", 0).is_none());
    }

    #[test]
    fn fzf_exact_match() {
        let terms = parse_fzf_query("'test");
        let res = match_fzf_terms(&terms, "cargo test --all", 0).expect("should match exact");
        assert_eq!(res.match_indices, vec![6, 7, 8, 9]);
    }

    #[test]
    fn fzf_subsequence_fuzzy_match() {
        let terms = parse_fzf_query("gcm");
        let res = match_fzf_terms(&terms, "git commit -m initial", 0).expect("should match subsequence");
        assert_eq!(res.match_indices, vec![0, 4, 6]);
    }

    #[test]
    fn fzf_smart_case_matching() {
        let terms_lower = parse_fzf_query("laplace");
        assert!(match_fzf_terms(&terms_lower, "Laplace Terminal", 0).is_some());

        let terms_upper = parse_fzf_query("Laplace");
        assert!(match_fzf_terms(&terms_upper, "Laplace Terminal", 0).is_some());
        assert!(match_fzf_terms(&terms_upper, "laplace terminal", 0).is_none());
    }

    #[test]
    fn session_command_recording() {
        let mut search = HistorySearch {
            is_open: true,
            query: String::new(),
            selected: 0,
            scroll_offset: 0,
            sort_mode: SortMode::Relevance,
            items: vec!["older 1".into(), "older 2".into()],
            session_commands: Vec::new(),
            filtered: Vec::new(),
            file_mtimes: Vec::new(),
        };
        search.add_session_command("just_now_command");
        assert_eq!(search.items[0], "just_now_command");
        assert_eq!(search.session_commands[0], "just_now_command");
    }

    #[test]
    fn delete_selected_entry() {
        let mut search = HistorySearch {
            is_open: true,
            query: String::new(),
            selected: 0,
            scroll_offset: 0,
            sort_mode: SortMode::Relevance,
            items: vec!["secret_token".into(), "ls -la".into()],
            session_commands: Vec::new(),
            filtered: Vec::new(),
            file_mtimes: Vec::new(),
        };
        search.refilter();
        assert_eq!(search.selected_command(), Some("secret_token"));
        let deleted = search.delete_selected();
        assert_eq!(deleted, Some("secret_token".into()));
        assert_eq!(search.match_count(), 1);
        assert_eq!(search.selected_command(), Some("ls -la"));
    }

    #[test]
    fn history_search_navigation() {
        let mut search = HistorySearch {
            is_open: true,
            query: String::new(),
            selected: 0,
            scroll_offset: 0,
            sort_mode: SortMode::Relevance,
            items: vec!["cmd 1".into(), "cmd 2".into(), "cmd 3".into(), "cmd 4".into()],
            session_commands: Vec::new(),
            filtered: Vec::new(),
            file_mtimes: Vec::new(),
        };
        search.refilter();
        assert_eq!(search.match_count(), 4);
        assert_eq!(search.selected, 0);

        search.select_next(2);
        assert_eq!(search.selected, 1);
        search.select_next(2);
        assert_eq!(search.selected, 2);
        assert_eq!(search.scroll_offset, 1);

        search.select_prev();
        assert_eq!(search.selected, 1);
        assert_eq!(search.scroll_offset, 1);

        search.select_prev();
        assert_eq!(search.selected, 0);
        assert_eq!(search.scroll_offset, 0);
    }

    #[test]
    fn parse_zsh_history_format() {
        let content = b": 1729835400:0;echo first\n: 1729835405:0;git commit -m \\\n  \"multiline commit\"\n: 1729835410:0;ls -la\n";
        let temp = std::env::temp_dir().join("laplace_test_zsh_hist");
        std::fs::write(&temp, content).expect("write temp hist");
        let mut out = Vec::new();
        parse_history_file(&temp, &mut out);
        let _ = std::fs::remove_file(&temp);

        assert_eq!(out.len(), 3);
        assert_eq!(out[0], "ls -la");
        assert_eq!(out[1], "git commit -m \n  \"multiline commit\"");
        assert_eq!(out[2], "echo first");
    }

    #[test]
    fn parse_bash_history_format() {
        let content = b"#1729835400\ncargo check\n#1729835405\ngit status\n";
        let temp = std::env::temp_dir().join("laplace_test_bash_hist");
        std::fs::write(&temp, content).expect("write temp hist");
        let mut out = Vec::new();
        parse_history_file(&temp, &mut out);
        let _ = std::fs::remove_file(&temp);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "git status");
        assert_eq!(out[1], "cargo check");
    }

    fn temp_history(name: &str, content: &[u8]) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("laplace_hist_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write temp hist");
        path
    }

    #[test]
    fn zsh_metafied_entries_decode_and_survive_removal() {
        // "ăn" = C4 83 6E, stored metafied by zsh as C4 83 A3 6E.
        let content = b": 1:0;echo \xC4\x83\xA3n\n: 2:0;git status\n: 3:0;ls\n";
        let path = temp_history(".zsh_history", content);

        let mut out = Vec::new();
        parse_history_file(&path, &mut out);
        assert_eq!(out, vec!["ls", "git status", "echo ăn"]);

        assert!(remove_command_from_file(&path, "git status").expect("remove"));
        assert_eq!(std::fs::read(&path).unwrap(), b": 1:0;echo \xC4\x83\xA3n\n: 3:0;ls\n");

        assert!(remove_command_from_file(&path, "echo ăn").expect("remove unicode"));
        assert_eq!(std::fs::read(&path).unwrap(), b": 3:0;ls\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn removal_drops_whole_multiline_entry() {
        let content = b": 1:0;git commit -m \\\n  \"multi\"\n: 2:0;ls\n";
        let path = temp_history(".zsh_history", content);
        assert!(remove_command_from_file(&path, "git commit -m \n  \"multi\"").expect("remove"));
        assert_eq!(std::fs::read(&path).unwrap(), b": 2:0;ls\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn removal_drops_bash_timestamp_and_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_history(".bash_history", b"#100\ncargo check\n#200\ngit status\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(remove_command_from_file(&path, "cargo check").expect("remove"));
        assert_eq!(std::fs::read(&path).unwrap(), b"#200\ngit status\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!remove_command_from_file(&path, "not there").expect("no-op"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn removal_drops_fish_block() {
        let content = b"- cmd: ls\n  when: 1\n- cmd: echo a\\nb\n  when: 2\n  paths:\n    - b\n";
        let path = temp_history("fish_history", content);
        let mut out = Vec::new();
        parse_history_file(&path, &mut out);
        assert_eq!(out, vec!["echo a\nb", "ls"]);
        assert!(remove_command_from_file(&path, "echo a\nb").expect("remove"));
        assert_eq!(std::fs::read(&path).unwrap(), b"- cmd: ls\n  when: 1\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_if_changed_with_session_command() {
        let mut search = HistorySearch::new();
        search.add_session_command("prompt_command_before_open");
        assert_eq!(search.items.len(), 1);
        assert!(search.file_mtimes.is_empty());

        search.reload_if_changed();
        // file_mtimes is now populated
        assert!(!search.file_mtimes.is_empty());
        // prompt command is still preserved at the top
        assert_eq!(search.items[0], "prompt_command_before_open");
    }

    #[test]
    fn add_session_command_caps_items() {
        let mut search = HistorySearch::new();
        for i in 0..(MAX_HISTORY_ITEMS + 50) {
            search.add_session_command(&format!("cmd_{i}"));
        }
        assert_eq!(search.items.len(), MAX_HISTORY_ITEMS);
        assert_eq!(search.items[0], format!("cmd_{}", MAX_HISTORY_ITEMS + 49));
    }
}
