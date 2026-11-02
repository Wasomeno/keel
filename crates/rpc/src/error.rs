//! Startup failures that are not one of the named process outcomes.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{0}")]
    Store(String),
}
