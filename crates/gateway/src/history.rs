//! Recent-telemetry history (Tranche 3, `Methodology.md` D-049).
//!
//! Every message the gateway emits is also kept in a **bounded in-memory ring buffer** so a
//! dashboard that connects (or reloads) late can fetch what it missed, the way Open MCT's
//! historical telemetry providers expect. Optionally the ring is backed by a **JSON-lines file**:
//! each message is appended, and on startup the newest `capacity` lines are reloaded, so history
//! survives a restart.
//!
//! Design points:
//!
//! - **Bounded.** At most `capacity` messages in memory; the file is compacted back to `capacity`
//!   lines whenever it reaches twice that, so it cannot grow without limit either.
//! - **Never blocks the pipeline.** File writes happen on a dedicated thread fed by a bounded
//!   channel; if the disk cannot keep up, messages are still kept in memory and the dropped
//!   writes are counted (`history.persist_dropped`), never waited on.
//! - **One copy of the JSON.** An entry holds the exact text sent to WebSocket clients plus the
//!   numeric values of the dictionary points ([`crate::openmct::POINTS`]), extracted once.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;

use crate::config::HistoryConfig;
use crate::openmct::{POINTS, point_value};

/// Schema id of the `GET /api/v1/chronus/history` response.
pub const HISTORY_SCHEMA_V1: &str = "chronus.history.v1";
/// Default number of messages a query returns.
pub const DEFAULT_QUERY_LIMIT: usize = 1_000;
/// Largest number of messages a single query may return.
pub const MAX_QUERY_LIMIT: usize = 10_000;
/// Pending file writes allowed before new ones are dropped (and counted).
const PERSIST_QUEUE: usize = 8_192;
/// Longest history-file line accepted on reload; longer lines are skipped.
const MAX_LINE_BYTES: usize = 256 * 1024;

/// One emitted message.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HistoryEntry {
    /// Ground receipt time, milliseconds since the Unix epoch (the Open MCT `utc` domain).
    pub received_at_ms: i64,
    /// APID of the packet.
    pub apid: u16,
    /// The `openmct.realtime.v1` JSON text, exactly as streamed.
    pub json: Arc<str>,
    /// Value of each [`POINTS`] entry, in the same order.
    pub values: Box<[Option<f64>]>,
}

impl HistoryEntry {
    /// Builds an entry from one realtime message. Returns `None` if the text is not a JSON object
    /// with a parseable `received_at` and an `apid`.
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::history::HistoryEntry;
    ///
    /// let e = HistoryEntry::from_json(
    ///     r#"{"chronus_schema":"openmct.realtime.v1","apid":7,"received_at":"2026-10-24T12:00:00Z","elevation_deg":12.5}"#.into(),
    /// ).unwrap();
    /// assert_eq!(e.apid, 7);
    /// assert_eq!(e.received_at_ms, 1_792_843_200_000);
    /// ```
    #[must_use]
    pub fn from_json(json: Arc<str>) -> Option<Self> {
        let v: Value = serde_json::from_str(&json).ok()?;
        let received_at = v.get("received_at")?.as_str()?;
        let received_at_ms = DateTime::parse_from_rfc3339(received_at)
            .ok()?
            .timestamp_millis();
        let apid = u16::try_from(v.get("apid")?.as_u64()?).ok()?;
        let values = POINTS.iter().map(|p| point_value(&v, p)).collect();
        Some(Self {
            received_at_ms,
            apid,
            json,
            values,
        })
    }
}

/// Filter for [`HistoryStore::query`]. All bounds are inclusive.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct HistoryQuery {
    /// Earliest ground receipt time (ms since the Unix epoch).
    pub start_ms: Option<i64>,
    /// Latest ground receipt time (ms since the Unix epoch).
    pub end_ms: Option<i64>,
    /// Only this APID.
    pub apid: Option<u16>,
    /// Only messages where this point (index into [`POINTS`]) has a value.
    pub point: Option<usize>,
    /// Maximum entries returned; the **newest** matching entries are kept. `0` means the default.
    pub limit: usize,
}

impl HistoryQuery {
    /// An unbounded query with the default limit.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts the query to `[start_ms, end_ms]`.
    #[must_use]
    pub fn between(mut self, start_ms: Option<i64>, end_ms: Option<i64>) -> Self {
        self.start_ms = start_ms;
        self.end_ms = end_ms;
        self
    }

