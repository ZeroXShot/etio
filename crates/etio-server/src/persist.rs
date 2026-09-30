//! Persistence: engine snapshots and the incident store.
//!
//! * **Snapshots** capture the engine's learned state (series history,
//!   detector baselines, incidents, graph) every `snapshot_interval` and at
//!   shutdown, so that a restart does not mean hours of re-learning. The file
//!   is `ETIOSNAP` + container version + payload length + CRC-32 + an
//!   LZ4-compressed postcard payload, written to a temporary file, fsynced
//!   and atomically renamed: a crash mid-write leaves the previous snapshot
//!   intact, and a corrupted file is detected and ignored.
//! * **SQLite** keeps every incident ever seen (the engine keeps only recent
//!   ones in memory) and the feedback users give on root causes, which is
//!   what a retrained ranking model learns from.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use etio_engine::{EngineSnapshot, Event, Incident};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::broadcast::error::RecvError;

use crate::actor::EngineHandle;

const MAGIC: &[u8; 8] = b"ETIOSNAP";
const CONTAINER_VERSION: u32 = 1;
const SCHEMA_VERSION: i32 = 1;

/// Serialises a snapshot to `path` atomically. Returns the file size.
///
/// # Errors
/// Fails on serialisation or I/O errors.
pub fn write_snapshot(path: &Path, snap: &EngineSnapshot) -> anyhow::Result<usize> {
    let payload = postcard::to_stdvec(snap).context("serialising snapshot")?;
    let compressed = lz4_flex::compress_prepend_size(&payload);
    let mut buf = Vec::with_capacity(compressed.len() + 24);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
    buf.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
    buf.extend_from_slice(&crc32fast::hash(&compressed).to_le_bytes());
    buf.extend_from_slice(&compressed);

    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renaming snapshot into {}", path.display()))?;
    if let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(buf.len())
}

/// Reads a snapshot. Returns `Ok(None)` if the file does not exist.
///
/// # Errors
/// Fails if the file exists but is corrupted or of an unknown version.
pub fn read_snapshot(path: &Path) -> anyhow::Result<Option<EngineSnapshot>> {
    let buf = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if buf.len() < 24 || &buf[..8] != MAGIC {
        bail!("{} is not an Etio snapshot", path.display());
    }
    let version = u32::from_le_bytes(buf[8..12].try_into()?);
    if version != CONTAINER_VERSION {
        bail!("unsupported snapshot container version {version}");
    }
    let len = usize::try_from(u64::from_le_bytes(buf[12..20].try_into()?))?;
    let crc = u32::from_le_bytes(buf[20..24].try_into()?);
    let payload = buf.get(24..24 + len).context("snapshot is truncated")?;
    if crc32fast::hash(payload) != crc {
        bail!("snapshot checksum mismatch (corrupted file)");
    }
    let raw = lz4_flex::decompress_size_prepended(payload).context("decompressing snapshot")?;
    Ok(Some(postcard::from_bytes(&raw).context("decoding snapshot")?))
}

/// A feedback entry.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FeedbackRow {
    /// Incident.
    pub incident: String,
    /// Confirmed root cause.
    pub root_cause: String,
    /// Free text.
    pub comment: String,
    /// When it was recorded, seconds since the epoch.
    pub recorded_at: i64,
}

