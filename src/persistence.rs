//! Session persistence: save/load session metadata and history to disk.
//!
//! All path functions are parameterized with `base_path` so consumers
//! can specify their own storage directory (e.g., `~/.terminar/` or custom).

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::constants::{HISTORY_SUBDIR, SESSION_METADATA_FILE};
use crate::history;
use crate::session::SessionState;

/// Metadata for a persisted session, serialized to/from JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersistedSession {
    pub id: String,
    pub name: String,
    pub shell_cmd: String,
    pub cwd: String,
    pub pid: Option<u32>,
    pub state: String,
}

/// Container for all persisted sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PersistedSessionData {
    pub sessions: Vec<PersistedSession>,
}

impl PersistedSessionData {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Returns the path to the session history directory under `base_path`.
pub fn get_history_dir(base_path: &Path) -> PathBuf {
    base_path.join(HISTORY_SUBDIR)
}

/// Returns the path to the session metadata file under `base_path`.
pub fn get_session_file_path(base_path: &Path) -> PathBuf {
    base_path.join(SESSION_METADATA_FILE)
}

/// Atomically write data to a file using temp file + rename.
/// This prevents corruption if the process crashes mid-write.
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("No parent directory for {:?}", path))?;
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory {:?}: {}", parent, e))?;
    }
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("Failed to create temp file in {:?}: {}", parent, e))?;
    tmp.write_all(data)
        .map_err(|e| format!("Failed to write temp file: {}", e))?;
    tmp.flush()
        .map_err(|e| format!("Failed to flush temp file: {}", e))?;
    tmp.persist(path)
        .map_err(|e| format!("Failed to rename temp file to {:?}: {}", path, e))?;
    Ok(())
}

/// Save session data to a JSON file.
/// Creates parent directories if needed. Uses atomic write (temp + rename).
pub fn save_sessions(path: &str, data: &PersistedSessionData) -> Result<(), String> {
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| format!("Failed to serialize sessions: {}", e))?;
    atomic_write(Path::new(path), json.as_bytes())
}

/// Build PersistedSessionData from a SessionMap by extracting metadata.
pub fn build_persisted_data(sessions: &crate::session::SessionMap) -> PersistedSessionData {
    let guard = sessions.lock();
    let sessions_vec = guard
        .values()
        .map(|s| PersistedSession {
            id: s.id.clone(),
            name: s.name.clone(),
            shell_cmd: s.shell_cmd.clone(),
            cwd: s.cwd.clone(),
            pid: None,
            state: format!("{:?}", s.state),
        })
        .collect();
    PersistedSessionData {
        sessions: sessions_vec,
    }
}

/// Load session data from a JSON file.
/// Returns empty data if file doesn't exist.
/// Returns error only if file exists but is corrupt/unreadable.
pub fn load_sessions(path: &str) -> Result<PersistedSessionData, String> {
    let p = Path::new(path);
    if !p.exists() {
        return Ok(PersistedSessionData::new());
    }
    let contents = std::fs::read_to_string(p)
        .map_err(|e| format!("Failed to read session file {:?}: {}", path, e))?;
    serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse session file {:?}: {}", path, e))
}

/// Returns the path for a session's history file.
pub fn history_file_path(base_dir: &str, session_id: &str) -> PathBuf {
    Path::new(base_dir).join(format!("{}.history", session_id))
}

/// Save history data to disk for a session.
/// Creates the directory if it doesn't exist. Uses atomic write (temp + rename).
pub fn save_history(base_dir: &str, session_id: &str, data: &[u8]) -> Result<(), String> {
    let path = history_file_path(base_dir, session_id);
    atomic_write(&path, data)
}

/// Load history data from disk for a session.
/// Returns None if the file doesn't exist.
pub fn load_history(base_dir: &str, session_id: &str) -> Result<Option<Vec<u8>>, String> {
    let path = history_file_path(base_dir, session_id);
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read(&path)
        .map(Some)
        .map_err(|e| format!("Failed to read history file {:?}: {}", path, e))
}

/// Delete a session's history file from disk.
/// No-op if the file doesn't exist.
pub fn delete_history(base_path: &Path, session_id: &str) {
    let dir = get_history_dir(base_path);
    let path = dir.join(format!("{}.history", session_id));
    if path.exists()
        && let Err(e) = std::fs::remove_file(&path)
    {
        tracing::warn!("Failed to delete history file {:?}: {}", path, e);
    }
}

/// Zstd magic bytes: 0x28 0xB5 0x2F 0xFD
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Load history data with automatic zstd decompression detection.
/// If the data starts with the zstd magic bytes, it is decompressed.
/// Otherwise it is returned as-is (raw terminal output).
pub fn load_history_auto(base_dir: &str, session_id: &str) -> Result<Option<Vec<u8>>, String> {
    match load_history(base_dir, session_id)? {
        None => Ok(None),
        Some(data) => {
            if data.len() >= 4 && data[..4] == ZSTD_MAGIC {
                let decompressed = history::decompress_history(&data).map_err(|e| {
                    format!("Failed to decompress history for {}: {}", session_id, e)
                })?;
                Ok(Some(decompressed))
            } else {
                Ok(Some(data))
            }
        }
    }
}