    /// Restricts the query to one APID.
    #[must_use]
    pub fn with_apid(mut self, apid: Option<u16>) -> Self {
        self.apid = apid;
        self
    }

    /// Restricts the query to entries carrying a value for this point.
    #[must_use]
    pub fn with_point(mut self, point: Option<usize>) -> Self {
        self.point = point;
        self
    }

    /// Sets the maximum number of entries (clamped to [`MAX_QUERY_LIMIT`]).
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    fn effective_limit(&self) -> usize {
        match self.limit {
            0 => DEFAULT_QUERY_LIMIT,
            n => n.min(MAX_QUERY_LIMIT),
        }
    }

    fn matches(&self, e: &HistoryEntry) -> bool {
        self.start_ms.is_none_or(|s| e.received_at_ms >= s)
            && self.end_ms.is_none_or(|t| e.received_at_ms <= t)
            && self.apid.is_none_or(|a| e.apid == a)
            && self
                .point
                .is_none_or(|i| e.values.get(i).copied().flatten().is_some())
    }
}

/// Result of a [`HistoryStore::query`]: matching entries, oldest first.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HistoryPage {
    /// The entries, in arrival order.
    pub entries: Vec<Arc<HistoryEntry>>,
    /// `true` when more entries matched than the limit allowed (the oldest were left out).
    pub truncated: bool,
}

/// History counters (exposed as `history` in `GET /api/v1/chronus/metrics`).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct HistoryStats {
    /// Messages recorded since start.
    pub recorded: AtomicU64,
    /// Messages evicted from memory to make room.
    pub evicted: AtomicU64,
    /// Messages reloaded from the history file at startup.
    pub loaded: AtomicU64,
    /// Lines in the history file that could not be reloaded (corrupt or foreign).
    pub load_skipped: AtomicU64,
    /// Messages written to the history file.
    pub persisted: AtomicU64,
    /// Messages not written because the write queue was full.
    pub persist_dropped: AtomicU64,
    /// File write or compaction errors.
    pub persist_errors: AtomicU64,
}

/// Serializable [`HistoryStats`] snapshot plus the current fill level.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct HistorySnapshot {
    /// Whether history is kept at all (`capacity > 0`).
    pub enabled: bool,
    /// Whether a history file is configured.
    pub persistent: bool,
    /// Configured capacity.
    pub capacity: usize,
    /// Messages in memory now.
    pub len: usize,
    /// Messages recorded since start.
    pub recorded: u64,
    /// Messages evicted from memory.
    pub evicted: u64,
    /// Messages reloaded at startup.
    pub loaded: u64,
    /// History-file lines skipped at startup.
    pub load_skipped: u64,
    /// Messages written to the file.
    pub persisted: u64,
    /// File writes dropped because the queue was full.
    pub persist_dropped: u64,
    /// File write errors.
    pub persist_errors: u64,
}

