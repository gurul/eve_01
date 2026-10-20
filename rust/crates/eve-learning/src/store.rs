//! Transactional lesson storage, including every saved revision of the whiteboard.
//!
//! The same SQLite file, tables and JSON text as Python's `learning/store.py`, so either daemon reads what the
//! other wrote: `lessons.sqlite3` with `lessons(id, updated, data)`, `revisions(lesson_id, revision, data)` and
//! `blobs(lesson_id, hash, data)`, and the widget's `dashboard.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::py::{char_len, dumps};
use crate::LearningError;

/// Lesson fields that can be megabytes (screenshot, flattened board, tldraw document, strokes). A revision stores
/// each one by its hash in `blobs`, so an autosave that changed only the typed ideas does not copy a 6 MB
/// screenshot again: an unchanged value is the same hash, stored once per lesson.
pub const BLOB_FIELDS: [&str; 4] = ["source_image", "board_image", "board", "strokes"];
/// Smaller values stay inline: a row in `blobs` would cost more than it saves.
pub const BLOB_MIN: usize = 4096;
pub const SUMMARY_FIELDS: [&str; 9] = ["id", "topic", "level", "mode", "problem", "stage", "updated", "demo", "revision"];

/// `time.time()`.
pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `EVE_LEARNING_DIR`, default `~/.config/eve-bridge/learning`, with `~` expanded. `env` is the variable's value
/// (`std::env::var_os` in the daemon).
pub fn default_dir_from(env: Option<&str>) -> PathBuf {
    let raw = env.unwrap_or("~/.config/eve-bridge/learning");
    // Path("") is "." in Python.
    let raw = if raw.is_empty() { "." } else { raw };
    eve_envfile::expanduser(Path::new(raw)).unwrap_or_else(|_| PathBuf::from(raw))
}

/// `default_dir()`: the process's `EVE_LEARNING_DIR`.
pub fn default_dir() -> PathBuf {
    default_dir_from(std::env::var("EVE_LEARNING_DIR").ok().as_deref())
}

fn sql(e: rusqlite::Error) -> LearningError {
    LearningError::Other(anyhow::Error::new(e).context("learning store"))
}

