use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags};
use uuid::Uuid;

use crate::codex_config::resolve_codex_home;

/// The provider id Neko Route injects into the Codex config.
const TARGET_PROVIDER: &str = "neko-route";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    /// Total `.jsonl` session files scanned.
    pub scanned: usize,
    /// Files re-tagged to the target provider.
    pub imported: usize,
    /// Files already tagged with the target provider (left untouched).
    pub already: usize,
    /// Files with no recognizable provider tag on line 1 (left untouched).
    pub skipped: usize,
    /// Count of imported sessions grouped by their previous provider.
    pub by_previous: BTreeMap<String, usize>,
    /// Total Codex thread index rows scanned from state_5.sqlite.
    pub sqlite_scanned: usize,
    /// Thread index rows re-tagged to the target provider.
    pub sqlite_updated: usize,
    /// Thread index rows already tagged with the target provider.
    pub sqlite_already: usize,
    /// Thread index rows whose provider differed from the JSONL session metadata.
    pub sqlite_mismatched: usize,
    /// Backup directory containing the SQLite backup and import manifest.
    pub backup_path: Option<String>,
    pub codex_home: String,
}

#[derive(Debug, Clone)]
struct SessionMeta {
    id: Option<String>,
    provider: String,
}

#[derive(Debug, Serialize)]
struct ImportBackupManifest {
    created_at: String,
    codex_home: String,
    target_provider: String,
    sqlite_backup: Option<String>,
    jsonl_files: Vec<JsonlBackupEntry>,
    sqlite_threads: Vec<SqliteBackupEntry>,
}

#[derive(Debug, Serialize)]
struct JsonlBackupEntry {
    path: String,
    id: Option<String>,
    previous_provider: String,
}

#[derive(Debug, Serialize)]
struct SqliteBackupEntry {
    id: String,
    rollout_path: String,
    previous_provider: String,
}

/// Re-tag every legacy Codex session so it resumes under the Neko Route
/// provider. Sessions are JSONL; line 1 is the `session_meta` record carrying
/// `"model_provider":"<id>"`. We rewrite only that field and stream the rest of
/// the (potentially huge) file unchanged.
pub fn import_sessions() -> Result<ImportResult, String> {
    import_sessions_from_home(&resolve_codex_home())
}

fn import_sessions_from_home(home: &Path) -> Result<ImportResult, String> {
    let backup_dir = create_backup_dir(home)?;
    let sqlite_backup = backup_sqlite(home, &backup_dir)?;

    let mut roots = Vec::new();
    for name in ["sessions", "archived_sessions"] {
        let dir = home.join(name);
        if dir.is_dir() {
            roots.push(dir);
        }
    }

    let mut files = Vec::new();
    for root in &roots {
        collect_jsonl(root, &mut files);
    }

    let mut result = ImportResult {
        scanned: 0,
        imported: 0,
        already: 0,
        skipped: 0,
        by_previous: BTreeMap::new(),
        sqlite_scanned: 0,
        sqlite_updated: 0,
        sqlite_already: 0,
        sqlite_mismatched: 0,
        backup_path: Some(backup_dir.display().to_string()),
        codex_home: home.display().to_string(),
    };
    let mut sessions_by_path = HashMap::new();
    let mut sessions_by_id = HashMap::new();
    let mut jsonl_backups = Vec::new();

    for file in files {
        result.scanned += 1;
        match retag_file(&file) {
            Ok(Retag::Imported { previous, meta }) => {
                result.imported += 1;
                *result.by_previous.entry(previous.clone()).or_insert(0) += 1;
                jsonl_backups.push(JsonlBackupEntry {
                    path: file.display().to_string(),
                    id: meta.id.clone(),
                    previous_provider: previous,
                });
                remember_session(&mut sessions_by_path, &mut sessions_by_id, &file, meta);
            }
            Ok(Retag::Already(meta)) => {
                result.already += 1;
                remember_session(&mut sessions_by_path, &mut sessions_by_id, &file, meta);
            }
            Ok(Retag::NoTag) => result.skipped += 1,
            // A single unreadable/locked file must not abort the whole import.
            Err(_) => result.skipped += 1,
        }
    }

    let sqlite_threads =
        sync_sqlite_threads(home, &sessions_by_path, &sessions_by_id, &mut result)?;
    let manifest = ImportBackupManifest {
        created_at: Utc::now().to_rfc3339(),
        codex_home: home.display().to_string(),
        target_provider: TARGET_PROVIDER.into(),
        sqlite_backup: sqlite_backup.map(|path| path.display().to_string()),
        jsonl_files: jsonl_backups,
        sqlite_threads,
    };
    write_backup_manifest(&backup_dir, &manifest)?;

    Ok(result)
}

