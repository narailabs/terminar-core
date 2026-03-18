//! terminar-core: PTY session engine library.
//!
//! This crate provides the core PTY session management functionality
//! extracted from terminar-server. It includes:
//! - PTY creation and management
//! - Session lifecycle and state machine
//! - History buffering with compression
//! - Session persistence (parameterized paths)
//! - Foreground process detection
//! - Session-related wire protocol types
//!
//! **Requires tokio runtime.** `create_session()` spawns blocking reader tasks
//! and async exit monitor tasks. Consumers must call it from within a tokio context.

pub mod constants;
pub mod engine;
pub mod history;
pub mod messages;
pub mod persistence;
#[cfg(unix)]
pub mod process;
pub mod pty;
pub mod session;

pub use engine::create_session;
pub use history::CircularBuffer;
pub use messages::{CoreClientMessage, CoreServerMessage, SessionInfo};
pub use pty::{MockPtyProvider, NativePtyProvider, PtyProvider};
pub use session::{Session, SessionEvent, SessionMap, SessionState};
