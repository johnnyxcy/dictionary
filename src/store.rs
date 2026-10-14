use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, ensure};
use async_channel::Sender;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{Attempt, AttemptStatus, Failure, HistoryEntry, SkillSnapshot, lookup_key, now};
use crate::{
    analysis::{AnalysisQuery, AnalysisRecord, AnalysisReport},
    direction::Direction,
    history::{
        HistoryCursor, HistoryPage, HistoryQuery, HistoryRow, PAGE_SIZE, normalize_search_text,
    },
    settings::{Catalog, Preferences},
};

type Job = Box<dyn FnOnce(&mut Database) + Send>;

const SCHEMA_VERSION: i64 = 5;

/// One connection and one file lock, both owned by a dedicated database thread.
#[derive(Clone)]
pub struct Store {
    worker: Arc<StorageWorker>,
}

struct StorageWorker {
    jobs: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        self.jobs.close();
        // Finish pending writes and release the database lock before a new
        // service can reopen this profile during restart.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Store {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let (jobs, receiver) = async_channel::bounded::<Job>(32);
        let (ready, initialized) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("dictionary-storage".into())
            .spawn(move || {
                let mut database = match Database::open(&path) {
                    Ok(database) => database,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(job) = receiver.recv_blocking() {
                    job(&mut database);
                }
            })?;
        initialized
            .await
            .context("Storage worker stopped during initialization")??;
        Ok(Self {
            worker: Arc::new(StorageWorker {
                jobs,
                thread: Some(thread),
            }),
        })
    }

    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.worker
            .jobs
            .send(Box::new(move |db| {
                let _ = tx.send(f(db));
            }))
            .await
            .map_err(|_| anyhow!("Storage worker is unavailable"))?;
        rx.await
            .context("Storage worker stopped before saving the result")?
    }

    pub async fn insert(&self, attempt: Attempt, skill: SkillSnapshot) -> Result<()> {
        self.call(move |db| db.insert(&attempt, &skill)).await
    }

    pub async fn save(&self, attempt: Attempt) -> Result<()> {
        self.call(move |db| db.save(&attempt)).await
    }

    pub async fn finalize(&self, mut attempt: Attempt, submitted: Instant) -> Result<Attempt> {
        self.call(move |db| {
            // Save the complete outcome atomically before measuring durable
            // availability. The following write records that measured boundary;
            // its own bookkeeping time is deliberately outside the metric.
            db.save(&attempt)?;
            attempt.duration_ms =
                Some(submitted.elapsed().as_millis().min(u64::MAX as u128) as u64);
            attempt.completed_at = Some(now());
            db.save(&attempt)?;
            Ok(attempt)
        })
        .await
    }

    pub async fn get(&self, id: String) -> Result<Attempt> {
        self.call(move |db| db.get(&id)).await
    }

    pub async fn history(&self) -> Result<Vec<HistoryEntry>> {
        self.call(|db| db.history()).await
    }

    pub async fn history_page(&self, query: HistoryQuery) -> Result<HistoryPage> {
        self.call(move |db| db.history_page(&query)).await
    }

    pub async fn analysis(&self, query: AnalysisQuery) -> Result<AnalysisReport> {
        self.call(move |db| db.analysis(query)).await
    }

    pub async fn cached(&self, input: String, direction: Direction) -> Result<Option<Attempt>> {
        self.call(move |db| db.cached(&input, direction)).await
    }

    pub async fn preferences(&self) -> Result<Option<Preferences>> {
        self.call(|db| db.read_state("preferences")).await
    }

    pub async fn catalog(&self) -> Result<Option<Catalog>> {
        self.call(|db| db.read_state("catalog")).await
    }

    pub async fn save_preferences(&self, preferences: Preferences) -> Result<()> {
        self.call(move |db| db.write_state("preferences", &preferences))
            .await
    }

    pub async fn save_catalog(&self, catalog: Catalog) -> Result<()> {
        self.call(move |db| db.write_state("catalog", &catalog))
            .await
    }

    pub async fn delete_lookup(&self, lookup_id: String) -> Result<()> {
        self.call(move |db| db.delete_lookup(&lookup_id)).await
    }
}

