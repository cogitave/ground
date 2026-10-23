use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ports::{Clock, RepoSlug};

/// `~/.ground/state.db`.
pub fn default_db_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".ground").join("state.db")
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("no ship session {0}")]
    NoSession(String),
    #[error("state database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("state database: {0}")]
    Io(#[from] std::io::Error),
    #[error("state database: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipSession {
    pub id: String,
    pub repo: RepoSlug,
    pub pr_number: Option<u64>,
    pub step: String,
    /// A JSON object.
    pub data: Value,
    /// `None` while the session is still running.
    pub outcome: Option<String>,
    pub started_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub id: i64,
    pub repo: RepoSlug,
    pub kind: String,
    pub payload: Value,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flake {
    pub id: i64,
    pub repo: RepoSlug,
    pub run_id: u64,
    pub job: String,
    pub attempt: u32,
    pub at: String,
}

#[derive(Debug, Clone)]
pub struct NewShipSession {
    pub repo: RepoSlug,
    pub pr_number: Option<u64>,
    pub step: String,
    /// A JSON object; `None` stores `{}`.
    pub data: Option<Value>,
}

impl NewShipSession {
    pub fn new(repo: impl Into<RepoSlug>, step: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            pr_number: None,
            step: step.into(),
            data: None,
        }
    }
}

/// Fields left `None` keep their stored value; the doubly-wrapped ones can be cleared with `Some(None)`.
#[derive(Debug, Clone, Default)]
pub struct ShipSessionPatch {
    pub pr_number: Option<Option<u64>>,
    pub step: Option<String>,
    pub data: Option<Value>,
    pub outcome: Option<Option<String>>,
}

const MIGRATIONS: [&str; 1] = ["CREATE TABLE ship_sessions (
     id TEXT PRIMARY KEY,
     repo TEXT NOT NULL,
     pr_number INTEGER,
     step TEXT NOT NULL,
     data TEXT NOT NULL DEFAULT '{}',
     outcome TEXT,
     started_at TEXT NOT NULL,
     updated_at TEXT NOT NULL
   );
   CREATE INDEX ship_sessions_repo ON ship_sessions (repo, outcome);
   CREATE TABLE events (
     id INTEGER PRIMARY KEY AUTOINCREMENT,
     repo TEXT NOT NULL,
     kind TEXT NOT NULL,
     payload TEXT NOT NULL DEFAULT '{}',
     at TEXT NOT NULL
   );
   CREATE INDEX events_repo ON events (repo, id);
   CREATE TABLE flakes (
     id INTEGER PRIMARY KEY AUTOINCREMENT,
     repo TEXT NOT NULL,
     run_id INTEGER NOT NULL,
     job TEXT NOT NULL,
     attempt INTEGER NOT NULL,
     at TEXT NOT NULL
   );
   CREATE INDEX flakes_repo ON flakes (repo, id);"];

fn session_of(r: &Row<'_>) -> rusqlite::Result<(ShipSession, String)> {
    let data: String = r.get("data")?;
    Ok((
        ShipSession {
            id: r.get("id")?,
            repo: r.get("repo")?,
            pr_number: r.get::<_, Option<i64>>("pr_number")?.map(|n| n as u64),
            step: r.get("step")?,
            data: Value::Null,
            outcome: r.get("outcome")?,
            started_at: r.get("started_at")?,
            updated_at: r.get("updated_at")?,
        },
        data,
    ))
}

fn finish_session((mut s, data): (ShipSession, String)) -> Result<ShipSession, StateError> {
    s.data = serde_json::from_str(&data)?;
    Ok(s)
}

type NowFn = Box<dyn Fn() -> DateTime<Utc> + Send + Sync>;

