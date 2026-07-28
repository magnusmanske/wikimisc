//! The error type shared by the disk-spilling storage layer
//! ([`crate::file_hash::FileHash`] and [`crate::file_vec::FileVec`]).
//!
//! Both types sit on the same tempfile-and-JSON machinery, so they fail in the
//! same ways and share one error type rather than one wrapping the other.

use thiserror::Error;

/// Failure modes of the disk-spilling collections.
#[derive(Debug, Error)]
pub enum FileError {
    /// A thread panicked while holding the backing file's lock, so the file can
    /// no longer be assumed to be in a known state.
    #[error("the backing file mutex is poisoned")]
    PoisonedMutex,

    /// An index outside the collection was addressed.
    #[error("index {index} is out of bounds for a collection of length {len}")]
    OutOfBounds { index: usize, len: usize },

    /// A record that should exist could not be read back. Indicates the
    /// in-memory index and the on-disk contents have diverged.
    #[error("row {0} could not be read back from storage")]
    UnreadableRow(usize),

    /// Reading from or writing to the backing file failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A record could not be serialised to, or deserialised from, JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