pub struct Database {
    connection: Connection,
    // Fields drop in order: close SQLite before releasing ownership of history.
    _lock: HistoryLock,
}

struct HistoryLock(File);

impl Drop for HistoryLock {
    fn drop(&mut self) {
        // A concurrently spawned process can briefly inherit this descriptor
        // before exec closes it. Closing our copy alone would retain the lock.
        let _ = FileExt::unlock(&self.0);
    }
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()
            .context("This dictionary history is already open in another application instance")?;
        let lock = HistoryLock(lock);
        let mut connection = Connection::open(path).context("Cannot open dictionary history")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= SCHEMA_VERSION,
            "This history was created by a newer version of JDict"
        );
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        if version < SCHEMA_VERSION {
            let has_data: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table')",
                [],
                |r| r.get(0),
            )?;
            let backup = has_data.then(|| {
                path.with_extension(format!("pre-v{version}-{}.sqlite3", uuid::Uuid::new_v4()))
            });
            if let Some(backup) = &backup {
                // SQLite copies a consistent snapshot, including committed WAL
                // contents. A file copy could silently omit the latest history.
                connection.execute("VACUUM INTO ?1", [backup.to_str().context("Backup path must be valid UTF-8")?])
                    .with_context(|| format!("Cannot back up history to {}. The upgrade was not started; free disk space and check folder permissions.", backup.display()))?;
            }
            migrate(&mut connection, version).with_context(|| match &backup {
                Some(backup) => format!("History upgrade failed and was rolled back. A recovery copy is at {}. Keep the original database and this backup before retrying.", backup.display()),
                None => "Cannot create dictionary history; initialization was rolled back".into(),
            })?;
        }
        let mut db = Self {
            connection,
            _lock: lock,
        };
        db.recover_interrupted()?;
        Ok(db)
    }

    pub fn read_state<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let json: Option<String> = self
            .connection
            .query_row("SELECT payload FROM app_state WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        json.map(|value| serde_json::from_str(&value).context("Cannot read application settings"))
            .transpose()
    }

    pub fn write_state<T: serde::Serialize>(&mut self, key: &str, value: &T) -> Result<()> {
        self.connection.execute("INSERT INTO app_state(key, payload) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload", params![key, serde_json::to_string(value)?])?;
        Ok(())
    }

    pub fn insert(&mut self, attempt: &Attempt, skill: &SkillSnapshot) -> Result<()> {
        let tx = self.connection.transaction()?;
        tx.execute("INSERT OR IGNORE INTO skill_snapshots(hash, name, source, content) VALUES (?1, ?2, ?3, ?4)",
            params![skill.hash, skill.name, skill.path.to_string_lossy(), skill.content])?;
        tx.execute("INSERT INTO lookups(id, input, search_key, direction, created_at, cache_key, requested_direction) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(id) DO NOTHING",
            params![attempt.lookup_id, attempt.input, normalize_search_text(&attempt.input), attempt.direction.key(), attempt.created_at, lookup_key(&attempt.input), serde_json::to_value(attempt.requested_direction)?.as_str()])?;
        let original: (String, String) = tx.query_row(
            "SELECT input, direction FROM lookups WHERE id=?1",
            [&attempt.lookup_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            original.0 == attempt.input && original.1 == attempt.direction.key(),
            "A regenerated attempt must preserve the original input and direction"
        );
        tx.execute("INSERT INTO attempts(id, lookup_id, attempt_number, state, created_at, model, effort, skill_hash, payload, answer_search_key) VALUES (?1, ?2, (SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM attempts WHERE lookup_id=?2), 'pending', ?3, ?4, ?5, ?6, ?7, ?8)",
            params![attempt.id, attempt.lookup_id, attempt.created_at, attempt.setup.model, attempt.setup.effort, attempt.skill_hash, serde_json::to_string(attempt)?, normalize_search_text(&attempt.answer)])?;
        tx.commit()?;
        Ok(())
    }

    pub fn save(&mut self, attempt: &Attempt) -> Result<()> {
        attempt.usage.validate()?;
        let state = serde_json::to_value(&attempt.status)?;
        let changed = self.connection.execute(
            "UPDATE attempts SET state=?2, input_tokens=?3, cached_input_tokens=?4, output_tokens=?5, reasoning_output_tokens=?6, duration_ms=?7, payload=?8, answer_search_key=?9 WHERE id=?1",
            params![attempt.id, state.as_str(), attempt.usage.input_tokens, attempt.usage.cached_input_tokens,
                attempt.usage.output_tokens, attempt.usage.reasoning_output_tokens, attempt.duration_ms, serde_json::to_string(attempt)?, normalize_search_text(&attempt.answer)])?;
        ensure!(changed == 1, "The lookup no longer exists in history");
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Attempt> {
        let json: Option<String> = self
            .connection
            .query_row("SELECT payload FROM attempts WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        serde_json::from_str(&json.context("Saved lookup was not found")?)
            .context("Cannot read the saved lookup")
    }

    pub fn history(&self) -> Result<Vec<HistoryEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT a.id, l.input, a.created_at, a.state, json_extract(a.payload, '$.answer'), l.id, l.direction FROM attempts a JOIN lookups l ON a.lookup_id=l.id ORDER BY a.created_at DESC, a.rowid DESC LIMIT 200")?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(
                |(id, input, created_at, state, answer, lookup_id, direction)| {
                    let direction: Direction =
                        serde_json::from_value(serde_json::Value::String(direction))?;
                    Ok(HistoryEntry {
                        id,
                        lookup_id,
                        input,
                        direction,
                        created_at,
                        status: serde_json::from_value(serde_json::Value::String(state))?,
                        translation: crate::model::translation_preview_for(&answer, direction),
                        generating: !answer.is_empty(),
                    })
                },
            )
            .collect()
    }

    pub fn history_page(&self, query: &HistoryQuery) -> Result<HistoryPage> {
        let search = query.filter.search_key();
        let direction = query.filter.direction.map(Direction::key);
        let status = query
            .filter
            .status
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        let status = status.as_ref().and_then(serde_json::Value::as_str);
        // instr treats %, _, quotes, and backslashes as literal text. Both
        // inputs and answers use the same Unicode normalization as the query.
        let filter = "FROM attempts a JOIN lookups l ON a.lookup_id=l.id
            WHERE (instr(l.search_key, ?1) > 0 OR instr(a.answer_search_key, ?1) > 0)
              AND (?2 IS NULL OR l.direction=?2)
              AND (?3 IS NULL OR a.state=?3)";
        let (attempts, lookups) = self.connection.query_row(
            &format!("SELECT COUNT(*), COUNT(DISTINCT a.lookup_id) {filter}"),
            params![search, direction, status],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT a.id, a.lookup_id, l.input, l.direction, a.created_at, a.state,
                json_extract(a.payload, '$.answer'), a.attempt_number, a.model, a.effort,
                json_extract(a.payload, '$.setup.service_tier'), a.input_tokens,
                a.cached_input_tokens, a.output_tokens, a.reasoning_output_tokens, a.duration_ms,
                EXISTS(SELECT 1 FROM attempts active WHERE active.lookup_id=a.lookup_id
                    AND active.state IN ('pending', 'running'))
             {filter}
               AND (?4 IS NULL OR (a.created_at, a.id) < (?4, ?5))
             ORDER BY a.created_at DESC, a.id DESC LIMIT ?6"
        ))?;
        let mut records = statement.query(params![
            search,
            direction,
            status,
            query.before.as_ref().map(|c| &c.created_at),
            query.before.as_ref().map(|c| &c.id),
            (PAGE_SIZE + 1) as u64
        ])?;
        let mut rows = Vec::new();
        while let Some(row) = records.next()? {
            let direction: Direction =
                serde_json::from_value(serde_json::Value::String(row.get(3)?))?;
            let answer: String = row.get(6)?;
            let tier: Option<String> = row.get(10)?;
            rows.push(HistoryRow {
                entry: HistoryEntry {
                    id: row.get(0)?,
                    lookup_id: row.get(1)?,
                    input: row.get(2)?,
                    direction,
                    created_at: row.get(4)?,
                    status: serde_json::from_value(serde_json::Value::String(row.get(5)?))?,
                    translation: crate::model::translation_preview_for(&answer, direction),
                    generating: !answer.is_empty(),
                },
                version: row.get(7)?,
                model: row.get(8)?,
                effort: row.get(9)?,
                tier: tier
                    .map(|value| serde_json::from_value(serde_json::Value::String(value)))
                    .transpose()?,
                usage: crate::model::Usage {
                    input_tokens: row.get(11)?,
                    cached_input_tokens: row.get(12)?,
                    output_tokens: row.get(13)?,
                    reasoning_output_tokens: row.get(14)?,
                },
                duration_ms: row.get(15)?,
                lookup_active: row.get(16)?,
            });
        }
        let next = if rows.len() > PAGE_SIZE {
            rows.pop();
            rows.last().map(|row| HistoryCursor {
                created_at: row.entry.created_at.clone(),
                id: row.entry.id.clone(),
            })
        } else {
            None
        };
        Ok(HistoryPage {
            rows,
            next,
            attempts,
            lookups,
        })
    }

    pub fn cached(&self, input: &str, direction: Direction) -> Result<Option<Attempt>> {
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT a.payload FROM attempts a JOIN lookups l ON a.lookup_id=l.id
             WHERE l.direction=?2 AND l.cache_key=?1 AND a.state='succeeded'
               AND length(trim(json_extract(a.payload, '$.answer'))) > 0
             ORDER BY a.created_at DESC, a.rowid DESC LIMIT 1",
                params![lookup_key(input), direction.key()],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).context("Cannot read the cached lookup"))
            .transpose()
    }

    pub fn analysis(&self, query: AnalysisQuery) -> Result<AnalysisReport> {
        let (start, end) = query.range.bounds(query.timezone)?;
        let status = query
            .filter
            .status
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        // SQLite converts RFC3339 to instants, so fractional seconds and legacy
        // UTC spellings have identical boundary semantics. Project measurements
        // only; never deserialize full answers or skill snapshots for charts.
        let mut statement = self.connection.prepare(
            "SELECT a.id, a.attempt_number, l.input, l.direction,
                a.created_at, a.state,
                a.input_tokens, a.cached_input_tokens, a.output_tokens, a.reasoning_output_tokens,
                a.duration_ms, json_extract(a.payload, '$.first_answer_ms'),
                json_extract(a.payload, '$.queue_duration_ms'), json_extract(a.payload, '$.cli_duration_ms')
             FROM attempts a JOIN lookups l ON l.id=a.lookup_id
             WHERE julianday(a.created_at) >= julianday(?1)
               AND julianday(a.created_at) < julianday(?2)
               AND julianday(a.created_at) <= julianday(?3)
               AND (?4 IS NULL OR a.model=?4) AND (?5 IS NULL OR a.effort=?5)
               AND (?6 IS NULL OR l.direction=?6) AND (?7 IS NULL OR a.state=?7)
             ORDER BY a.created_at DESC, a.id DESC")?;
        let mut rows = statement.query(params![
            start.to_rfc3339(),
            end.to_rfc3339(),
            query.as_of.to_rfc3339(),
            query.filter.model,
            query.filter.effort,
            query.filter.direction.map(Direction::key),
            status.as_ref().and_then(serde_json::Value::as_str)
        ])?;
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            let created: String = row.get(4)?;
            records.push(AnalysisRecord {
                id: row.get(0)?,
                version: row.get(1)?,
                input: row.get(2)?,
                direction: serde_json::from_value(serde_json::Value::String(row.get(3)?))?,
                created_at: chrono::DateTime::parse_from_rfc3339(&created)?
                    .with_timezone(&chrono::Utc),
                status: serde_json::from_value(serde_json::Value::String(row.get(5)?))?,
                usage: crate::model::Usage {
                    input_tokens: row.get(6)?,
                    cached_input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    reasoning_output_tokens: row.get(9)?,
                },
                duration_ms: row.get(10)?,
                first_answer_ms: row.get(11)?,
                queue_duration_ms: row.get(12)?,
                cli_duration_ms: row.get(13)?,
            });
        }
        let models = self
            .connection
            .prepare("SELECT DISTINCT model FROM attempts ORDER BY model")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let efforts = self
            .connection
            .prepare("SELECT DISTINCT effort FROM attempts ORDER BY effort")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        AnalysisReport::build(query, records, models, efforts)
    }

    /// Delete the lookup and every version together. The cascade also removes
    /// usage and timing records; shared skill snapshots remain until unused.
    pub fn delete_lookup(&mut self, lookup_id: &str) -> Result<()> {
        let tx = self.connection.transaction()?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE lookup_id=?1 AND state IN ('pending', 'running'))",
            [lookup_id],
            |r| r.get(0),
        )?;
        ensure!(
            !active,
            "Stop this lookup or wait for it to finish before deleting it"
        );
        let deleted = tx.execute("DELETE FROM lookups WHERE id=?1", [lookup_id])?;
        ensure!(deleted == 1, "The lookup no longer exists in history");
        tx.execute(
            "DELETE FROM skill_snapshots WHERE NOT EXISTS(SELECT 1 FROM attempts WHERE attempts.skill_hash=skill_snapshots.hash)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn recover_interrupted(&mut self) -> Result<()> {
        let ids = {
            let mut statement = self
                .connection
                .prepare("SELECT id FROM attempts WHERE state IN ('pending', 'running')")?;
            statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in ids {
            let mut attempt = self.get(&id)?;
            attempt.status = AttemptStatus::Interrupted;
            attempt.completed_at = Some(now());
            attempt.failure = Some(Failure::new(
                "interrupted",
                "JDict closed before this lookup completed. You can submit it again.",
            ));
            self.save(&attempt)?;
        }
        Ok(())
    }
}

