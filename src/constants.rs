//! Centralized constants for terminar-core.
//!
//! Contains PTY, session, history, and security constants
//! used by the core engine.

// ==================== Security Constants ====================

/// Whitelist of approved shell paths for security.
/// Only these shells can be spawned via CreateSession.
/// Includes common locations on Linux and macOS (including Homebrew paths).
pub const SHELL_WHITELIST: &[&str] = &[
    "/bin/bash",
    "/bin/sh",
    "/bin/zsh",
    "/usr/bin/fish",
    "/usr/local/bin/bash",
    "/usr/local/bin/zsh",
    "/usr/local/bin/fish",
    "/opt/homebrew/bin/bash",
    "/opt/homebrew/bin/zsh",
    "/opt/homebrew/bin/fish",
];

/// Blocklist of environment variables that could be used for code injection.
/// These variables are filtered out before spawning shells to prevent
/// library injection attacks (e.g., LD_PRELOAD, DYLD_INSERT_LIBRARIES).
pub const ENV_BLOCKLIST: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_FORCE_FLAT_NAMESPACE",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
];

// ==================== PTY Constants ====================

/// Buffer size for PTY read operations (16KB).
/// Larger buffer reduces syscall overhead for high-throughput terminal output.
pub const PTY_READ_BUFFER_SIZE: usize = 16384;

// ==================== History Constants ====================

/// Default history buffer capacity (10 MB).
/// This is sufficient for most terminal sessions while bounding memory usage.
pub const DEFAULT_HISTORY_CAPACITY: usize = 10 * 1024 * 1024;

/// Threshold in bytes above which history data should be compressed.
/// When the buffer exceeds this size, zstd compression is applied
/// during replay/persistence.
pub const COMPRESSION_THRESHOLD: usize = 1024 * 1024; // 1 MB

/// Subdirectory name under the base path for session history files.
pub const HISTORY_SUBDIR: &str = "sessions";

/// Filename for session metadata persistence (under the base path).
pub const SESSION_METADATA_FILE: &str = "sessions.json";