fn stamp(now: &NowFn) -> String {
    now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    // A pre-existing directory keeps its old mode, so tighten it explicitly.
    set_mode(dir, 0o700)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

fn migrate(conn: &mut Connection, now: &NowFn) -> Result<(), StateError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM migrations",
        [],
        |r| r.get(0),
    )?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO migrations (version, applied_at) VALUES (?1, ?2)",
            params![i as i64 + 1, stamp(now)],
        )?;
        tx.commit()?;
    }
    Ok(())
}

/// SQLite store shared by `groundd` and `ground ship`, which open the same file
/// from separate processes (hence WAL and a 5 s busy timeout). The connection
/// sits behind a mutex, so the store is `Sync`.
pub struct StateStore {
    conn: Mutex<Connection>,
    now: NowFn,
}

impl StateStore {
    /// `path` may be `":memory:"`. Any other path gets its directory created
    /// with mode 0700 and the database file mode 0600.
    pub fn new<C: Clock + Clone>(clock: C, path: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = path.as_ref();
        let in_memory = path == Path::new(":memory:");
        if !in_memory && let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_private_dir(dir)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        if !in_memory {
            // Returns the new mode as a row, so it is a query, not an execute.
            conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
            set_mode(path, 0o600)?;
        }
        let now: NowFn = Box::new(move || clock.now());
        migrate(&mut conn, &now)?;
        Ok(Self {
            conn: Mutex::new(conn),
            now,
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave the connection half-used.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn stamp(&self) -> String {
        stamp(&self.now)
    }

    pub fn create_ship_session(&self, input: NewShipSession) -> Result<ShipSession, StateError> {
        let id = uuid::Uuid::new_v4().to_string();
        let at = self.stamp();
        let data = input.data.unwrap_or_else(|| json!({}));
        self.conn().execute(
            "INSERT INTO ship_sessions (id, repo, pr_number, step, data, started_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, input.repo, input.pr_number.map(|n| n as i64), input.step, data.to_string(), at, at],
        )?;
        self.get_ship_session(&id)?.ok_or(StateError::NoSession(id))
    }

    pub fn get_ship_session(&self, id: &str) -> Result<Option<ShipSession>, StateError> {
        let row = self
            .conn()
            .query_row(
                "SELECT * FROM ship_sessions WHERE id = ?1",
                [id],
                session_of,
            )
            .optional()?;
        row.map(finish_session).transpose()
    }

    pub fn update_ship_session(
        &self,
        id: &str,
        patch: ShipSessionPatch,
    ) -> Result<ShipSession, StateError> {
        let current = self
            .get_ship_session(id)?
            .ok_or_else(|| StateError::NoSession(id.to_owned()))?;
        let pr_number = patch.pr_number.unwrap_or(current.pr_number);
        let step = patch.step.unwrap_or(current.step);
        let data = patch.data.unwrap_or(current.data);
        let outcome = patch.outcome.unwrap_or(current.outcome);
        self.conn().execute(
            "UPDATE ship_sessions SET pr_number = ?1, step = ?2, data = ?3, outcome = ?4, updated_at = ?5 WHERE id = ?6",
            params![pr_number.map(|n| n as i64), step, data.to_string(), outcome, self.stamp(), id],
        )?;
        self.get_ship_session(id)?
            .ok_or_else(|| StateError::NoSession(id.to_owned()))
    }

    /// The newest session of the repo that has no outcome yet, i.e. one `ship` can resume.
    pub fn get_active_ship_session(&self, repo: &str) -> Result<Option<ShipSession>, StateError> {
        let row = self
            .conn()
            .query_row(
                "SELECT * FROM ship_sessions WHERE repo = ?1 AND outcome IS NULL ORDER BY started_at DESC, rowid DESC LIMIT 1",
                [repo],
                session_of,
            )
            .optional()?;
        row.map(finish_session).transpose()
    }

    pub fn record_event(
        &self,
        repo: &str,
        kind: &str,
        payload: Option<Value>,
    ) -> Result<StoredEvent, StateError> {
        let at = self.stamp();
        let payload = payload.unwrap_or_else(|| json!({}));
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (repo, kind, payload, at) VALUES (?1, ?2, ?3, ?4)",
            params![repo, kind, payload.to_string(), at],
        )?;
        Ok(StoredEvent {
            id: conn.last_insert_rowid(),
            repo: repo.to_owned(),
            kind: kind.to_owned(),
            payload,
            at,
        })
    }

    /// Newest first. The TypeScript default limit was 50; pass it explicitly.
    pub fn list_events(&self, repo: &str, limit: u32) -> Result<Vec<StoredEvent>, StateError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM events WHERE repo = ?1 ORDER BY id DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![repo, limit], |r| {
            Ok((
                StoredEvent {
                    id: r.get("id")?,
                    repo: r.get("repo")?,
                    kind: r.get("kind")?,
                    payload: Value::Null,
                    at: r.get("at")?,
                },
                r.get::<_, String>("payload")?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (mut e, payload) = row?;
            e.payload = serde_json::from_str(&payload)?;
            out.push(e);
        }
        Ok(out)
    }

    pub fn record_flake(
        &self,
        repo: &str,
        run_id: u64,
        job: &str,
        attempt: u32,
    ) -> Result<Flake, StateError> {
        let at = self.stamp();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO flakes (repo, run_id, job, attempt, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo, run_id as i64, job, attempt, at],
        )?;
        Ok(Flake {
            id: conn.last_insert_rowid(),
            repo: repo.to_owned(),
            run_id,
            job: job.to_owned(),
            attempt,
            at,
        })
    }

    /// Newest first.
    pub fn list_flakes(&self, repo: &str) -> Result<Vec<Flake>, StateError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM flakes WHERE repo = ?1 ORDER BY id DESC")?;
        let rows = stmt.query_map([repo], |r| {
            Ok(Flake {
                id: r.get("id")?,
                repo: r.get("repo")?,
                run_id: r.get::<_, i64>("run_id")? as u64,
                job: r.get("job")?,
                attempt: r.get("attempt")?,
                at: r.get("at")?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeClock;
    use serde_json::json;

    const REPO: &str = "cogitave/namzu";

    fn setup() -> (FakeClock, StateStore) {
        let clock = FakeClock::new("2026-01-01T00:00:00Z");
        let store = StateStore::new(clock.clone(), ":memory:").unwrap();
        (clock, store)
    }

    #[tokio::test]
    async fn creates_updates_and_finds_the_active_session() {
        let (clock, store) = setup();
        assert!(store.get_active_ship_session(REPO).unwrap().is_none());
        let s = store
            .create_ship_session(NewShipSession::new(REPO, "find-pr"))
            .unwrap();
        assert_eq!(s.repo, REPO);
        assert_eq!(s.step, "find-pr");
        assert_eq!(
            (s.pr_number, s.outcome.as_deref(), &s.data),
            (None, None, &json!({}))
        );
        assert_eq!(s.started_at, "2026-01-01T00:00:00.000Z");

        clock.sleep(1000).await;
        let u = store
            .update_ship_session(
                &s.id,
                ShipSessionPatch {
                    step: Some("merge".into()),
                    pr_number: Some(Some(5)),
                    data: Some(json!({"sha": "x"})),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (u.step.as_str(), u.pr_number, &u.data),
            ("merge", Some(5), &json!({"sha": "x"}))
        );
        assert_eq!(u.updated_at, "2026-01-01T00:00:01.000Z");
        assert_eq!(u.started_at, s.started_at);
        assert_eq!(
            store.get_active_ship_session(REPO).unwrap().unwrap().id,
            s.id
        );
    }

    #[test]
    fn stops_being_active_once_it_has_an_outcome_and_is_per_repo() {
        let (_, store) = setup();
        let s = store
            .create_ship_session(NewShipSession::new(REPO, "summary"))
            .unwrap();
        store
            .create_ship_session(NewShipSession::new("cogitave/other", "find-pr"))
            .unwrap();
        store
            .update_ship_session(
                &s.id,
                ShipSessionPatch {
                    outcome: Some(Some("shipped".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.get_active_ship_session(REPO).unwrap().is_none());
        assert_eq!(
            store
                .get_ship_session(&s.id)
                .unwrap()
                .unwrap()
                .outcome
                .as_deref(),
            Some("shipped")
        );
        assert!(
            store
                .get_active_ship_session("cogitave/other")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_patch_can_clear_the_outcome_and_the_pr_number() {
        let (_, store) = setup();
        let mut input = NewShipSession::new(REPO, "a");
        input.pr_number = Some(3);
        let s = store.create_ship_session(input).unwrap();
        store
            .update_ship_session(
                &s.id,
                ShipSessionPatch {
                    outcome: Some(Some("x".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let u = store
            .update_ship_session(
                &s.id,
                ShipSessionPatch {
                    outcome: Some(None),
                    pr_number: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!((u.outcome, u.pr_number), (None, None));
    }

    #[test]
    fn returns_the_newest_active_session() {
        let (_, store) = setup();
        store
            .create_ship_session(NewShipSession::new(REPO, "a"))
            .unwrap();
        let b = store
            .create_ship_session(NewShipSession::new(REPO, "b"))
            .unwrap();
        assert_eq!(
            store.get_active_ship_session(REPO).unwrap().unwrap().id,
            b.id
        );
    }

    #[test]
    fn rejects_updates_to_unknown_sessions() {
        let (_, store) = setup();
        let err = store
            .update_ship_session("nope", ShipSessionPatch::default())
            .unwrap_err();
        assert!(err.to_string().contains("no ship session"));
    }

    #[test]
    fn lists_events_newest_first_honouring_the_limit() {
        let (_, store) = setup();
        store
            .record_event(REPO, "a", Some(json!({"n": 1})))
            .unwrap();
        store.record_event(REPO, "b", None).unwrap();
        store.record_event("cogitave/other", "c", None).unwrap();
        let kinds = |limit| -> Vec<String> {
            store
                .list_events(REPO, limit)
                .unwrap()
                .into_iter()
                .map(|e| e.kind)
                .collect()
        };
        assert_eq!(kinds(50), ["b", "a"]);
        assert_eq!(kinds(1), ["b"]);
        assert_eq!(
            store.list_events(REPO, 50).unwrap()[1].payload,
            json!({"n": 1})
        );
    }

    #[test]
    fn records_flakes_per_repo() {
        let (_, store) = setup();
        store.record_flake(REPO, 7, "test", 1).unwrap();
        store.record_flake("cogitave/other", 8, "lint", 1).unwrap();
        let flakes = store.list_flakes(REPO).unwrap();
        assert_eq!(flakes.len(), 1);
        assert_eq!(
            (flakes[0].run_id, flakes[0].job.as_str(), flakes[0].attempt),
            (7, "test", 1)
        );
    }

    #[cfg(unix)]
    #[test]
    fn on_disk_store_is_private_migrates_once_and_persists_across_opens() {
        use std::os::unix::fs::PermissionsExt;
        let clock = FakeClock::default();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nested").join("state.db");
        let first = StateStore::new(clock.clone(), &path).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
        first.record_event(REPO, "kept", None).unwrap();
        drop(first);
        let second = StateStore::new(clock, &path).unwrap();
        let kinds: Vec<_> = second
            .list_events(REPO, 50)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["kept"]);
        let versions: i64 = second
            .conn()
            .query_row("SELECT COUNT(*) FROM migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 1);
        let journal: String = second
            .conn()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
    }

    #[cfg(unix)]
    #[test]
    fn tightens_a_pre_existing_loose_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        StateStore::new(FakeClock::default(), root.path().join("state.db")).unwrap();
        let mode = std::fs::metadata(root.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