fn migrate(connection: &mut Connection, version: i64) -> Result<()> {
    // All pending versions succeed together; a later failure cannot strand an
    // existing installation halfway through an upgrade.
    let tx = connection.transaction()?;
    if version == 0 {
        tx.execute_batch(include_str!("../migrations/001_initial.sql"))?;
    }
    if version < 2 {
        tx.execute_batch(include_str!("../migrations/002_lookup_cache.sql"))?;
        let lookups = {
            let mut statement = tx.prepare("SELECT id, input FROM lookups")?;
            statement
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, input) in lookups {
            tx.execute(
                "UPDATE lookups SET cache_key=?2 WHERE id=?1",
                params![id, lookup_key(&input)],
            )?;
        }
    }
    if version < 3 {
        tx.execute_batch(include_str!("../migrations/003_settings.sql"))?;
    }
    if version < 4 {
        tx.execute_batch(include_str!("../migrations/004_history.sql"))?;
    }
    if version < 5 {
        tx.execute_batch(include_str!("../migrations/005_translation_search.sql"))?;
        let ids = {
            let mut statement = tx.prepare("SELECT id FROM attempts")?;
            statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        // Normalize one answer at a time without loading all saved prose or
        // rewriting the immutable attempt payloads.
        let mut answers = tx.prepare(
            "SELECT COALESCE(json_extract(payload, '$.answer'), '') FROM attempts WHERE id=?1",
        )?;
        let mut update = tx.prepare("UPDATE attempts SET answer_search_key=?2 WHERE id=?1")?;
        for id in ids {
            let answer: String = answers.query_row([&id], |r| r.get(0))?;
            update.execute(params![id, normalize_search_text(&answer)])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_database_releases_lock_even_if_a_spawn_inherited_the_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.sqlite3");
        let database = Database::open(&path).unwrap();
        // A clone shares the file description, just like a fork before exec's
        // close-on-exec cleanup. It must not prolong the owner's history lock.
        let _inherited = database._lock.0.try_clone().unwrap();
        drop(database);
        let _reopened = Database::open(&path).unwrap();
    }
}
