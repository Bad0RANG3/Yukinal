//! Host-owned, streaming local ↔ remote file transfers.
//!
//! This module is deliberately separate from the bounded text-file service. It moves bytes in
//! fixed-size buffers between a native local file handle and an SFTP stream; file contents never
//! enter an IPC or JSON value. The host supplies a [`RemoteTransferBackend`] backed by its already
//! authenticated SSH session.

mod local;
mod manager;
mod types;

pub use manager::TransferManager;
pub use types::{
    ConflictAction, ConflictActionKind, ConflictPolicy, ConflictRequest, RecoveredTransfer,
    RemoteTransferBackend, RemoteTransferEntry, TransferDirection, TransferError,
    TransferFailureKind, TransferFuture, TransferHistoryStore, TransferId, TransferItemFailure,
    TransferReader, TransferSnapshot, TransferStagingFile, TransferStatus, TransferWriter,
};

#[cfg(test)]
mod tests;
