//! Session creation engine: core PTY session lifecycle.
//!
//! Extracted from terminar-server's handlers/session.rs.
//! Contains `create_session()` and validation helpers.
//!
//! **Requires tokio runtime.** `create_session()` spawns a blocking reader task
//! (`tokio::task::spawn_blocking`) and an exit monitor task (`tokio::spawn`).

use crate::constants::{ENV_BLOCKLIST, PTY_READ_BUFFER_SIZE, SHELL_WHITELIST};
use crate::history::CircularBuffer;
use crate::messages::SessionInfo;
use crate::pty::{MockPtyProvider, PtyProvider};
use crate::session::{Session, SessionEvent, SessionMap, SessionState};

use parking_lot::Mutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Find the last byte index in `data` that ends a complete UTF-8 sequence.
/// Any trailing bytes that start but don't complete a multi-byte character
/// are excluded, so the caller can carry them over to the next read.
fn find_utf8_safe_boundary(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    match std::str::from_utf8(data) {
        Ok(_) => data.len(),
        Err(e) => {
            let valid = e.valid_up_to();
            if e.error_len().is_some() {
                // Genuinely invalid byte(s) - skip past them
                valid + e.error_len().unwrap()
            } else {
                // Incomplete sequence at end - carry it over
                valid
            }
        }
    }
}

/// Gets the default shell from $SHELL environment variable.
/// Falls back to /bin/sh if $SHELL is not set or not in whitelist.
fn get_default_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL")
        && SHELL_WHITELIST.contains(&shell.as_str())
    {
        return shell;
    }
    "/bin/sh".to_string()
}

/// Resolves the shell path - if empty, uses the system default.
pub fn resolve_shell(shell: &str) -> String {
    if shell.is_empty() {
        get_default_shell()
    } else {
        shell.to_string()
    }
}

/// Resolves the working directory - if empty or "/", uses the user's home directory.
pub fn resolve_cwd(cwd: &str) -> String {
    if cwd.is_empty() || cwd == "/" {
        std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
    } else {
        cwd.to_string()
    }
}

/// Validates that a shell path is in the whitelist and is safe.
/// Returns an error message if validation fails, None if valid.
pub fn validate_shell(shell: &str) -> Option<String> {
    // Reject path traversal attempts (contains "..")
    if shell.contains("..") {
        return Some(format!(
            "Shell path '{}' contains path traversal and is not allowed",
            shell
        ));
    }

    // Reject relative paths (must start with /)
    if !shell.starts_with('/') {
        return Some(format!("Shell '{}' must be an absolute path", shell));
    }

    // Check against whitelist
    if !SHELL_WHITELIST.contains(&shell) {
        return Some(format!("Shell '{}' is not in the allowed whitelist", shell));
    }

    None
}

/// Validates a working directory path for safety and existence.
/// Returns an error message if validation fails, None if valid.
pub fn validate_cwd(cwd: &str) -> Option<String> {
    // Reject path traversal
    if cwd.contains("..") {
        return Some(format!(
            "Working directory '{}' contains path traversal and is not allowed",
            cwd
        ));
    }

    // Check path exists and is a directory
    let path = std::path::Path::new(cwd);
    if !path.exists() {
        return Some(format!("Working directory '{}' does not exist", cwd));
    }
    if !path.is_dir() {
        return Some(format!("Working directory '{}' is not a directory", cwd));
    }

    None
}

