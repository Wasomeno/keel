//! Local persistence: the profile layout on disk and the SQLite snapshot store.
//!
//! `DocsStore` hosts the session and registry Loro documents and writes their
//! snapshots. Keel is local-only; this crate does no networking.

mod error;
mod layout;
mod store;

pub use error::StoreError;
pub use layout::{Layout, ThemeChoice, UiSettings};
pub use store::{
    DocsStore, NativeSession, REGISTRY_DOC_ID, SIDECAR_DIFF, SIDECAR_TAIL, session_doc_id,
};