enum Retag {
    Imported { previous: String, meta: SessionMeta },
    Already(SessionMeta),
    NoTag,
}

fn retag_file(path: &Path) -> Result<Retag, String> {
    let input = File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(input);

    let mut first_line = String::new();
    let read = reader
        .read_line(&mut first_line)
        .map_err(|e| e.to_string())?;
    if read == 0 {
        return Ok(Retag::NoTag);
    }

    let Some(mut meta) = extract_session_meta(&first_line) else {
        return Ok(Retag::NoTag);
    };
    let previous = meta.provider.clone();
    if previous == TARGET_PROVIDER {
        return Ok(Retag::Already(meta));
    }

    let rewritten = replace_provider(&first_line, &previous, TARGET_PROVIDER);
    meta.provider = TARGET_PROVIDER.into();

    // Write line 1 (rewritten) + stream the remaining bytes verbatim to a temp
    // file, then atomically swap it into place.
    let tmp_path = temp_sibling(path);
    {
        let out = File::create(&tmp_path).map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(out);
        writer
            .write_all(rewritten.as_bytes())
            .map_err(|e| e.to_string())?;
        std::io::copy(&mut reader, &mut writer).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        e.to_string()
    })?;

    Ok(Retag::Imported { previous, meta })
}

fn remember_session(
    sessions_by_path: &mut HashMap<String, SessionMeta>,
    sessions_by_id: &mut HashMap<String, SessionMeta>,
    path: &Path,
    meta: SessionMeta,
) {
    sessions_by_path.insert(path.display().to_string(), meta.clone());
    if let Some(id) = &meta.id {
        sessions_by_id.insert(id.clone(), meta);
    }
}

/// Pull the `model_provider` string value out of a `session_meta` JSON line
/// without fully parsing the (large) line.
fn extract_provider(line: &str) -> Option<String> {
    extract_string_field(line, "\"model_provider\"")
}

fn extract_session_meta(line: &str) -> Option<SessionMeta> {
    Some(SessionMeta {
        id: extract_string_field(line, "\"id\""),
        provider: extract_provider(line)?,
    })
}

