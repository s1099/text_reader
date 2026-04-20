use gpui::*;
use gpui_component::input::InputState;
use memmap2::Mmap;
use std::sync::{Arc, RwLock};

/// Hard cap on matches to keep rendering responsive on pathological queries
/// (e.g. searching for " " in a multi-GB log file).
pub const MAX_MATCHES: usize = 200_000;

/// A single match's position, sorted ascending by `(line, col)` alongside
/// other matches in [`FindState::matches`].
#[derive(Clone, Copy, Debug)]
pub struct Match {
    /// 0-based line number (same numbering as `SparseIndex`).
    pub line: u64,
    /// Byte offset within the line (from the first byte of the line).
    pub col: usize,
    /// Match length in bytes.
    pub len: usize,
}

/// Per-find-bar state. Lives on [`crate::TextEditor`] while the bar is open.
pub struct FindState {
    pub query_input: Entity<InputState>,
    pub case_sensitive: bool,
    pub matches: Arc<RwLock<Vec<Match>>>,
    /// Index into `matches` of the currently "active" highlight.
    pub current: usize,
    /// Tab this find is bound to; closed automatically if the tab changes/closes.
    pub tab_index: usize,
    /// Last query string that produced `matches`, used to avoid duplicate searches.
    pub last_query: String,
    /// Monotonic ID incremented on each search; background tasks check this before
    /// committing their results so stale runs don't overwrite newer ones.
    pub search_gen: u64,
    /// Set on render when the active match has just changed, so the content view
    /// can scroll it into view exactly once per navigation.
    pub pending_scroll: bool,
    pub _subscription: Option<Subscription>,
    pub _search_task: Option<Task<()>>,
}

impl FindState {
    pub fn match_count(&self) -> usize {
        self.matches.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn label(&self) -> String {
        let n = self.match_count();
        if n == 0 {
            "No results".to_string()
        } else {
            format!("{} of {}", self.current + 1, n)
        }
    }
}

/// Search `mmap` for all occurrences of `query`, returning `(line, col, len)`
/// triples sorted by position. Scanning is done with SIMD byte search; line
/// numbers are accumulated in a single forward pass over the newline-to-match
/// prefixes.
pub fn search_in_mmap(mmap: &Mmap, query: &str, case_insensitive: bool) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let data: &[u8] = mmap;
    let needle = query.as_bytes();
    if needle.len() > data.len() {
        return Vec::new();
    }

    let positions: Vec<usize> = if case_insensitive {
        find_all_ascii_ci(data, needle, MAX_MATCHES)
    } else {
        memchr::memmem::find_iter(data, needle)
            .take(MAX_MATCHES)
            .collect()
    };

    let mut out = Vec::with_capacity(positions.len());
    let mut line: u64 = 0;
    let mut line_start: usize = 0;
    let mut scan: usize = 0;
    for pos in positions {
        while scan < pos {
            match memchr::memchr(b'\n', &data[scan..pos]) {
                Some(off) => {
                    line += 1;
                    line_start = scan + off + 1;
                    scan = line_start;
                }
                None => {
                    scan = pos;
                }
            }
        }
        out.push(Match {
            line,
            col: pos - line_start,
            len: needle.len(),
        });
    }
    out
}

/// ASCII-only case-insensitive substring search.
///
/// Uses `memchr::memchr2` to jump between possible starts of the needle
/// (the first byte in either case), then verifies each window with
/// `eq_ignore_ascii_case`. Non-ASCII bytes in the needle/haystack compare
/// byte-exact, which matches the behavior callers document to users.
fn find_all_ascii_ci(haystack: &[u8], needle: &[u8], max_matches: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || haystack.len() < needle.len() {
        return out;
    }
    let first_lo = needle[0].to_ascii_lowercase();
    let first_up = needle[0].to_ascii_uppercase();
    let mut start = 0;
    while start + needle.len() <= haystack.len() && out.len() < max_matches {
        let rem = &haystack[start..];
        let found = if first_lo == first_up {
            memchr::memchr(first_lo, rem)
        } else {
            memchr::memchr2(first_lo, first_up, rem)
        };
        match found {
            Some(o) => {
                let pos = start + o;
                if pos + needle.len() > haystack.len() {
                    break;
                }
                let window = &haystack[pos..pos + needle.len()];
                if window
                    .iter()
                    .zip(needle)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
                {
                    out.push(pos);
                }
                start = pos + 1;
            }
            None => break,
        }
    }
    out
}

/// Find the slice of `matches` whose `line` falls in `[start_line, end_line)`.
/// Matches are sorted ascending by `(line, col)`, so this is two binary
/// searches.
pub fn slice_for_line_range(matches: &[Match], start_line: u64, end_line: u64) -> &[Match] {
    let lo = matches.partition_point(|m| m.line < start_line);
    let hi = matches.partition_point(|m| m.line < end_line);
    &matches[lo..hi]
}
