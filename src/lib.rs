//! tongs: atomic, cross-process locked, schema-versioned JSON persistence.
//!
//! ```no_run
//! use tongs::{Store, json};
//!
//! # fn main() -> tongs::Result<()> {
//! let store = Store::builder("state.json")
//!     .default_with(|| json!({"runs": 0, "last": null}))
//!     .build()?;
//!
//! // Read the whole document (the default if the file does not exist yet).
//! let doc = store.load()?;
//!
//! // Replace it atomically.
//! store.save(&json!({"runs": 1, "last": "2026-09-06"}))?;
//!
//! // Read-modify-write under the exclusive lock.
//! store.update(|doc| {
//!     doc["runs"] = json!(doc["runs"].as_i64().unwrap_or(0) + 1);
//! })?;
//!
//! // Commit only when the closure returns Ok.
//! store.transaction(|doc| {
//!     doc["last"] = json!("2026-09-07");
//!     Ok::<_, tongs::Error>(())
//! })?;
//! # let _ = doc;
//! # Ok(())
//! # }
//! ```
//!
//! Files use the same envelope, formatting and sidecar lock protocol as the
//! Python 1.x implementation (released as `atomic-json-store`), so Rust and
//! Python processes can share a store. The envelope's `format` marker is
//! therefore still `"atomic-json-store/1"` (see [`FORMAT`]).

#![warn(missing_docs)]

#[cfg(not(unix))]
compile_error!("tongs supports Unix-like systems (Linux, macOS) only");

mod error;
pub mod json;
pub mod python;
mod store;
mod sys;

pub use error::{Error, ErrorKind, Result};
pub use serde_json::{self, Map, Value, json};
pub use store::{
    CorruptPolicy, FORMAT, LEGACY_VERSION, LockGuard, MigrationFailure, MigrationOutput, Store,
    StoreBuilder, StoreInfo, UpdateOutcome,
};
