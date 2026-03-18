use parking_lot::Mutex; // Non-poisoning mutex
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::debug;

use crate::history::CircularBuffer;

/// Type alias for synchronized PTY writer access.
/// The writer is taken once from the master and cached for the session lifetime.
pub type SyncWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Represents the state of a terminal session
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    /// Session is being created (PTY being set up)
    Creating,
    /// Session is running and accepting input
    Running,
    /// Session is in the process of closing
    Closing,
    /// Session has been closed
    Closed,
    /// Shell process exited but session stays viewable
    Exited,
    /// Session encountered an error
    Error,
}

impl SessionState {
    /// Check if a transition from the current state to the target state is valid.
    ///
    /// Valid transitions:
    /// - Creating -> Running (successful creation)
    /// - Creating -> Error (creation failed)
    /// - Running -> Closing (graceful shutdown initiated)
    /// - Running -> Exited (shell process exits)
    /// - Running -> Error (runtime error)
    /// - Closing -> Closed (shutdown complete)
    /// - Closing -> Error (shutdown failed)
    /// - Exited -> Closed (user manually closes / cleanup)
    ///
    /// Invalid transitions (terminal states cannot transition out):
    /// - Closed -> any
    /// - Error -> any
    /// - Any state -> Creating (cannot go back to initial state)
    /// - Closing -> Running (cannot resume from closing)
    /// - Exited -> Running (cannot restart exited shell)
    pub fn can_transition_to(&self, target: &SessionState) -> bool {
        // Cannot transition to same state
        if self == target {
            return false;
        }

        match (self, target) {
            // Creating can transition to Running or Error
            (SessionState::Creating, SessionState::Running) => true,
            (SessionState::Creating, SessionState::Error) => true,

            // Running can transition to Closing, Exited, or Error
            (SessionState::Running, SessionState::Closing) => true,
            (SessionState::Running, SessionState::Exited) => true,
            (SessionState::Running, SessionState::Error) => true,

            // Closing can transition to Closed or Error
            (SessionState::Closing, SessionState::Closed) => true,
            (SessionState::Closing, SessionState::Error) => true,

            // Exited can transition to Closed (user manually closes)
            (SessionState::Exited, SessionState::Closed) => true,

            // All other transitions are invalid
            _ => false,
        }
    }

    /// Check if input operations are allowed in this state.
    /// Only Running state allows input.
    pub fn allows_input(&self) -> bool {
        matches!(self, SessionState::Running)
    }

    /// Check if resize operations are allowed in this state.
    /// Only Running state allows resize.
    pub fn allows_resize(&self) -> bool {
        matches!(self, SessionState::Running)
    }

    /// Check if attach operations are allowed in this state.
    /// Running and Exited states allow attach (Exited is read-only).
    pub fn allows_attach(&self) -> bool {
        matches!(self, SessionState::Running | SessionState::Exited)
    }

    /// Return the lowercase display name of the state.
    pub fn display_name(&self) -> &'static str {
        match self {
            SessionState::Creating => "creating",
            SessionState::Running => "running",
            SessionState::Closing => "closing",
            SessionState::Closed => "closed",
            SessionState::Exited => "exited",
            SessionState::Error => "error",
        }
    }
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    Output(String),
    Closed,
    /// Shell process exited with an optional exit code
    Exited(Option<i32>),
    /// Bell character detected in output
    Bell,
    /// Activity notification (first output after silence)
    Activity,
    /// Silence notification (no output for N seconds)
    Silence,
    /// Foreground process changed (e.g., user started vim, claude, etc.)
    ForegroundChanged(Option<String>),
    /// Current working directory changed (e.g., user ran `cd`)
    CwdChanged(String),
}

/// Type alias for synchronized PTY master access.
///
/// Uses Mutex instead of RwLock because `portable_pty::MasterPty` is `Send` but
/// not `Sync`. RwLock requires `T: Sync` for `RwLock<T>: Sync`, which is needed for
/// `Arc<RwLock<T>>: Send + Sync`.
pub type SyncMasterPty = Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>;