/// Why the history file could not be opened.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HistoryError {
    /// Reading, creating or rewriting the file failed.
    #[error("history file {path}: {source}")]
    Io {
        /// The configured path.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// The bounded history ring, optionally backed by a JSON-lines file.
#[derive(Debug)]
pub struct HistoryStore {
    capacity: usize,
    ring: Mutex<VecDeque<Arc<HistoryEntry>>>,
    stats: Arc<HistoryStats>,
    persist_path: Option<PathBuf>,
    writer: Mutex<Option<SyncSender<Arc<str>>>>,
    writer_thread: Mutex<Option<JoinHandle<()>>>,
}

impl HistoryStore {
    /// An in-memory store holding at most `capacity` messages (`0` disables history).
    #[must_use]
    pub fn in_memory(capacity: usize) -> Self {
        Self {
            capacity,
            ring: Mutex::new(VecDeque::with_capacity(capacity.min(4096))),
            stats: Arc::new(HistoryStats::default()),
            persist_path: None,
            writer: Mutex::new(None),
            writer_thread: Mutex::new(None),
        }
    }

    /// Opens the store described by `cfg`: in memory, or reloaded from and appended to
    /// `cfg.persist_path`. The file is created if missing and compacted to the newest
    /// `capacity` lines.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Io`] when the file exists but cannot be read, or cannot be created or
    /// rewritten (for example a missing directory or no write permission).
    pub fn open(cfg: &HistoryConfig) -> Result<Self, HistoryError> {
        let mut store = Self::in_memory(cfg.capacity);
        let Some(path) = cfg.persist_path.clone().filter(|_| cfg.capacity > 0) else {
            return Ok(store);
        };
        let io = |source| HistoryError::Io {
            path: path.clone(),
            source,
        };
        let lines = read_tail(&path, cfg.capacity, &store.stats).map_err(io)?;
        {
            let mut ring = store.lock_ring();
            for line in &lines {
                match HistoryEntry::from_json(Arc::clone(line)) {
                    Some(e) => ring.push_back(Arc::new(e)),
                    None => {
                        store.stats.load_skipped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            store
                .stats
                .loaded
                .store(ring.len() as u64, Ordering::Relaxed);
        }
        let kept: VecDeque<Arc<str>> = store
            .lock_ring()
            .iter()
            .map(|e| Arc::clone(&e.json))
            .collect();
        rewrite(&path, &kept).map_err(io)?;
        let file = OpenOptions::new().append(true).open(&path).map_err(io)?;

        let (tx, rx) = std::sync::mpsc::sync_channel(PERSIST_QUEUE);
        let stats = Arc::clone(&store.stats);
        let capacity = cfg.capacity;
        let thread_path = path.clone();
        let handle = std::thread::Builder::new()
            .name("chronus-history".into())
            .spawn(move || writer_loop(rx, file, thread_path, kept, capacity, &stats))
            .map_err(io)?;
        store.persist_path = Some(path);
        store.writer = Mutex::new(Some(tx));
        store.writer_thread = Mutex::new(Some(handle));
        Ok(store)
    }

    fn lock_ring(&self) -> std::sync::MutexGuard<'_, VecDeque<Arc<HistoryEntry>>> {
        // A panic while holding the lock cannot leave the deque inconsistent; keep serving.
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether history is kept at all.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.capacity > 0
    }

    /// Messages currently held in memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock_ring().len()
    }

    /// `true` when nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The configured history file, if any.
    #[must_use]
    pub fn persist_path(&self) -> Option<&Path> {
        self.persist_path.as_deref()
    }

    /// Records one emitted message (the exact `openmct.realtime.v1` JSON text).
    ///
    /// Returns `false` if history is off or the text is not a realtime message.
    pub fn record(&self, json: Arc<str>) -> bool {
        if self.capacity == 0 {
            return false;
        }
        let Some(entry) = HistoryEntry::from_json(Arc::clone(&json)) else {
            return false;
        };
        {
            let mut ring = self.lock_ring();
            if ring.len() >= self.capacity {
                ring.pop_front();
                self.stats.evicted.fetch_add(1, Ordering::Relaxed);
            }
            ring.push_back(Arc::new(entry));
        }
        self.stats.recorded.fetch_add(1, Ordering::Relaxed);
        let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(tx) = writer.as_ref() {
            match tx.try_send(json) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    self.stats.persist_dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.stats.persist_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        true
    }

    /// Entries matching `q`, oldest first, keeping the newest when more than the limit match.
    #[must_use]
    pub fn query(&self, q: &HistoryQuery) -> HistoryPage {
        let limit = q.effective_limit();
        let ring = self.lock_ring();
        let mut newest_first: Vec<Arc<HistoryEntry>> = Vec::new();
        let mut truncated = false;
        for e in ring.iter().rev() {
            if q.matches(e) {
                if newest_first.len() == limit {
                    truncated = true;
                    break;
                }
                newest_first.push(Arc::clone(e));
            }
        }
        drop(ring);
        newest_first.reverse();
        HistoryPage {
            entries: newest_first,
            truncated,
        }
    }

    /// Counters plus fill level.
    #[must_use]
    pub fn snapshot(&self) -> HistorySnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let s = &self.stats;
        HistorySnapshot {
            enabled: self.is_enabled(),
            persistent: self.persist_path.is_some(),
            capacity: self.capacity,
            len: self.len(),
            recorded: l(&s.recorded),
            evicted: l(&s.evicted),
            loaded: l(&s.loaded),
            load_skipped: l(&s.load_skipped),
            persisted: l(&s.persisted),
            persist_dropped: l(&s.persist_dropped),
            persist_errors: l(&s.persist_errors),
        }
    }

    /// Stops the file writer after it has written everything queued. Safe to call more than once;
    /// later [`record`](Self::record) calls keep history in memory only.
    pub fn close(&self) {
        let tx = self
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(tx);
        let handle = self
            .writer_thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(h) = handle
            && h.join().is_err()
        {
            self.stats.persist_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for HistoryStore {
    fn drop(&mut self) {
        self.close();
    }
}

/// Reads the newest `keep` non-empty lines of `path` (a missing file is empty history).
fn read_tail(path: &Path, keep: usize, stats: &HistoryStats) -> std::io::Result<Vec<Arc<str>>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut reader = BufReader::new(file);
    let mut tail: VecDeque<Arc<str>> = VecDeque::with_capacity(keep.min(4096));
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Bounded read: a corrupt file with no newlines cannot make us allocate without limit.
        let n = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') && n > MAX_LINE_BYTES {
            stats.load_skipped.fetch_add(1, Ordering::Relaxed);
            // Skip the rest of the oversized line.
            let mut sink = Vec::new();
            reader.read_until(b'\n', &mut sink)?;
            continue;
        }
        let Ok(text) = std::str::from_utf8(&buf) else {
            stats.load_skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if tail.len() == keep {
            tail.pop_front();
        }
        tail.push_back(Arc::from(text));
    }
    Ok(tail.into())
}

/// Atomically replaces `path` with `lines` (write to a sibling temp file, then rename).
fn rewrite(path: &Path, lines: &VecDeque<Arc<str>>) -> std::io::Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut w = BufWriter::new(File::create(&tmp)?);
        for l in lines {
            w.write_all(l.as_bytes())?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn writer_loop(
    rx: Receiver<Arc<str>>,
    file: File,
    path: PathBuf,
    mut kept: VecDeque<Arc<str>>,
    capacity: usize,
    stats: &HistoryStats,
) {
    let mut out = BufWriter::new(file);
    let mut lines_in_file = kept.len();
    let mut warned = false;
    let mut fail = |e: std::io::Error, what: &str| {
        stats.persist_errors.fetch_add(1, Ordering::Relaxed);
        if !warned {
            warned = true;
            tracing::warn!(path = %path.display(), error = %e, "history file {what} failed; history continues in memory");
        } else {
            tracing::debug!(error = %e, "history file {what} failed");
        }
    };
    while let Ok(first) = rx.recv() {
        // Drain whatever else is queued, then flush once.
        let batch = std::iter::once(first).chain(rx.try_iter());
        for line in batch {
            let res = out
                .write_all(line.as_bytes())
                .and_then(|()| out.write_all(b"\n"));
            match res {
                Ok(()) => {
                    stats.persisted.fetch_add(1, Ordering::Relaxed);
                    lines_in_file += 1;
                }
                Err(e) => fail(e, "write"),
            }
            if kept.len() == capacity {
                kept.pop_front();
            }
            kept.push_back(line);
        }
        if let Err(e) = out.flush() {
            fail(e, "flush");
        }
        if lines_in_file >= capacity.saturating_mul(2).max(1) {
            match rewrite(&path, &kept).and_then(|()| OpenOptions::new().append(true).open(&path)) {
                Ok(f) => {
                    out = BufWriter::new(f);
                    lines_in_file = kept.len();
                }
                Err(e) => fail(e, "compaction"),
            }
        }
    }
    if let Err(e) = out.flush() {
        fail(e, "flush");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(apid: u16, ms: i64, el: Option<f64>) -> Arc<str> {
        let t = DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339();
        let mut v = serde_json::json!({
            "chronus_schema": "openmct.realtime.v1",
            "apid": apid,
            "seq_count": 1,
            "received_at": t,
            "elevation_deg": el,
            "rf_matched": false,
        });
        v["physics_flags"] = 0.into();
        Arc::from(v.to_string())
    }

    #[test]
    fn ring_is_bounded_and_evicts_oldest() {
        let h = HistoryStore::in_memory(3);
        for i in 0..5 {
            assert!(h.record(msg(1, i, None)));
        }
        assert_eq!(h.len(), 3);
        let page = h.query(&HistoryQuery::new());
        let times: Vec<_> = page.entries.iter().map(|e| e.received_at_ms).collect();
        assert_eq!(times, vec![2, 3, 4]);
        let s = h.snapshot();
        assert_eq!((s.recorded, s.evicted, s.len), (5, 2, 3));
    }

    #[test]
    fn disabled_store_records_nothing() {
        let h = HistoryStore::in_memory(0);
        assert!(!h.record(msg(1, 1, None)));
        assert!(h.is_empty());
        assert!(!h.snapshot().enabled);
    }

    #[test]
    fn non_messages_are_rejected() {
        let h = HistoryStore::in_memory(4);
        assert!(!h.record(Arc::from("not json")));
        assert!(!h.record(Arc::from(r#"{"apid":1}"#)));
        assert!(!h.record(Arc::from(
            r#"{"apid":99999,"received_at":"2026-01-01T00:00:00Z"}"#
        )));
        assert!(h.is_empty());
    }

    #[test]
    fn query_filters_by_time_apid_point_and_keeps_the_newest() {
        let h = HistoryStore::in_memory(100);
        for i in 0..10 {
            h.record(msg(
                if i % 2 == 0 { 5 } else { 6 },
                i * 1000,
                (i < 5).then_some(1.0),
            ));
        }
        let p = h.query(&HistoryQuery::new().between(Some(2000), Some(6000)));
        assert_eq!(p.entries.len(), 5);
        assert!(!p.truncated);
        let p = h.query(&HistoryQuery::new().with_apid(Some(6)));
        assert!(p.entries.iter().all(|e| e.apid == 6));
        let el = crate::openmct::point_index("chronus.physics.elevation_deg");
        let p = h.query(&HistoryQuery::new().with_point(el));
        assert_eq!(p.entries.len(), 5);
        let p = h.query(&HistoryQuery::new().with_limit(3));
        assert!(p.truncated);
        let t: Vec<_> = p.entries.iter().map(|e| e.received_at_ms).collect();
        assert_eq!(t, vec![7000, 8000, 9000], "newest kept, oldest first");
        let p = h.query(&HistoryQuery::new().with_limit(usize::MAX));
        assert_eq!(p.entries.len(), 10, "limit clamps, does not overflow");
    }

    #[test]
    fn persisted_history_survives_a_restart_and_is_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let cfg = HistoryConfig {
            capacity: 5,
            persist_path: Some(path.clone()),
        };
        {
            let h = HistoryStore::open(&cfg).unwrap();
            for i in 0..23 {
                h.record(msg(2, i, Some(i as f64)));
            }
            h.close();
            let s = h.snapshot();
            assert_eq!(s.persisted, 23);
            assert_eq!(s.persist_errors, 0);
        }
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert!(lines < 2 * 5 + 5, "file compacted, got {lines} lines");

        // Corrupt line in the middle is skipped on reload.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{garbage\n");
        std::fs::write(&path, text).unwrap();
        let h = HistoryStore::open(&cfg).unwrap();
        let page = h.query(&HistoryQuery::new());
        let times: Vec<_> = page.entries.iter().map(|e| e.received_at_ms).collect();
        assert_eq!(
            times,
            vec![19, 20, 21, 22],
            "newest lines reloaded, garbage skipped"
        );
        let s = h.snapshot();
        assert_eq!((s.loaded, s.load_skipped), (4, 1));
        assert!(s.persistent);
        h.close();
        // After open the file was rewritten without the garbage line.
        assert!(!std::fs::read_to_string(&path).unwrap().contains("garbage"));
    }

    #[test]
    fn unopenable_history_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = HistoryConfig {
            persist_path: Some(dir.path().join("missing-dir").join("h.jsonl")),
            ..HistoryConfig::default()
        };
        let err = HistoryStore::open(&cfg).unwrap_err();
        assert!(err.to_string().contains("missing-dir"));
    }

    #[test]
    fn oversized_lines_are_skipped_on_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let mut text = "x".repeat(MAX_LINE_BYTES + 10);
        text.push('\n');
        text.push_str(&msg(3, 5, None));
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        let cfg = HistoryConfig {
            persist_path: Some(path),
            ..HistoryConfig::default()
        };
        let h = HistoryStore::open(&cfg).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h.snapshot().load_skipped, 1);
    }
}
