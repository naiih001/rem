use rusqlite::{Connection, Row};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Session {
    pub id: String,
    pub project_root: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub model: String,
    pub messages_json: String,
}

fn row_to_session(row: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        project_root: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        model: row.get(5)?,
        messages_json: row.get(6)?,
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn gen_id(now: i64) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    (now, std::process::id(), std::thread::current().id()).hash(&mut h);
    // Mix in nanos for extra uniqueness.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos as u64).hash(&mut h);
    format!("{:016x}{:08x}", h.finish(), (now & 0xffff_ffff) as u64)
}

pub fn db_path() -> Result<PathBuf, String> {
    let data_dir = dirs::data_dir().ok_or("could not determine platform data directory")?;
    let path = data_dir.join("rem").join("sessions.db");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    Ok(path)
}

pub fn open() -> Result<Connection, String> {
    let path = db_path()?;
    let conn =
        Connection::open(&path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, project_root TEXT NOT NULL, title TEXT NOT NULL, created_at INTEGER, updated_at INTEGER, model TEXT, messages_json TEXT NOT NULL)",
        [],
    )
    .map_err(|e| format!("failed to init schema: {e}"))?;
    Ok(conn)
}

pub fn create_session(
    conn: &Connection,
    project_root: &str,
    model: &str,
) -> Result<Session, String> {
    let ts = now();
    let s = Session {
        id: gen_id(ts),
        project_root: project_root.to_string(),
        title: "New session".to_string(),
        created_at: ts,
        updated_at: ts,
        model: model.to_string(),
        messages_json: "[]".to_string(),
    };
    conn.execute(
        "INSERT INTO sessions (id, project_root, title, created_at, updated_at, model, messages_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![s.id, s.project_root, s.title, s.created_at, s.updated_at, s.model, s.messages_json],
    )
    .map_err(|e| format!("create_session: {e}"))?;
    Ok(s)
}

pub fn get_session(conn: &Connection, id: &str) -> Result<Option<Session>, String> {
    let mut stmt = conn
        .prepare("SELECT id, project_root, title, created_at, updated_at, model, messages_json FROM sessions WHERE id = ?1")
        .map_err(|e| format!("get_session: {e}"))?;
    let mut rows = stmt
        .query_map([id], row_to_session)
        .map_err(|e| format!("get_session: {e}"))?;
    match rows.next() {
        None => Ok(None),
        Some(r) => r.map(Some).map_err(|e| format!("get_session: {e}")),
    }
}

pub fn list_for_project(
    conn: &Connection,
    project_root: &str,
) -> Result<Vec<Session>, String> {
    let mut stmt = conn
        .prepare("SELECT id, project_root, title, created_at, updated_at, model, messages_json FROM sessions WHERE project_root = ?1 ORDER BY updated_at DESC")
        .map_err(|e| format!("list_for_project: {e}"))?;
    stmt.query_map([project_root], row_to_session)
        .map_err(|e| format!("list_for_project: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("list_for_project: {e}"))
}

pub fn list_all(conn: &Connection) -> Result<Vec<Session>, String> {
    let mut stmt = conn
        .prepare("SELECT id, project_root, title, created_at, updated_at, model, messages_json FROM sessions ORDER BY updated_at DESC")
        .map_err(|e| format!("list_all: {e}"))?;
    stmt.query_map([], row_to_session)
        .map_err(|e| format!("list_all: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("list_all: {e}"))
}

pub fn save_messages(
    conn: &Connection,
    id: &str,
    messages_json: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE sessions SET messages_json = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![messages_json, now(), id],
    )
    .map(|_| ())
    .map_err(|e| format!("save_messages: {e}"))
}

pub fn update_title(conn: &Connection, id: &str, title: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![title, now(), id],
    )
    .map(|_| ())
    .map_err(|e| format!("update_title: {e}"))
}

pub fn touch(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now(), id],
    )
    .map(|_| ())
    .map_err(|e| format!("touch: {e}"))
}