pub struct Session {
    pub id: String,
    pub name: String,
    pub shell_cmd: String,
    /// The initial working directory for the session
    pub cwd: String,
    /// The current state of the session
    pub state: SessionState,
    /// The PTY master wrapped in Arc<Mutex<>> for synchronized concurrent access.
    pub master: SyncMasterPty,
    /// Cached PTY writer - taken once from master and reused for all input.
    pub writer: SyncWriter,
    pub output_tx: broadcast::Sender<SessionEvent>,
    pub history: Arc<Mutex<CircularBuffer>>,
    /// Handle to the reader thread, used for cleanup
    pub reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// Raw file descriptor of the PTY master, used for `tcgetpgrp()` calls on Unix.
    /// `None` for mock PTYs that don't have a real fd, or on non-Unix platforms.
    /// Stored as `i32` to match `portable_pty::MasterPty::as_raw_fd()` return type
    /// and avoid depending on `std::os::unix::io::RawFd`.
    pub pty_fd: Option<i32>,
    /// Name of the foreground process (e.g., "vim", "claude")
    pub foreground_process: Option<String>,
    /// Timestamp of last output from the PTY (shared with reader task)
    pub last_output_at: Arc<Mutex<Option<Instant>>>,
    /// Timestamp of last bell character received (shared with reader task)
    pub last_bell_at: Arc<Mutex<Option<Instant>>>,
    /// Child process ID (PID of the shell)
    pub child_pid: Option<u32>,
    /// Exit code from the shell process
    pub exit_code: Option<i32>,
    /// Whether silence notification has been sent (shared with reader task)
    pub silence_notified: Arc<AtomicBool>,
    /// Silence threshold in seconds (default: 30)
    pub silence_threshold_secs: u64,
}

pub type SessionMap = Arc<Mutex<HashMap<String, Session>>>;

impl Session {
    /// Create a new session with the given PTY master.
    ///
    /// The master is automatically wrapped in `Arc<Mutex<>>` for synchronized
    /// concurrent access. The writer is taken from the master before wrapping
    /// and cached to prevent the PTY stdin from being closed after each input.
    ///
    /// # Errors
    /// Returns an error if `take_writer()` fails on the master.
    pub fn new(
        id: String,
        name: String,
        shell_cmd: String,
        cwd: String,
        master: Box<dyn portable_pty::MasterPty + Send>,
        output_tx: broadcast::Sender<SessionEvent>,
        history: Arc<Mutex<CircularBuffer>>,
    ) -> Result<Self, String> {
        // Capture the raw fd before wrapping - used for tcgetpgrp() calls
        let pty_fd = master.as_raw_fd();
        // Take the writer before wrapping master to cache it for the session lifetime
        let writer = master
            .take_writer()
            .map_err(|e| format!("Failed to take writer from PTY master: {}", e))?;
        Ok(Self {
            id,
            name,
            shell_cmd,
            cwd,
            state: SessionState::Running,
            master: Arc::new(Mutex::new(master)),
            writer: Arc::new(Mutex::new(writer)),
            output_tx,
            history,
            reader_handle: None,
            pty_fd,
            foreground_process: None,
            last_output_at: Arc::new(Mutex::new(None)),
            last_bell_at: Arc::new(Mutex::new(None)),
            child_pid: None,
            exit_code: None,
            silence_notified: Arc::new(AtomicBool::new(false)),
            silence_threshold_secs: 30,
        })
    }

    /// Attempt to transition the session to a new state.
    /// Returns Ok(()) if the transition is valid, Err with message if invalid.
    pub fn transition_to(&mut self, new_state: SessionState) -> Result<(), String> {
        if self.state.can_transition_to(&new_state) {
            self.state = new_state;
            Ok(())
        } else {
            Err(format!(
                "Invalid state transition from {:?} to {:?}",
                self.state, new_state
            ))
        }
    }

    /// Check if input operations are allowed in the current state.
    pub fn allows_input(&self) -> bool {
        self.state.allows_input()
    }

    /// Check if resize operations are allowed in the current state.
    pub fn allows_resize(&self) -> bool {
        self.state.allows_resize()
    }

    /// Set the reader thread handle after spawning
    pub fn set_reader_handle(&mut self, handle: tokio::task::JoinHandle<()>) {
        self.reader_handle = Some(handle);
    }

