use portable_pty::{ChildKiller, CommandBuilder, ExitStatus, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Abstraction for creating PTYs to allow mocking
pub trait PtyProvider: Send + Sync {
    fn create_pty(
        &self,
        cols: u16,
        rows: u16,
    ) -> Result<Box<dyn portable_pty::MasterPty + Send>, anyhow::Error>;
    fn spawn_command(
        &self,
        master: &(dyn portable_pty::MasterPty + Send),
        cmd: CommandBuilder,
    ) -> Result<Box<dyn portable_pty::Child + Send + Sync>, anyhow::Error>;
}

pub struct NativePtyProvider;

impl PtyProvider for NativePtyProvider {
    fn create_pty(
        &self,
        cols: u16,
        rows: u16,
    ) -> Result<Box<dyn portable_pty::MasterPty + Send>, anyhow::Error> {
        let system = native_pty_system();
        let pair = system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(pair.master)
    }

    fn spawn_command(
        &self,
        _master: &(dyn portable_pty::MasterPty + Send),
        _cmd: CommandBuilder,
    ) -> Result<Box<dyn portable_pty::Child + Send + Sync>, anyhow::Error> {
        Err(anyhow::anyhow!(
            "Use native_pty_system directly for native spawning for now"
        ))
    }
}

// --- Mock Implementation ---

pub struct MockPtyProvider;

impl PtyProvider for MockPtyProvider {
    fn create_pty(
        &self,
        _cols: u16,
        _rows: u16,
    ) -> Result<Box<dyn portable_pty::MasterPty + Send>, anyhow::Error> {
        Ok(Box::new(MockMasterPty::new()))
    }

    fn spawn_command(
        &self,
        _master: &(dyn portable_pty::MasterPty + Send),
        _cmd: CommandBuilder,
    ) -> Result<Box<dyn portable_pty::Child + Send + Sync>, anyhow::Error> {
        Ok(Box::new(MockChild::new()))
    }
}

/// MockChild state shared between clone_killer instances
struct MockChildState {
    killed: bool,
}

/// Mock implementation of portable_pty::Child for testing
struct MockChild {
    state: Arc<(Mutex<MockChildState>, Condvar)>,
}

impl MockChild {
    fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new(MockChildState { killed: false }), Condvar::new())),
        }
    }

    /// Get a valid ExitStatus by spawning a real trivial process
    /// This is necessary because portable_pty::ExitStatus doesn't expose a public constructor
    fn create_exit_status() -> std::io::Result<ExitStatus> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize::default())
            .map_err(std::io::Error::other)?;

        #[cfg(unix)]
        let cmd = CommandBuilder::new("true");
        #[cfg(windows)]
        let cmd = {
            let mut c = CommandBuilder::new("cmd");
            c.args(&["/c", "exit", "0"]);
            c
        };

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        child.wait()
    }
}

impl std::fmt::Debug for MockChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockChild").finish()
    }
}

impl ChildKiller for MockChild {
    fn kill(&mut self) -> std::io::Result<()> {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.killed = true;
        cvar.notify_all();
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(MockChild {
            state: self.state.clone(),
        })
    }
}

impl portable_pty::Child for MockChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let (lock, _) = &*self.state;
        let state = lock.lock().unwrap();
        if state.killed {
            Ok(Some(Self::create_exit_status()?))
        } else {
            Ok(None)
        }
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let (lock, cvar) = &*self.state;

        // Wait for kill signal with timeout to prevent hanging tests
        {
            let mut state = lock.lock().unwrap();
            let timeout = Duration::from_secs(30); // 30s timeout for tests

            while !state.killed {
                let (new_state, wait_result) = cvar.wait_timeout(state, timeout).unwrap();
                state = new_state;

                if wait_result.timed_out() && !state.killed {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "MockChild::wait() timed out after 30s - ensure kill() is called",
                    ));
                }
            }
        };

        Self::create_exit_status()
    }

    fn process_id(&self) -> Option<u32> {
        Some(12345)
    }
}

pub struct MockMasterPty {
    sender: std::sync::mpsc::Sender<u8>,
    receiver: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<u8>>>,
    size: std::sync::Mutex<PtySize>,
}

impl Default for MockMasterPty {
    fn default() -> Self {
        Self::new()
    }
}

impl MockMasterPty {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            sender: tx,
            receiver: std::sync::Arc::new(std::sync::Mutex::new(rx)),
            size: std::sync::Mutex::new(PtySize::default()),
        }
    }
}

impl portable_pty::MasterPty for MockMasterPty {
    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        *self.size.lock().unwrap() = size;
        Ok(())
    }
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(*self.size.lock().unwrap())
    }
    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send + 'static>, anyhow::Error> {
        Ok(Box::new(MockReader {
            receiver: self.receiver.clone(),
        }))
    }
    fn take_writer(&self) -> Result<Box<dyn Write + Send + 'static>, anyhow::Error> {
        Ok(Box::new(MockWriter {
            sender: self.sender.clone(),
        }))
    }
    fn process_group_leader(&self) -> Option<i32> {
        Some(1)
    }
    fn as_raw_fd(&self) -> Option<i32> {
        None
    }
}

struct MockReader {
    receiver: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<u8>>>,
}

impl Read for MockReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Wait for first byte without holding the lock during blocking recv()
        // This prevents deadlock when writer needs the lock
        let first_byte = loop {
            // Try to receive without blocking first
            let result = {
                let rx = self.receiver.lock().unwrap();
                rx.try_recv()
            }; // Lock released here

            match result {
                Ok(byte) => break Some(byte),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // No data yet, sleep briefly and retry
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    break None; // Channel closed = EOF
                }
            }
        };

        let Some(first) = first_byte else {
            return Ok(0); // EOF
        };

        buf[0] = first;
        let mut count = 1;

        // Drain any additional available bytes (non-blocking)
        let rx = self.receiver.lock().unwrap();
        while count < buf.len() {
            match rx.try_recv() {
                Ok(b) => {
                    buf[count] = b;
                    count += 1;
                }
                Err(_) => break,
            }
        }

        Ok(count)
    }
}

struct MockWriter {
    sender: std::sync::mpsc::Sender<u8>,
}

impl Write for MockWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for b in buf {
            let _ = self.sender.send(*b);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {

    use super::*;

    pub use super::MockMasterPty;

    use portable_pty::MasterPty;

    #[test]

    fn test_mock_pty_pipe() {
        let pty = MockMasterPty::new();

        let mut writer = pty.take_writer().unwrap();

        let mut reader = pty.try_clone_reader().unwrap();

        writer.write_all(b"hello").unwrap();

        let mut buf = [0u8; 5];

        reader.read_exact(&mut buf).unwrap();

        assert_eq!(&buf, b"hello");
    }

    #[test]

    fn test_mock_pty_resize() {
        let pty = MockMasterPty::new();

        let new_size = PtySize {
            rows: 50,
            cols: 120,
            pixel_width: 960,
            pixel_height: 400,
        };
        pty.resize(new_size).unwrap();

        let retrieved = pty.get_size().unwrap();
        assert_eq!(retrieved.rows, 50);
        assert_eq!(retrieved.cols, 120);
        assert_eq!(retrieved.pixel_width, 960);
        assert_eq!(retrieved.pixel_height, 400);
    }
}