fn extract_string_field(line: &str, key: &str) -> Option<String> {
    let start = line.find(key)? + key.len();
    let rest = line[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Replace only the first `"model_provider":"<prev>"` occurrence, preserving
/// any whitespace between the colon and the value.
fn replace_provider(line: &str, previous: &str, target: &str) -> String {
    let key = "\"model_provider\"";
    let Some(key_at) = line.find(key) else {
        return line.to_string();
    };
    let after_key = key_at + key.len();
    // Locate the opening quote of the value after the colon.
    let value_quote_rel = line[after_key..].find('"');
    let Some(open_rel) = value_quote_rel else {
        return line.to_string();
    };
    let open = after_key + open_rel; // index of opening quote
    let val_start = open + 1;
    let Some(close_rel) = line[val_start..].find('"') else {
        return line.to_string();
    };
    let close = val_start + close_rel; // index of closing quote
    debug_assert_eq!(&line[val_start..close], previous);
    let _ = previous;
    let mut out = String::with_capacity(line.len() - (close - val_start) + target.len());
    out.push_str(&line[..val_start]);
    out.push_str(target);
    out.push_str(&line[close..]);
    out
}

fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".neko-import.tmp");
    path.with_file_name(name)
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

fn create_backup_dir(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join("config-backups").join(format!(
        "neko-route-session-import-{}-{}",
        Utc::now().timestamp_millis(),
        Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn backup_sqlite(home: &Path, backup_dir: &Path) -> Result<Option<PathBuf>, String> {
    let db_path = home.join("state_5.sqlite");
    if !db_path.exists() {
        return Ok(None);
    }
    let backup_path = backup_dir.join("state_5.sqlite");
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    let escaped = backup_path.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))
        .map_err(|error| error.to_string())?;
    Ok(Some(backup_path))
}

fn sync_sqlite_threads(
    home: &Path,
    sessions_by_path: &HashMap<String, SessionMeta>,
    sessions_by_id: &HashMap<String, SessionMeta>,
    result: &mut ImportResult,
) -> Result<Vec<SqliteBackupEntry>, String> {
    let db_path = home.join("state_5.sqlite");
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let mut conn = Connection::open(&db_path).map_err(|error| error.to_string())?;
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    let rows = {
        let mut stmt = tx
            .prepare("SELECT id, rollout_path, model_provider FROM threads")
            .map_err(|error| error.to_string())?;
        let mapped = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };

    let mut backups = Vec::new();
    for (id, rollout_path, provider) in rows {
        result.sqlite_scanned += 1;
        if provider == TARGET_PROVIDER {
            result.sqlite_already += 1;
            continue;
        }

        let meta = sessions_by_path
            .get(&rollout_path)
            .or_else(|| sessions_by_id.get(&id));
        let Some(meta) = meta else {
            continue;
        };

        if meta.provider != provider {
            result.sqlite_mismatched += 1;
        }
        if meta.provider != TARGET_PROVIDER {
            continue;
        }

        let changed = tx
            .execute(
                "UPDATE threads SET model_provider = ?1 WHERE id = ?2 AND model_provider = ?3",
                params![TARGET_PROVIDER, id, provider],
            )
            .map_err(|error| error.to_string())?;
        if changed > 0 {
            result.sqlite_updated += changed;
            backups.push(SqliteBackupEntry {
                id,
                rollout_path,
                previous_provider: provider,
            });
        }
    }
    tx.commit().map_err(|error| error.to_string())?;
    Ok(backups)
}

fn write_backup_manifest(backup_dir: &Path, manifest: &ImportBackupManifest) -> Result<(), String> {
    let content = serde_json::to_string_pretty(manifest).map_err(|error| error.to_string())?;
    fs::write(backup_dir.join("manifest.json"), content).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn write_session(dir: &Path, name: &str, id: &str, provider: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            "{{\"timestamp\":\"t\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"{provider}\",\"cwd\":\"/tmp\"}}}}"
        )
        .unwrap();
        writeln!(f, "{{\"type\":\"event_msg\",\"payload\":{{\"k\":\"v\"}}}}").unwrap();
        path
    }

    fn create_threads_db(home: &Path, rows: &[(&str, &Path, &str)]) {
        let conn = Connection::open(home.join("state_5.sqlite")).unwrap();
        conn.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, model_provider TEXT NOT NULL)",
            [],
        )
        .unwrap();
        for (id, path, provider) in rows {
            conn.execute(
                "INSERT INTO threads (id, rollout_path, model_provider) VALUES (?1, ?2, ?3)",
                params![id, path.display().to_string(), provider],
            )
            .unwrap();
        }
    }

    #[test]
    fn extracts_and_replaces_provider() {
        let line = r#"{"type":"session_meta","payload":{"model_provider":"custom","cwd":"/x"}}"#;
        assert_eq!(extract_provider(line).as_deref(), Some("custom"));
        let out = replace_provider(line, "custom", "neko-route");
        assert!(out.contains(r#""model_provider":"neko-route""#));
        assert!(out.contains(r#""cwd":"/x""#));
    }

    #[test]
    fn retags_legacy_session_and_preserves_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), "a.jsonl", "x", "custom");
        let body_before = fs::read_to_string(&path).unwrap();
        assert!(body_before.contains("\"event_msg\""));

        match retag_file(&path).unwrap() {
            Retag::Imported { previous, meta } => {
                assert_eq!(previous, "custom");
                assert_eq!(meta.id.as_deref(), Some("x"));
                assert_eq!(meta.provider, "neko-route");
            }
            _ => panic!("expected import"),
        }
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains(r#""model_provider":"neko-route""#));
        // Second line (body) preserved exactly.
        assert!(after.contains("{\"type\":\"event_msg\",\"payload\":{\"k\":\"v\"}}"));
    }

    #[test]
    fn already_tagged_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), "b.jsonl", "x", "neko-route");
        assert!(matches!(retag_file(&path).unwrap(), Retag::Already(_)));
    }

    #[test]
    fn missing_tag_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"x\"}}}}"
        )
        .unwrap();
        assert!(matches!(retag_file(&path).unwrap(), Retag::NoTag));
    }

    #[test]
    fn collects_nested_and_flat_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("2026/06/19");
        fs::create_dir_all(&nested).unwrap();
        write_session(&nested, "deep.jsonl", "deep", "custom");
        write_session(dir.path(), "flat.jsonl", "flat", "custom");
        File::create(dir.path().join("ignore.txt")).unwrap();
        let mut found = Vec::new();
        collect_jsonl(dir.path(), &mut found);
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn import_updates_sqlite_threads_by_rollout_path() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let path = write_session(&sessions, "a.jsonl", "thread-a", "custom");
        create_threads_db(home.path(), &[("thread-a", &path, "custom")]);

        let result = import_sessions_from_home(home.path()).unwrap();
        let conn = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        let provider: String = conn
            .query_row(
                "SELECT model_provider FROM threads WHERE id='thread-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(result.imported, 1);
        assert_eq!(result.sqlite_scanned, 1);
        assert_eq!(result.sqlite_updated, 1);
        assert_eq!(result.sqlite_mismatched, 1);
        assert_eq!(provider, "neko-route");
        assert!(result.backup_path.as_deref().is_some_and(|path| {
            Path::new(path).join("state_5.sqlite").exists()
                && Path::new(path).join("manifest.json").exists()
        }));
    }

    #[test]
    fn import_updates_sqlite_threads_by_id_when_path_differs() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let path = write_session(&sessions, "a.jsonl", "thread-a", "custom");
        let moved_path = sessions.join("moved.jsonl");
        create_threads_db(home.path(), &[("thread-a", &moved_path, "custom")]);

        let result = import_sessions_from_home(home.path()).unwrap();
        let conn = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        let provider: String = conn
            .query_row(
                "SELECT model_provider FROM threads WHERE id='thread-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(path.exists(), true);
        assert_eq!(result.sqlite_updated, 1);
        assert_eq!(provider, "neko-route");
    }

    #[test]
    fn import_leaves_sqlite_without_confirmed_jsonl_untouched() {
        let home = tempfile::tempdir().unwrap();
        let missing = home.path().join("sessions/missing.jsonl");
        create_threads_db(home.path(), &[("thread-a", &missing, "custom")]);

        let result = import_sessions_from_home(home.path()).unwrap();
        let conn = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        let provider: String = conn
            .query_row(
                "SELECT model_provider FROM threads WHERE id='thread-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(result.sqlite_scanned, 1);
        assert_eq!(result.sqlite_updated, 0);
        assert_eq!(provider, "custom");
    }
}