    /// Returns the number of active broadcast subscribers for this session.
    pub fn subscriber_count(&self) -> usize {
        self.output_tx.receiver_count()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let subscriber_count = self.subscriber_count();
        debug!(
            "Dropping session {} (state: {:?}, subscribers: {})",
            self.id, self.state, subscriber_count
        );

        if subscriber_count > 0 {
            debug!(
                "Session {} has {} active broadcast subscriber(s) that will be disconnected",
                self.id, subscriber_count
            );
        }

        // Abort the reader thread if it's still running
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::MockPtyProvider;
    use crate::pty::PtyProvider;
    use portable_pty::PtySize;
    use std::sync::atomic::Ordering;
    use std::thread;

    fn create_test_session() -> Session {
        let mock_provider = MockPtyProvider;
        let master = mock_provider.create_pty(80, 24).unwrap();
        let (tx, _rx) = broadcast::channel(100);
        let history = Arc::new(Mutex::new(
            crate::history::CircularBuffer::with_default_capacity(),
        ));

        Session::new(
            "test-id".to_string(),
            "test-session".to_string(),
            "/bin/bash".to_string(),
            "/home/user".to_string(),
            master,
            tx,
            history,
        )
        .unwrap()
    }

    #[test]
    fn test_session_master_is_mutex_wrapped() {
        let session = create_test_session();
        let guard = session.master.lock();
        let reader_result = guard.try_clone_reader();
        assert!(reader_result.is_ok());
    }

    #[test]
    fn test_cached_writer_through_mutex() {
        let session = create_test_session();
        let mut writer_guard = session.writer.lock();
        use std::io::Write;
        let write_result = writer_guard.write_all(b"test input\n");
        assert!(write_result.is_ok());
    }

    #[test]
    fn test_resize_through_mutex() {
        let session = create_test_session();
        let master_guard = session.master.lock();
        let resize_result = master_guard.resize(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        });
        assert!(resize_result.is_ok());
    }

    #[test]
    fn test_concurrent_input_and_resize_are_serialized() {
        let session = Arc::new(create_test_session());
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let s = session.clone();
                thread::spawn(move || {
                    if i % 2 == 0 {
                        let master_guard = s.master.lock();
                        let _ = master_guard.resize(PtySize {
                            rows: 24 + (i as u16),
                            cols: 80,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    } else {
                        let mut writer_guard = s.writer.lock();
                        use std::io::Write;
                        let _ = write!(writer_guard, "test{}", i);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Thread should complete successfully");
        }
    }

    #[test]
    fn test_session_state_transitions() {
        assert!(SessionState::Creating.can_transition_to(&SessionState::Running));
        assert!(SessionState::Creating.can_transition_to(&SessionState::Error));
        assert!(SessionState::Running.can_transition_to(&SessionState::Closing));
        assert!(SessionState::Running.can_transition_to(&SessionState::Exited));
        assert!(SessionState::Closing.can_transition_to(&SessionState::Closed));
        assert!(SessionState::Exited.can_transition_to(&SessionState::Closed));
        assert!(!SessionState::Closed.can_transition_to(&SessionState::Running));
        assert!(!SessionState::Error.can_transition_to(&SessionState::Running));
    }

    #[test]
    fn test_session_state_allows_operations() {
        assert!(SessionState::Running.allows_input());
        assert!(SessionState::Running.allows_resize());
        assert!(SessionState::Running.allows_attach());
        assert!(!SessionState::Closed.allows_input());
        assert!(!SessionState::Exited.allows_input());
        assert!(SessionState::Exited.allows_attach());
    }

    #[test]
    fn test_session_state_display_names() {
        assert_eq!(SessionState::Creating.display_name(), "creating");
        assert_eq!(SessionState::Running.display_name(), "running");
        assert_eq!(SessionState::Closing.display_name(), "closing");
        assert_eq!(SessionState::Closed.display_name(), "closed");
        assert_eq!(SessionState::Exited.display_name(), "exited");
        assert_eq!(SessionState::Error.display_name(), "error");
    }

    #[test]
    fn test_session_lifecycle() {
        let mut session = create_test_session();
        assert_eq!(session.state, SessionState::Running);
        assert!(session.allows_input());

        session.transition_to(SessionState::Exited).unwrap();
        assert!(!session.allows_input());
        assert!(session.state.allows_attach());

        session.transition_to(SessionState::Closed).unwrap();
        assert!(!session.state.allows_attach());
    }

    #[test]
    fn test_session_exit_code() {
        let mut session = create_test_session();
        assert_eq!(session.exit_code, None);
        session.exit_code = Some(0);
        assert_eq!(session.exit_code, Some(0));
    }

    #[test]
    fn test_session_silence_fields() {
        let session = create_test_session();
        assert!(!session.silence_notified.load(Ordering::Relaxed));
        assert_eq!(session.silence_threshold_secs, 30);
        assert!(session.last_output_at.lock().is_none());
        assert!(session.last_bell_at.lock().is_none());
    }

    #[test]
    fn test_session_subscriber_count() {
        let session = create_test_session();
        assert_eq!(session.subscriber_count(), 0);
        let _rx1 = session.output_tx.subscribe();
        assert_eq!(session.subscriber_count(), 1);
        let _rx2 = session.output_tx.subscribe();
        assert_eq!(session.subscriber_count(), 2);
    }

    #[test]
    fn test_session_drop_with_subscribers() {
        let session = create_test_session();
        let _rx1 = session.output_tx.subscribe();
        let _rx2 = session.output_tx.subscribe();
        drop(session);
        // If we get here without panic, the Drop works
    }
}