/// Save all session histories to disk.
///
/// Snapshots each session's history bytes under the sessions lock (brief),
/// then releases the lock before doing file I/O (compression + atomic write).
/// Only saves Running sessions with non-empty history.
pub fn save_all_histories(base_path: &Path, sessions: &crate::session::SessionMap) {
    // Snapshot history data under the lock (fast)
    let snapshots: Vec<(String, Vec<u8>)> = {
        let guard = sessions.lock();
        guard
            .values()
            .filter(|s| matches!(s.state, SessionState::Running))
            .filter_map(|s| {
                let h = s.history.lock();
                if h.is_empty() {
                    None
                } else {
                    Some((s.id.clone(), h.to_vec()))
                }
            })
            .collect()
    };

    let history_dir = get_history_dir(base_path);
    let base_dir = history_dir.to_string_lossy();

    for (session_id, data) in &snapshots {
        // Compress if above threshold
        let to_write = if history::should_compress(data.len()) {
            match history::compress_history(data) {
                Ok(compressed) => compressed,
                Err(e) => {
                    tracing::warn!(
                        "Failed to compress history for {}: {}, saving uncompressed",
                        session_id,
                        e
                    );
                    data.clone()
                }
            }
        } else {
            data.clone()
        };

        if let Err(e) = save_history(&base_dir, session_id, &to_write) {
            tracing::warn!("Failed to save history for session {}: {}", session_id, e);
        }
    }
}

/// Persist current session metadata and histories to disk.
pub fn persist_all(base_path: &Path, sessions: &crate::session::SessionMap) {
    // Save metadata
    let data = build_persisted_data(sessions);
    let path = get_session_file_path(base_path);
    if let Err(e) = save_sessions(&path.to_string_lossy(), &data) {
        tracing::warn!("Failed to persist session metadata: {}", e);
    }
    // Save histories
    save_all_histories(base_path, sessions);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persisted_session_serializes_to_json() {
        let session = PersistedSession {
            id: "abc-123".to_string(),
            name: "Terminal".to_string(),
            shell_cmd: "/bin/bash".to_string(),
            cwd: "/home/user".to_string(),
            pid: Some(1234),
            state: "Running".to_string(),
        };
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("abc-123"));
        assert!(json.contains("/bin/bash"));
    }

    #[test]
    fn test_persisted_session_deserializes_from_json() {
        let json =
            r#"{"id":"abc","name":"T","shell_cmd":"/bin/sh","cwd":"/","pid":42,"state":"Running"}"#;
        let session: PersistedSession = serde_json::from_str(json).unwrap();
        assert_eq!(session.id, "abc");
        assert_eq!(session.pid, Some(42));
    }

    #[test]
    fn test_save_sessions_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let path_str = path.to_str().unwrap();

        let data = PersistedSessionData { sessions: vec![] };
        save_sessions(path_str, &data).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn test_load_sessions_missing_file_returns_empty() {
        let result = load_sessions("/tmp/nonexistent-test-file-12345.json");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().sessions.len(), 0);
    }

    #[test]
    fn test_save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.json");
        let path_str = path.to_str().unwrap();

        let data = PersistedSessionData {
            sessions: vec![PersistedSession {
                id: "r1".to_string(),
                name: "R1".to_string(),
                shell_cmd: "/bin/bash".to_string(),
                cwd: "/tmp".to_string(),
                pid: Some(111),
                state: "Running".to_string(),
            }],
        };

        save_sessions(path_str, &data).unwrap();
        let loaded = load_sessions(path_str).unwrap();
        assert_eq!(data, loaded);
    }

    #[test]
    fn test_history_file_path() {
        let path = history_file_path("/tmp/history", "session-123");
        assert_eq!(path, PathBuf::from("/tmp/history/session-123.history"));
    }

    #[test]
    fn test_save_and_load_history() {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().to_str().unwrap();

        let data = b"terminal output data here";
        save_history(base_dir, "test-session", data).unwrap();

        let loaded = load_history(base_dir, "test-session").unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap(), data.to_vec());
    }

    #[test]
    fn test_load_history_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().to_str().unwrap();

        let loaded = load_history(base_dir, "nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_load_history_auto_raw_data() {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().to_str().unwrap();

        let data = b"raw terminal output";
        save_history(base_dir, "raw-session", data).unwrap();

        let loaded = load_history_auto(base_dir, "raw-session").unwrap();
        assert_eq!(loaded.unwrap(), data.to_vec());
    }

    #[test]
    fn test_load_history_auto_compressed_data() {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().to_str().unwrap();

        let original = b"compressed terminal output data for testing";
        let compressed = history::compress_history(original).unwrap();
        assert_eq!(&compressed[..4], &ZSTD_MAGIC);

        save_history(base_dir, "compressed-session", &compressed).unwrap();

        let loaded = load_history_auto(base_dir, "compressed-session").unwrap();
        assert_eq!(loaded.unwrap(), original.to_vec());
    }

    #[test]
    fn test_get_history_dir_uses_base_path() {
        let dir = get_history_dir(Path::new("/tmp/terminar"));
        assert_eq!(dir, PathBuf::from("/tmp/terminar/sessions"));
    }

    #[test]
    fn test_get_session_file_path_uses_base_path() {
        let path = get_session_file_path(Path::new("/tmp/terminar"));
        assert_eq!(path, PathBuf::from("/tmp/terminar/sessions.json"));
    }
}