fn sql_to_json(v: rusqlite::types::Value) -> Value {
    use rusqlite::types::Value as S;
    match v {
        S::Null => Value::Null,
        S::Integer(i) => json!(i),
        S::Real(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        S::Text(t) => Value::String(t),
        S::Blob(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
    }
}

fn parse(text: &str) -> Result<Value, LearningError> {
    serde_json::from_str(text).map_err(|e| LearningError::Value(e.to_string()))
}

/// The lessons of one folder.
#[derive(Debug, Clone)]
pub struct Store {
    pub directory: PathBuf,
    pub path: PathBuf,
}

impl Store {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Store, LearningError> {
        let directory = directory.into();
        std::fs::create_dir_all(&directory).map_err(|e| LearningError::Other(e.into()))?;
        let store = Store { path: directory.join("lessons.sqlite3"), directory };
        let db = store.connect()?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS lessons (id TEXT PRIMARY KEY, updated REAL, data TEXT);
             CREATE TABLE IF NOT EXISTS revisions (lesson_id TEXT, revision INTEGER, data TEXT, PRIMARY KEY(lesson_id,revision));
             CREATE TABLE IF NOT EXISTS blobs (lesson_id TEXT, hash TEXT, data TEXT, PRIMARY KEY(lesson_id,hash));",
        )
        .map_err(sql)?;
        Ok(store)
    }

    /// `sqlite3.connect(path, timeout=15)`.
    pub fn connect(&self) -> Result<Connection, LearningError> {
        let db = Connection::open(&self.path).map_err(sql)?;
        db.busy_timeout(Duration::from_secs(15)).map_err(sql)?;
        Ok(db)
    }

    /// One lesson. `LearningError::NotFound` ("Lesson not found") when there is none.
    pub fn get(&self, key: &str) -> Result<Map<String, Value>, LearningError> {
        let db = self.connect()?;
        let row: Option<String> =
            db.query_row("SELECT data FROM lessons WHERE id=?", [key], |r| r.get(0)).optional().map_err(sql)?;
        let Some(text) = row else { return Err(LearningError::NotFound("Lesson not found".into())) };
        match parse(&text)? {
            Value::Object(m) => Ok(m),
            _ => Err(LearningError::Type("a saved lesson is not an object".into())),
        }
    }

    /// Every lesson's summary, newest first.
    pub fn list(&self) -> Result<Vec<Map<String, Value>>, LearningError> {
        // SQLite picks the summary fields out of each lesson: parsing every whole lesson (images and boards
        // included) on every save made the dashboard widget the slow part of a save.
        let columns = SUMMARY_FIELDS.iter().map(|k| format!("json_extract(data, '$.{k}')")).collect::<Vec<_>>().join(", ");
        let db = self.connect()?;
        let mut stmt = db.prepare(&format!("SELECT {columns} FROM lessons ORDER BY updated DESC")).map_err(sql)?;
        let rows = stmt
            .query_map([], |r| {
                let mut m = Map::new();
                for (i, k) in SUMMARY_FIELDS.iter().enumerate() {
                    m.insert(k.to_string(), sql_to_json(r.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(m)
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        Ok(rows
            .into_iter()
            .map(|mut m| {
                // JSON true comes back from SQLite as 1.
                let demo = crate::py::truthy(&m["demo"]);
                m.insert("demo".into(), Value::Bool(demo));
                m
            })
            .collect())
    }

    /// The summary fields of a lesson. NotFound (Python's KeyError) when one is missing.
    pub fn summary(s: &Map<String, Value>) -> Result<Map<String, Value>, LearningError> {
        let mut out = Map::new();
        for k in SUMMARY_FIELDS {
            let v = s.get(k).ok_or_else(|| LearningError::NotFound(k.to_string()))?;
            out.insert(k.to_string(), v.clone());
        }
        Ok(out)
    }

    /// Save a lesson as its next revision. With `expected`, refuse when the stored revision moved on.
    pub fn save(&self, lesson: &Map<String, Value>, expected: Option<i64>) -> Result<Map<String, Value>, LearningError> {
        let mut lesson = lesson.clone();
        let id = match lesson.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(_) => return Err(LearningError::Type("lesson id must be text".into())),
            None => return Err(LearningError::NotFound("id".into())),
        };
        let mut db = self.connect()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(sql)?;
        let row: Option<rusqlite::types::Value> = tx
            .query_row("SELECT json_extract(data, '$.revision') FROM lessons WHERE id=?", [&id], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        let revision = match row {
            None => 0,
            Some(rusqlite::types::Value::Integer(i)) => i,
            Some(_) => return Err(LearningError::Type("unsupported operand type(s) for +: 'NoneType' and 'int'".into())),
        };
        if let Some(expected) = expected {
            if revision != expected {
                return Err(LearningError::Value(
                    "This lesson changed in another window. Reload it before continuing.".into(),
                ));
            }
        }
        lesson.insert("revision".into(), json!(revision + 1));
        let updated = now();
        lesson.insert("updated".into(), json!(updated));
        let encoded = dumps(&Value::Object(lesson.clone()), false);
        tx.execute("INSERT OR REPLACE INTO lessons VALUES (?,?,?)", params![id, updated, encoded]).map_err(sql)?;
        let data = Self::revision_data(&tx, &id, &lesson)?;
        tx.execute("INSERT INTO revisions VALUES (?,?,?)", params![id, revision + 1, data]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        drop(db);
        self.write_widget()?;
        Ok(lesson)
    }

    /// A new lesson, saved as revision 1.
    pub fn create(&self, mode: &Value, topic: &str, level: &str, demo: bool) -> Result<Map<String, Value>, LearningError> {
        let mode = match mode.as_str() {
            Some(m @ ("learn" | "help")) => m,
            _ => return Err(LearningError::Value("Choose learn or help.".into())),
        };
        let lesson = json!({"id": uuid::Uuid::new_v4().simple().to_string(), "mode": mode,
                            "topic": if topic.is_empty() { "My problem" } else { topic },
                            "level": level, "demo": demo, "problem": "", "ideas": "", "stuck": false,
                            "strokes": [], "board": null, "source_image": "", "board_image": "", "events": [],
                            "stage": if mode == "learn" { "setup" } else { "input" }, "revision": 0});
        self.save(lesson.as_object().expect("object"), None)
    }

    /// The revision row: the lesson with each large field replaced by {"$blob": hash} into `blobs`.
    fn revision_data(db: &Connection, id: &str, lesson: &Map<String, Value>) -> Result<String, LearningError> {
        let mut revision = lesson.clone();
        for field in BLOB_FIELDS {
            let value = dumps(lesson.get(field).unwrap_or(&Value::Null), false);
            if char_len(&value) < BLOB_MIN {
                continue;
            }
            let digest = hex::encode(Sha256::digest(value.as_bytes()));
            db.execute("INSERT OR IGNORE INTO blobs VALUES (?,?,?)", params![id, digest, value]).map_err(sql)?;
            revision.insert(field.to_string(), json!({"$blob": digest}));
        }
        Ok(dumps(&Value::Object(revision), false))
    }

    /// Every saved revision of a lesson, oldest first, with blob references read back.
    pub fn history(&self, key: &str) -> Result<Vec<Value>, LearningError> {
        self.get(key)?;
        let db = self.connect()?;
        let rows: Vec<String> = db
            .prepare("SELECT data FROM revisions WHERE lesson_id=? ORDER BY revision")
            .map_err(sql)?
            .query_map([key], |r| r.get(0))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        let blobs: HashMap<String, String> = db
            .prepare("SELECT hash, data FROM blobs WHERE lesson_id=?")
            .map_err(sql)?
            .query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        let mut decoded: HashMap<String, Value> = HashMap::new();
        let mut history = Vec::with_capacity(rows.len());
        for text in rows {
            let mut revision = parse(&text)?;
            if let Value::Object(map) = &mut revision {
                for field in BLOB_FIELDS {
                    // Revisions saved before `blobs` existed hold the value inline and are left as they are.
                    let digest = match map.get(field) {
                        Some(Value::Object(r)) if r.len() == 1 => match r.get("$blob") {
                            Some(Value::String(d)) if blobs.contains_key(d) => d.clone(),
                            _ => continue,
                        },
                        _ => continue,
                    };
                    if !decoded.contains_key(&digest) {
                        decoded.insert(digest.clone(), parse(&blobs[&digest])?);
                    }
                    map.insert(field.to_string(), decoded[&digest].clone());
                }
            }
            history.push(revision);
        }
        Ok(history)
    }

    pub fn delete(&self, key: &str) -> Result<(), LearningError> {
        let mut db = self.connect()?;
        let tx = db.transaction().map_err(sql)?;
        tx.execute("DELETE FROM revisions WHERE lesson_id=?", [key]).map_err(sql)?;
        tx.execute("DELETE FROM blobs WHERE lesson_id=?", [key]).map_err(sql)?;
        tx.execute("DELETE FROM lessons WHERE id=?", [key]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        drop(db);
        self.write_widget()
    }

    /// `dashboard.json` for the widget: totals and the five newest summaries, written through a temp file.
    pub fn write_widget(&self) -> Result<(), LearningError> {
        let rows = self.list()?;
        let completed = rows.iter().filter(|s| s.get("stage").and_then(Value::as_str) == Some("complete")).count();
        let snapshot = json!({"total": rows.len(), "completed": completed,
                              "lessons": rows.iter().take(5).cloned().map(Value::Object).collect::<Vec<_>>(),
                              "updated": now()});
        let temporary = self.directory.join("dashboard.tmp");
        std::fs::write(&temporary, dumps(&snapshot, true)).map_err(|e| LearningError::Other(e.into()))?;
        std::fs::rename(&temporary, self.directory.join("dashboard.json")).map_err(|e| LearningError::Other(e.into()))
    }
}
