//! Linux writer boundary for independent clients of the disposable fixture.

use crate::{ConnectError, Target};
use mysql::Conn;
use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockEvent {
    Busy,
    Acquired,
}

#[derive(Debug)]
pub enum WriterError {
    Busy,
    Lock(io::Error),
    Connect(ConnectError),
}

impl fmt::Display for WriterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("fixture writer lock is busy"),
            Self::Lock(error) => write!(f, "fixture writer lock failed ({:?})", error.kind()),
            Self::Connect(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for WriterError {}

pub struct FixtureWriter {
    pub connection: Conn,
    // Drop the connection before releasing the process-held lock.
    _lock: File,
}

impl FixtureWriter {
    pub fn acquire(
        target: &Target,
        lock: &Path,
        timeout: Duration,
        mut observe: impl FnMut(LockEvent) -> io::Result<()>,
    ) -> Result<Self, WriterError> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock)
            .map_err(WriterError::Lock)?;
        let deadline = Instant::now() + timeout;
        let mut observed_busy = false;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if !observed_busy {
                        observe(LockEvent::Busy).map_err(WriterError::Lock)?;
                        observed_busy = true;
                    }
                    if Instant::now() >= deadline {
                        return Err(WriterError::Busy);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryLockError::Error(error)) => return Err(WriterError::Lock(error)),
            }
        }
        observe(LockEvent::Acquired).map_err(WriterError::Lock)?;
        let connection = target.connect().map_err(WriterError::Connect)?;
        Ok(Self {
            connection,
            _lock: lock,
        })
    }
}