/// Filters environment variables, removing those in the blocklist.
pub fn filter_env(env: &HashMap<String, String>) -> HashMap<String, String> {
    env.iter()
        .filter(|(k, _)| !ENV_BLOCKLIST.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Clamps terminal dimensions to valid range (1-500).
/// Returns the clamped value and logs a warning if clamping was necessary.
pub fn clamp_dimension(val: u16, name: &str) -> u16 {
    if val < 1 {
        warn!("{} {} below minimum, clamping to 1", name, val);
        1
    } else if val > 500 {
        warn!("{} {} above maximum, clamping to 500", name, val);
        500
    } else {
        val
    }
}

/// Build a SessionInfo list from the current sessions.
pub fn build_session_list(sessions: &SessionMap) -> Vec<SessionInfo> {
    let guard = sessions.lock();
    guard
        .values()
        .map(|s| SessionInfo {
            id: s.id.clone(),
            name: s.name.clone(),
            shell: s.shell_cmd.clone(),
            cwd: s.cwd.clone(),
            started_at: "now".to_string(),
            state: Some(s.state.display_name().to_string()),
            foreground_process: s.foreground_process.clone(),
            last_activity_at: s.last_output_at.lock().map(|_| "now".to_string()),
            exit_code: s.exit_code,
        })
        .collect()
}

/// Core PTY session creation logic.
///
/// Creates a PTY, spawns a shell, sets up the reader thread and exit monitor,
/// and inserts the session into the session map.
///
/// **Requires tokio runtime.** Spawns `tokio::task::spawn_blocking` for the reader
/// and `tokio::spawn` for the exit monitor.
///
/// - `session_id`: None = generate UUID, Some = reuse ID (for restoration)
/// - `name`: Session display name
/// - `initial_history`: Pre-loaded scrollback to seed the history buffer
#[allow(clippy::too_many_arguments)]
pub fn create_session(
    session_id: Option<&str>,
    name: &str,
    shell_cmd: &str,
    cwd: &str,
    cols: u16,
    rows: u16,
    env: &HashMap<String, String>,
    sessions: &SessionMap,
    mock_provider: Option<&Arc<MockPtyProvider>>,
    initial_history: Option<&[u8]>,
) -> Result<String, Box<dyn std::error::Error>> {
    let safe_env = filter_env(env);

    let master: Box<dyn portable_pty::MasterPty + Send>;

    if let Some(provider) = mock_provider {
        master = provider.create_pty(cols, rows)?;
        let _c = provider.spawn_command(&*master, CommandBuilder::new(shell_cmd))?;
    } else {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        master = pair.master;

        let mut cmd = CommandBuilder::new(shell_cmd);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &safe_env {
            cmd.env(k, v);
        }
        let _c = pair.slave.spawn_command(cmd)?;
    }

    let id = session_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let (tx, _) = broadcast::channel(100);
    let history = Arc::new(Mutex::new(CircularBuffer::with_default_capacity()));

    // Seed history buffer with pre-loaded scrollback (for restoration)
    if let Some(data) = initial_history {
        let mut h = history.lock();
        h.push(data);
    }

    let history_clone = history.clone();

    // Subscribe for exit monitoring before tx is moved into the session
    let mut exit_rx = tx.subscribe();
    let exit_sessions = sessions.clone();
    let exit_session_id = id.clone();

    // Clone reader and take writer before wrapping master in Arc<Mutex<>>
    let mut reader = master.try_clone_reader()?;
    let writer = master.take_writer()?;
    let tx_clone = tx.clone();

    // Capture the raw fd before wrapping - used for tcgetpgrp() foreground process detection
    #[cfg(unix)]
    let pty_fd = master.as_raw_fd();
    #[cfg(not(unix))]
    let pty_fd: Option<i32> = None;

    // Wrap master in Arc<Mutex<>> for synchronized concurrent access
    let sync_master = Arc::new(Mutex::new(master));
    // Wrap writer in Arc<Mutex<>> - cache it to prevent dropping after each input
    let sync_writer = Arc::new(Mutex::new(writer));

    // Share activity tracking state with the reader task (avoids locking the sessions map)
    let reader_last_output_at = Arc::new(Mutex::new(None::<std::time::Instant>));
    let reader_last_bell_at = Arc::new(Mutex::new(None::<std::time::Instant>));
    let reader_silence_notified = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // These will be assigned to the session after construction
    let session_last_output_at = reader_last_output_at.clone();
    let session_last_bell_at = reader_last_bell_at.clone();
    let session_silence_notified = reader_silence_notified.clone();

    // Use tokio::task::spawn_blocking for PTY reading (blocking I/O)
    let reader_handle = tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut buf = [0u8; PTY_READ_BUFFER_SIZE];
            // Carry-over buffer for incomplete UTF-8 sequences split across reads.
            // A UTF-8 character is at most 4 bytes, so we only ever carry up to 3 bytes.
            let mut utf8_remainder: Vec<u8> = Vec::with_capacity(4);
            loop {
                // Read into the buffer, leaving room for the remainder prefix
                let read_start = utf8_remainder.len();
                match reader.read(&mut buf[read_start..]) {
                    Ok(0) => {
                        // PTY EOF - shell process exited
                        let _ = tx_clone.send(SessionEvent::Exited(None));
                        break;
                    }
                    Ok(n) => {
                        // Prepend any leftover bytes from previous read
                        buf[..read_start].copy_from_slice(&utf8_remainder);
                        utf8_remainder.clear();
                        let total = read_start + n;
                        let data_bytes = &buf[0..total];

                        // Find the boundary of complete UTF-8 sequences.
                        let valid_up_to = find_utf8_safe_boundary(data_bytes);

                        // Save any trailing incomplete bytes for the next read
                        if valid_up_to < total {
                            utf8_remainder.extend_from_slice(&data_bytes[valid_up_to..total]);
                        }

                        let safe_bytes = &data_bytes[0..valid_up_to];
                        if !safe_bytes.is_empty() {
                            // Check for bell character (0x07) in the output
                            let has_bell = safe_bytes.contains(&0x07);

                            // Update activity timestamps
                            let was_silent = reader_silence_notified
                                .swap(false, std::sync::atomic::Ordering::Relaxed);
                            {
                                let mut t = reader_last_output_at.lock();
                                *t = Some(std::time::Instant::now());
                            }
                            if has_bell {
                                let mut t = reader_last_bell_at.lock();
                                *t = Some(std::time::Instant::now());
                            }

                            // Send activity notification if this is first output after silence
                            if was_silent {
                                let _ = tx_clone.send(SessionEvent::Activity);
                            }

                            // Send bell notification if detected
                            if has_bell {
                                let _ = tx_clone.send(SessionEvent::Bell);
                            }

                            let data = String::from_utf8_lossy(safe_bytes).to_string();
                            {
                                let mut h = history_clone.lock();
                                h.push(safe_bytes);
                            }
                            let _ = tx_clone.send(SessionEvent::Output(data));
                        }
                    }
                    Err(_) => {
                        // I/O error - treat as process exit
                        let _ = tx_clone.send(SessionEvent::Exited(None));
                        break;
                    }
                }
            }
        }));

        if let Err(panic_info) = result {
            let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else {
                "Unknown panic".to_string()
            };
            error!("PTY reader thread panicked: {}", panic_msg);
        }
    });

    let session = Session {
        id: id.clone(),
        name: name.to_string(),
        shell_cmd: shell_cmd.to_string(),
        cwd: cwd.to_string(),
        state: SessionState::Running,
        master: sync_master,
        writer: sync_writer,
        output_tx: tx,
        history,
        reader_handle: Some(reader_handle),
        pty_fd,
        foreground_process: None,
        last_output_at: session_last_output_at,
        last_bell_at: session_last_bell_at,
        child_pid: None,
        exit_code: None,
        silence_notified: session_silence_notified,
        silence_threshold_secs: 30,
    };

    {
        let mut guard = sessions.lock();
        guard.insert(id.clone(), session);
        info!(session_id = %id, shell = %shell_cmd, cwd = %cwd, event = "session_created", "Session lifecycle: created");
    }

    // Spawn a task that listens for the Exited event and transitions the session state.
    tokio::spawn(async move {
        while let Ok(event) = exit_rx.recv().await {
            if let SessionEvent::Exited(exit_code) = event {
                let mut guard = exit_sessions.lock();
                if let Some(session) = guard.get_mut(&exit_session_id) {
                    let _ = session.transition_to(SessionState::Exited);
                    session.exit_code = exit_code;
                }
                break;
            }
        }
    });

    Ok(id)
}