/// The durable store.
pub struct Store {
    conn: Mutex<Connection>,
    dir: PathBuf,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (or creates) the store in `dir`.
    ///
    /// # Errors
    /// Fails if the directory or database cannot be created or migrated.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let conn = Connection::open(dir.join("etio.sqlite")).context("opening the incident database")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("database schema {version} is newer than this server ({SCHEMA_VERSION})");
        }
        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS incidents (
                     id TEXT PRIMARY KEY,
                     status TEXT NOT NULL,
                     start_ns INTEGER NOT NULL,
                     opened_ns INTEGER NOT NULL,
                     resolved_ns INTEGER,
                     top_candidate TEXT,
                     body TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS incidents_start ON incidents(start_ns);
                 CREATE TABLE IF NOT EXISTS feedback (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     incident TEXT NOT NULL,
                     root_cause TEXT NOT NULL,
                     comment TEXT NOT NULL,
                     recorded_at INTEGER NOT NULL
                 );
                 PRAGMA user_version = 1;",
            )?;
        }
        Ok(Self { conn: Mutex::new(conn), dir: dir.to_owned() })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Path of the engine snapshot.
    #[must_use]
    pub fn snapshot_path(&self) -> PathBuf {
        self.dir.join("engine.snapshot")
    }

    /// Inserts or updates an incident.
    ///
    /// # Errors
    /// Fails on database errors.
    pub fn upsert_incident(&self, i: &Incident) -> anyhow::Result<()> {
        let body = serde_json::to_string(i)?;
        let status = serde_json::to_value(i.status)?.as_str().unwrap_or("open").to_owned();
        self.conn().execute(
            "INSERT INTO incidents (id, status, start_ns, opened_ns, resolved_ns, top_candidate, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, resolved_ns = excluded.resolved_ns,
                 top_candidate = excluded.top_candidate, body = excluded.body",
            params![
                i.id,
                status,
                i.start,
                i.opened_at,
                i.resolved_at,
                i.top_candidate().map(|(s, _)| s.to_owned()),
                body
            ],
        )?;
        Ok(())
    }

    /// Looks up an incident.
    ///
    /// # Errors
    /// Fails on database or decoding errors.
    pub fn incident(&self, id: &str) -> anyhow::Result<Option<Incident>> {
        let body: Option<String> =
            self.conn().query_row("SELECT body FROM incidents WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        body.map(|b| serde_json::from_str(&b).context("decoding stored incident")).transpose()
    }

    /// Records the root cause a user confirmed.
    ///
    /// # Errors
    /// Fails on database errors.
    pub fn record_feedback(&self, incident: &str, root_cause: &str, comment: &str) -> anyhow::Result<()> {
        let now = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs())?;
        self.conn().execute(
            "INSERT INTO feedback (incident, root_cause, comment, recorded_at) VALUES (?1, ?2, ?3, ?4)",
            params![incident, root_cause, comment, now],
        )?;
        Ok(())
    }

    /// All feedback, oldest first.
    ///
    /// # Errors
    /// Fails on database errors.
    pub fn feedback(&self) -> anyhow::Result<Vec<FeedbackRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT incident, root_cause, comment, recorded_at FROM feedback ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(FeedbackRow { incident: r.get(0)?, root_cause: r.get(1)?, comment: r.get(2)?, recorded_at: r.get(3)? })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// Takes a snapshot through the engine actor and writes it.
///
/// # Errors
/// Fails if the engine stopped or the file cannot be written.
pub async fn snapshot_now(handle: &EngineHandle, store: &Store) -> anyhow::Result<usize> {
    let started = Instant::now();
    let snap = handle.with_engine(|e| e.snapshot()).await?;
    let path = store.snapshot_path();
    let bytes = tokio::task::spawn_blocking(move || write_snapshot(&path, &snap)).await??;
    let m = handle.metrics();
    m.snapshot_seconds.observe(started.elapsed().as_secs_f64());
    m.snapshot_bytes.set(i64::try_from(bytes).unwrap_or(i64::MAX));
    Ok(bytes)
}

/// Background task: periodic snapshots, and every incident event persisted.
pub async fn run(handle: EngineHandle, store: Arc<Store>, interval: Duration) {
    let mut events = handle.subscribe();
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = snapshot_now(&handle, &store).await {
                    tracing::warn!(error = %e, "snapshot failed");
                }
            }
            ev = events.recv() => match ev {
                Ok(event) => {
                    let incident = match &*event {
                        Event::IncidentOpened { incident } | Event::IncidentAnalyzed { incident } | Event::IncidentResolved { incident } => incident,
                    };
                    let store = store.clone();
                    let incident = incident.clone();
                    let r = tokio::task::spawn_blocking(move || store.upsert_incident(&incident)).await;
                    if !matches!(r, Ok(Ok(()))) {
                        tracing::warn!("failed to persist incident");
                    }
                }
                Err(RecvError::Lagged(n)) => tracing::warn!(missed = n, "persistence fell behind the event stream"),
                Err(RecvError::Closed) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etio_engine::{Engine, EngineConfig};

    #[test]
    fn snapshots_round_trip_and_detect_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.snapshot");
        assert!(read_snapshot(&path).unwrap().is_none());
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let bytes = write_snapshot(&path, &engine.snapshot()).unwrap();
        assert!(bytes > 24);
        let back = read_snapshot(&path).unwrap().unwrap();
        assert_eq!(back.format, etio_engine::SNAPSHOT_FORMAT);

        let mut raw = std::fs::read(&path).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xff;
        std::fs::write(&path, &raw).unwrap();
        assert!(read_snapshot(&path).unwrap_err().to_string().contains("checksum"));
        std::fs::write(&path, b"garbage").unwrap();
        assert!(read_snapshot(&path).is_err());
    }

    #[test]
    fn store_keeps_incidents_and_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert!(store.incident("nope").unwrap().is_none());
        store.record_feedback("inc-1", "cart", "confirmed by on-call").unwrap();
        let fb = store.feedback().unwrap();
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].root_cause, "cart");
        drop(store);
        // Re-opening an existing database keeps its content and schema.
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.feedback().unwrap().len(), 1);
    }
}
