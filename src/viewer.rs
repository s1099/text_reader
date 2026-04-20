use memmap2::Mmap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// One sparse index entry every SPARSE_STRIDE lines.
pub const SPARSE_STRIDE: u64 = 10_000;

/// Maximum bytes decoded per line for display (lines longer than this get truncated).
const MAX_LINE_BYTES: usize = 4 * 1024;

// ─── Sparse index ────────────────────────────────────────────────────────────

/// Sparse byte-offset index over newline positions.
///
/// `entries[i]` = byte offset of the first byte of line `(i * SPARSE_STRIDE)`.
/// Built incrementally by a background thread; `total_lines` and `complete`
/// reflect how far indexing has progressed.
pub struct SparseIndex {
    pub entries: Vec<u64>,
    pub total_lines: u64,
    pub complete: bool,
}

impl SparseIndex {
    fn new() -> Self {
        Self {
            entries: vec![0], // line 0 always starts at byte 0
            total_lines: 1,
            complete: false,
        }
    }

    /// Binary search for the nearest indexed line ≤ `target`.
    /// Returns `(entry_line_number, byte_offset)`.
    pub fn lookup(&self, target: u64) -> (u64, u64) {
        if self.entries.is_empty() {
            return (0, 0);
        }
        let i = ((target / SPARSE_STRIDE) as usize).min(self.entries.len() - 1);
        (i as u64 * SPARSE_STRIDE, self.entries[i])
    }
}

pub struct MappedFile {
    pub path: PathBuf,
    pub mmap: Arc<Mmap>,
    pub index: Arc<RwLock<SparseIndex>>,
}

impl MappedFile {
    /// Memory-map `path` and return a `MappedFile` ready for indexing.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = std::fs::File::open(&path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(Self {
            path,
            mmap: Arc::new(mmap),
            index: Arc::new(RwLock::new(SparseIndex::new())),
        })
    }

    /// Spawn a background thread that builds the sparse line index.
    /// Progress is visible immediately via `total_lines()` and `index_progress()`.
    pub fn start_indexing(&self) {
        let mmap = Arc::clone(&self.mmap);
        let index = Arc::clone(&self.index);
        std::thread::Builder::new()
            .name("index-builder".into())
            .spawn(move || build_sparse_index(mmap, index))
            .expect("failed to spawn indexing thread");
    }

    pub fn total_lines(&self) -> u64 {
        self.index.read().unwrap().total_lines
    }

    pub fn is_indexed(&self) -> bool {
        self.index.read().unwrap().complete
    }

    /// 0.0 → 1.0 progress estimate based on last indexed byte offset.
    pub fn index_progress(&self) -> f64 {
        let file_size = self.mmap.len() as f64;
        if file_size == 0.0 {
            return 1.0;
        }
        let idx = self.index.read().unwrap();
        if idx.complete {
            return 1.0;
        }
        idx.entries.last().copied().unwrap_or(0) as f64 / file_size
    }

    pub fn file_size(&self) -> u64 {
        self.mmap.len() as u64
    }
}

// ─── Line fetcher ─────────────────────────────────────────────────────────────

/// Fetch `count` consecutive lines starting at zero-based line `start`.
///
/// Uses the sparse index to seek close to `start`, then scans forward
/// linearly. Clones the Arcs so the `uniform_list` closure can capture them.
pub fn get_lines(
    mmap: &Mmap,
    index: &RwLock<SparseIndex>,
    start: u64,
    count: usize,
) -> Vec<String> {
    if count == 0 {
        return vec![];
    }

    let (entry_line, byte_offset) = {
        let idx = index.read().unwrap();
        idx.lookup(start)
    };

    let data: &[u8] = mmap;
    let mut pos = byte_offset as usize;
    let mut cur = entry_line;

    // Scan forward from the nearest index entry to the requested start line.
    while cur < start {
        match memchr::memchr(b'\n', &data[pos..]) {
            Some(nl) => {
                pos += nl + 1;
                cur += 1;
            }
            None => return vec![],
        }
        if pos >= data.len() {
            return vec![];
        }
    }

    // Collect `count` lines sequentially (single forward scan).
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        if pos >= data.len() {
            break;
        }
        let nl = memchr::memchr(b'\n', &data[pos..])
            .map(|n| pos + n)
            .unwrap_or(data.len());
        let raw = &data[pos..nl.min(pos + MAX_LINE_BYTES)];
        let text = String::from_utf8_lossy(raw);
        lines.push(text.trim_end_matches('\r').to_string());
        pos = nl + 1;
    }

    lines
}

// ─── Index builder (background thread) ───────────────────────────────────────

fn build_sparse_index(mmap: Arc<Mmap>, index: Arc<RwLock<SparseIndex>>) {
    let data: &[u8] = &mmap;
    let mut line_count: u64 = 0;
    let mut pending: Vec<u64> = Vec::new();

    // Scan in 64 MB chunks. After each chunk we publish accumulated entries
    // and update total_lines so the UI can show progress.
    const CHUNK: usize = 64 * 1024 * 1024;

    for (chunk_idx, chunk) in data.chunks(CHUNK).enumerate() {
        let chunk_start = chunk_idx * CHUNK;

        // memchr_iter uses SIMD (AVX2/SSE2) to find all '\n' in one pass.
        for nl_off in memchr::memchr_iter(b'\n', chunk) {
            line_count += 1;
            let abs_next = (chunk_start + nl_off + 1) as u64;
            if line_count % SPARSE_STRIDE == 0 {
                pending.push(abs_next);
            }
        }

        // Batch-write accumulated entries + progress update after each chunk.
        {
            let mut idx = index.write().unwrap();
            idx.entries.extend(pending.drain(..));
            idx.total_lines = line_count + 1; // +1 for the line after the last '\n'
        }
    }

    let mut idx = index.write().unwrap();
    if !pending.is_empty() {
        idx.entries.extend(pending);
    }
    idx.total_lines = line_count + 1;
    idx.complete = true;
}
