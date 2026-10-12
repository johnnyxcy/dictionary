use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, anyhow, ensure};
use async_channel::Sender;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use unicode_normalization::UnicodeNormalization;

use crate::model::{Attempt, AttemptStatus, Failure, HistoryEntry, SkillSnapshot, lookup_key, now};

type Job = Box<dyn FnOnce(&mut Database) + Send>;

/// One connection and one file lock, both owned by a dedicated database thread.
#[derive(Clone)]
pub struct Store {
    jobs: Sender<Job>,
}

impl Store {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let (jobs, receiver) = async_channel::bounded::<Job>(32);
        let (ready, initialized) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
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
        Ok(Self { jobs })
    }

    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.jobs
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

    pub async fn cached(&self, input: String) -> Result<Option<Attempt>> {
        self.call(move |db| db.cached(&input)).await
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
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= 2,
            "This history was created by a newer version of Dictionary"
        );
        if version == 0 {
            let tx = connection.transaction()?;
            tx.execute_batch(include_str!("../migrations/001_initial.sql"))?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = connection.transaction()?;
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
            tx.commit()?;
        }
        let mut db = Self {
            connection,
            _lock: lock,
        };
        db.recover_interrupted()?;
        Ok(db)
    }

    pub fn insert(&mut self, attempt: &Attempt, skill: &SkillSnapshot) -> Result<()> {
        let tx = self.connection.transaction()?;
        tx.execute("INSERT OR IGNORE INTO skill_snapshots(hash, name, source, content) VALUES (?1, ?2, ?3, ?4)",
            params![skill.hash, skill.name, skill.path.to_string_lossy(), skill.content])?;
        tx.execute("INSERT INTO lookups(id, input, search_key, direction, created_at, cache_key) VALUES (?1, ?2, ?3, 'en_zh', ?4, ?5) ON CONFLICT(id) DO NOTHING",
            params![attempt.lookup_id, attempt.input, attempt.input.nfc().collect::<String>().to_lowercase(), attempt.created_at, lookup_key(&attempt.input)])?;
        let original: String = tx.query_row(
            "SELECT input FROM lookups WHERE id=?1",
            [&attempt.lookup_id],
            |r| r.get(0),
        )?;
        ensure!(
            original == attempt.input,
            "A regenerated attempt must preserve the original input"
        );
        tx.execute("INSERT INTO attempts(id, lookup_id, attempt_number, state, created_at, model, effort, skill_hash, payload) VALUES (?1, ?2, (SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM attempts WHERE lookup_id=?2), 'pending', ?3, ?4, ?5, ?6, ?7)",
            params![attempt.id, attempt.lookup_id, attempt.created_at, attempt.setup.model, attempt.setup.effort, attempt.skill_hash, serde_json::to_string(attempt)?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn save(&mut self, attempt: &Attempt) -> Result<()> {
        attempt.usage.validate()?;
        let state = serde_json::to_value(&attempt.status)?;
        let changed = self.connection.execute(
            "UPDATE attempts SET state=?2, input_tokens=?3, cached_input_tokens=?4, output_tokens=?5, reasoning_output_tokens=?6, duration_ms=?7, payload=?8 WHERE id=?1",
            params![attempt.id, state.as_str(), attempt.usage.input_tokens, attempt.usage.cached_input_tokens,
                attempt.usage.output_tokens, attempt.usage.reasoning_output_tokens, attempt.duration_ms, serde_json::to_string(attempt)?])?;
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
            "SELECT a.id, l.input, a.created_at, a.state, json_extract(a.payload, '$.answer'), l.id FROM attempts a JOIN lookups l ON a.lookup_id=l.id ORDER BY a.created_at DESC, a.rowid DESC LIMIT 200")?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, input, created_at, state, answer, lookup_id)| {
                Ok(HistoryEntry {
                    id,
                    lookup_id,
                    input,
                    created_at,
                    status: serde_json::from_value(serde_json::Value::String(state))?,
                    translation: crate::model::translation_preview(&answer),
                    generating: !answer.is_empty(),
                })
            })
            .collect()
    }

    pub fn cached(&self, input: &str) -> Result<Option<Attempt>> {
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT a.payload FROM attempts a JOIN lookups l ON a.lookup_id=l.id
             WHERE l.direction='en_zh' AND l.cache_key=?1 AND a.state='succeeded'
               AND length(trim(json_extract(a.payload, '$.answer'))) > 0
             ORDER BY a.created_at DESC, a.rowid DESC LIMIT 1",
                [lookup_key(input)],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).context("Cannot read the cached lookup"))
            .transpose()
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
                "Dictionary closed before this lookup completed. You can submit it again.",
            ));
            self.save(&attempt)?;
        }
        Ok(())
    }
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
