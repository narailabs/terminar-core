//! Session-related wire protocol types.
//!
//! These are the core message types for PTY session operations.
//! Auth, pairing, and workspace messages remain in terminar-server.
//!
//! Client messages use `snake_case` tags; server messages use `PascalCase` tags.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Core client messages for session operations.
///
/// Serialized with a `"type"` field in `snake_case`.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum CoreClientMessage {
    /// Request the list of all active sessions.
    ListSessions,
    /// Create a new terminal session with a PTY process.
    CreateSession {
        cwd: String,
        shell: String,
        env: HashMap<String, String>,
        cols: u16,
        rows: u16,
    },
    /// Attach to a session to receive its output stream.
    Attach {
        session_id: String,
        mode: String,
    },
    /// Send keyboard input to a session's PTY.
    Input {
        session_id: String,
        data: String,
    },
    /// Resize a session's terminal dimensions.
    Resize {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    /// Rename an existing session.
    RenameSession {
        session_id: String,
        new_name: String,
    },
    /// Terminate a session and its PTY process.
    KillSession {
        session_id: String,
    },
}

/// Core server messages for session operations.
///
/// Serialized with a `"type"` field in `PascalCase`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum CoreServerMessage {
    /// List of all active sessions.
    SessionList { sessions: Vec<SessionInfo> },
    /// Terminal output data from a session.
    Output { session_id: String, data: String },
    /// A session has been closed.
    SessionClosed { session_id: String },
    /// An error occurred processing a client message.
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    /// Notification that the foreground process in a session has changed.
    ForegroundChanged {
        session_id: String,
        process_name: Option<String>,
    },
    /// Notification of session activity (output, bell, or silence marker).
    SessionActivity {
        session_id: String,
        activity_type: String, // "activity", "bell", "silence"
    },
    /// Notification that a session's shell has exited.
    SessionExited {
        session_id: String,
        exit_code: Option<i32>,
    },
    /// Notification that the current working directory in a session has changed.
    CwdChanged { session_id: String, cwd: String },
}

/// Information about an active terminal session.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionInfo {
    /// Unique session identifier (UUID).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Shell binary path (e.g., `/bin/bash`).
    pub shell: String,
    /// Working directory.
    pub cwd: String,
    /// When the session was started.
    pub started_at: String,
    /// Current state of the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Name of the foreground process (e.g., "vim", "claude").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_process: Option<String>,
    /// Timestamp of last activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// Exit code if the shell has exited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_client_message_list_sessions() {
        let msg = CoreClientMessage::ListSessions;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"list_sessions"}"#);
    }

    #[test]
    fn test_core_client_message_create_session() {
        let mut env = HashMap::new();
        env.insert("FOO".into(), "BAR".into());
        let msg = CoreClientMessage::CreateSession {
            cwd: "/tmp".into(),
            shell: "sh".into(),
            env,
            cols: 100,
            rows: 50,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"create_session""#));
        assert!(json.contains(r#""cwd":"/tmp""#));
    }

    #[test]
    fn test_core_client_message_input() {
        let msg = CoreClientMessage::Input {
            session_id: "id".into(),
            data: "ls\n".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"input","session_id":"id","data":"ls\n"}"#);
    }

    #[test]
    fn test_core_server_message_session_list() {
        let info = SessionInfo {
            id: "1".into(),
            name: "n".into(),
            shell: "s".into(),
            cwd: "/home".into(),
            started_at: "t".into(),
            state: None,
            foreground_process: None,
            last_activity_at: None,
            exit_code: None,
        };
        let msg = CoreServerMessage::SessionList {
            sessions: vec![info],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("SessionList"));
    }

    #[test]
    fn test_core_server_message_output() {
        let msg = CoreServerMessage::Output {
            session_id: "1".into(),
            data: "d".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"Output","session_id":"1","data":"d"}"#);
    }

    #[test]
    fn test_core_server_message_error() {
        let msg = CoreServerMessage::Error {
            message: "err".into(),
            error_code: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"Error","message":"err"}"#);
    }

    #[test]
    fn test_core_server_message_foreground_changed() {
        let msg = CoreServerMessage::ForegroundChanged {
            session_id: "id1".into(),
            process_name: Some("vim".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ForegroundChanged""#));
        assert!(json.contains(r#""process_name":"vim""#));
    }

    #[test]
    fn test_core_server_message_session_exited() {
        let msg = CoreServerMessage::SessionExited {
            session_id: "id1".into(),
            exit_code: Some(0),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"SessionExited""#));
        assert!(json.contains(r#""exit_code":0"#));
    }

    #[test]
    fn test_session_info_backward_compat() {
        let old_json = r#"{"id":"1","name":"n","shell":"s","cwd":"/home","started_at":"t"}"#;
        let deserialized: SessionInfo = serde_json::from_str(old_json).unwrap();
        assert_eq!(deserialized.id, "1");
        assert_eq!(deserialized.state, None);
        assert_eq!(deserialized.foreground_process, None);
    }

    #[test]
    fn test_session_info_with_exit_code() {
        let info = SessionInfo {
            id: "1".into(),
            name: "n".into(),
            shell: "s".into(),
            cwd: "/home".into(),
            started_at: "t".into(),
            state: Some("exited".into()),
            foreground_process: None,
            last_activity_at: None,
            exit_code: Some(0),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""exit_code":0"#));
    }
}
