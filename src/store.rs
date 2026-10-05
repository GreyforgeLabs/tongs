//! The [`Store`] type.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::marker::PhantomData;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::error::{Error, ErrorKind, Result};
use crate::json::{self, DumpOptions};
use crate::python::{float_repr, normalize_path, path_display, path_name, path_parent, path_repr};
use crate::sys::{self, UtcNow};

/// Envelope format marker written into every file and required on read.
///
/// The value keeps the name the format was introduced under (the Python 1.x
/// package `atomic-json-store`). Python 1.x only recognises an envelope whose
/// `format` is exactly this string; a file carrying any other marker would be
/// read there as legacy plain data and re-wrapped, silently corrupting it. It
/// must never change while Python 1.x interoperability is supported.
pub const FORMAT: &str = "atomic-json-store/1";
/// Nesting limit for [`Store::load_as`], the same as
/// `serde_json::from_str`'s.
const SERDE_RECURSION_LIMIT: usize = 128;

/// Schema version assigned to plain JSON files that have no envelope.
pub const LEGACY_VERSION: u64 = 0;

/// What to do when the store file exists but cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CorruptPolicy {
    /// Fail with [`ErrorKind::Corrupt`] and leave the file untouched.
    #[default]
    Raise,
    /// Rename the file to `<name>.corrupt-<UTC timestamp>` and start again
    /// from the default document.
    Quarantine,
}

/// Envelope metadata, read without migrating, locking or writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreInfo {
    /// Store path (normalised like Python's `pathlib`).
    pub path: PathBuf,
    /// Whether the file exists.
    pub exists: bool,
    /// `Some(FORMAT)` for an enveloped file.
    pub format: Option<String>,
    /// The file's schema version: `Some(0)` for a plain JSON file, `None` for
    /// a missing or unreadable file or an envelope with an invalid version.
    pub schema_version: Option<u64>,
    /// The envelope's `updated_at` timestamp, if it is a string.
    pub updated_at: Option<String>,
    /// File size in bytes (0 when missing).
    pub size_bytes: u64,
}

impl StoreInfo {
    /// The metadata as the JSON object the CLI prints (key order: `path`,
    /// `exists`, `format`, `schema_version`, `updated_at`, `size_bytes`).
    pub fn to_json(&self) -> Value {
        let opt_str = |s: &Option<String>| s.clone().map_or(Value::Null, Value::String);
        let mut map = Map::new();
        map.insert(
            "path".to_owned(),
            Value::String(self.path.to_string_lossy().into_owned()),
        );
        map.insert("exists".to_owned(), Value::Bool(self.exists));
        map.insert("format".to_owned(), opt_str(&self.format));
        map.insert(
            "schema_version".to_owned(),
            self.schema_version.map_or(Value::Null, Value::from),
        );
        map.insert("updated_at".to_owned(), opt_str(&self.updated_at));
        map.insert("size_bytes".to_owned(), Value::from(self.size_bytes));
        Value::Object(map)
    }
}

type MigrationFn = Box<dyn Fn(Value) -> Result<Value> + Send + Sync>;

/// Values a migration closure may return.
///
/// * `Value` - the migrated document.
/// * `Option<Value>` - `None` is reported as an error, like a Python migration
///   that forgot to `return`.
/// * `Result<T, E>` - an `Err` aborts the load with [`ErrorKind::Migration`].
///
/// A migration that produces JSON `null` is rejected, exactly as the Python
/// implementation rejected a migration returning `None`. Nothing is written
/// when a migration fails.
pub trait MigrationOutput {
    /// Convert into the migrated document (`None` means "returned nothing").
    fn into_migrated(self) -> std::result::Result<Option<Value>, MigrationFailure>;
}

/// An error returned by a fallible migration.
#[derive(Debug)]
pub struct MigrationFailure(Box<dyn std::error::Error + Send + Sync + 'static>);

impl MigrationOutput for Value {
    fn into_migrated(self) -> std::result::Result<Option<Value>, MigrationFailure> {
        Ok(Some(self))
    }
}

impl MigrationOutput for Option<Value> {
    fn into_migrated(self) -> std::result::Result<Option<Value>, MigrationFailure> {
        Ok(self)
    }
}

impl<T, E> MigrationOutput for std::result::Result<T, E>
where
    T: MigrationOutput,
    E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    fn into_migrated(self) -> std::result::Result<Option<Value>, MigrationFailure> {
        match self {
            Ok(v) => v.into_migrated(),
            Err(e) => Err(MigrationFailure(e.into())),
        }
    }
}

/// What an [`Store::update`] closure returns.
///
/// * `()` - keep the (possibly mutated) document.
/// * `Option<Value>` - `Some` replaces the document, `None` keeps it.
/// * `Value` - replaces the document.
pub trait UpdateOutcome {
    /// Resolve the stored document from the mutated one.
    fn resolve(self, mutated: Value) -> Value;
}

impl UpdateOutcome for () {
    fn resolve(self, mutated: Value) -> Value {
        mutated
    }
}

impl UpdateOutcome for Option<Value> {
    fn resolve(self, mutated: Value) -> Value {
        self.unwrap_or(mutated)
    }
}

impl UpdateOutcome for Value {
    fn resolve(self, _mutated: Value) -> Value {
        self
    }
}

enum DefaultDoc {
    Value(Value),
    Factory(Box<dyn Fn() -> Value + Send + Sync>),
}

/// Configures and creates an [`Store`].
pub struct StoreBuilder {
    path: PathBuf,
    schema_version: u64,
    migrations: BTreeMap<u64, MigrationFn>,
    default: DefaultDoc,
    lock_timeout: Option<f64>,
    indent: Option<String>,
    sort_keys: bool,
    ensure_ascii: bool,
    fsync: bool,
    on_corrupt: CorruptPolicy,
    file_mode: Option<u32>,
}

impl StoreBuilder {
    /// Version this program expects (default `1`). Older files are migrated,
    /// newer files are refused.
    pub fn schema_version(mut self, version: u64) -> Self {
        self.schema_version = version;
        self
    }

    /// Register the upgrade from version `from` to `from + 1`.
    pub fn migration<F, R>(mut self, from: u64, f: F) -> Self
    where
        F: Fn(Value) -> R + Send + Sync + 'static,
        R: MigrationOutput,
    {
        let step: MigrationFn = Box::new(move |doc| match f(doc).into_migrated() {
            Ok(Some(Value::Null)) | Ok(None) => Err(Error::new(
                ErrorKind::SchemaVersion,
                format!(
                    "migration {from} -> {} returned None; return the migrated document",
                    from + 1
                ),
            )),
            Ok(Some(v)) => Ok(v),
            Err(MigrationFailure(e)) => Err(Error::new(
                ErrorKind::Migration,
                format!("migration {from} -> {} failed: {e}", from + 1),
            )
            .with_source(e)),
        });
        self.migrations.insert(from, step);
        self
    }

    /// Document used when the file does not exist (cloned each time).
    /// Default: an empty object.
    pub fn default_value(mut self, value: Value) -> Self {
        self.default = DefaultDoc::Value(value);
        self
    }

    /// Factory producing the document used when the file does not exist.
    pub fn default_with<F>(mut self, f: F) -> Self
    where
        F: Fn() -> Value + Send + Sync + 'static,
    {
        self.default = DefaultDoc::Factory(Box::new(f));
        self
    }

    /// How long to wait for the lock: `None` waits forever, zero fails fast.
    /// Default: 10 seconds.
    pub fn lock_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.lock_timeout = timeout.map(|d| d.as_secs_f64());
        self
    }

    /// Like [`lock_timeout`](Self::lock_timeout) but in (fractional) seconds.
    /// Negative values are rejected by [`build`](Self::build); NaN and
    /// infinity wait forever.
    pub fn lock_timeout_secs(mut self, seconds: Option<f64>) -> Self {
        self.lock_timeout = seconds;
        self
    }

    /// Spaces of indentation on disk (default `Some(2)`); `None` writes one line.
    pub fn indent(mut self, spaces: Option<usize>) -> Self {
        self.indent = spaces.map(|n| " ".repeat(n));
        self
    }

    /// Custom indentation string (e.g. `"\t"`); `None` writes one line.
    pub fn indent_str(mut self, indent: Option<&str>) -> Self {
        self.indent = indent.map(str::to_owned);
        self
    }

    /// Sort object keys on disk (default `false`, insertion order is kept).
    pub fn sort_keys(mut self, sort: bool) -> Self {
        self.sort_keys = sort;
        self
    }

    /// Escape non-ASCII characters on disk (default `false`).
    pub fn ensure_ascii(mut self, ascii: bool) -> Self {
        self.ensure_ascii = ascii;
        self
    }

    /// fsync the file and its directory on every write (default `true`).
    pub fn fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }

    /// Corrupt-file policy (default [`CorruptPolicy::Raise`]).
    pub fn on_corrupt(mut self, policy: CorruptPolicy) -> Self {
        self.on_corrupt = policy;
        self
    }

    /// Mode for the file; `None` (default) keeps an existing file's mode and
    /// uses `0o600` for new files.
    pub fn file_mode(mut self, mode: Option<u32>) -> Self {
        self.file_mode = mode;
        self
    }

    /// Validate the configuration and create the store. Nothing touches the
    /// filesystem until the first operation.
    pub fn build(self) -> Result<Store> {
        if let Some(t) = self.lock_timeout
            && t < 0.0
        {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                "lock_timeout must be >= 0 or None",
            ));
        }
        let name = path_name(&self.path);
        if name.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!("PosixPath({}) has an empty name", path_repr(&self.path)),
            ));
        }
        let mut lock_name = OsString::from(name);
        lock_name.push(".lock");
        let lock_path = sibling(&self.path, &lock_name);
        Ok(Store {
            path: self.path,
            lock_path,
            schema_version: self.schema_version,
            migrations: self.migrations,
            default: self.default,
            lock_timeout: self.lock_timeout,
            dump: DumpOptions {
                indent: self.indent,
                sort_keys: self.sort_keys,
                ensure_ascii: self.ensure_ascii,
            },
            fsync: self.fsync,
            on_corrupt: self.on_corrupt,
            file_mode: self.file_mode,
            lock_states: Mutex::new(HashMap::new()),
        })
    }
}

/// `PurePath.with_name(name)` for a normalised path with a non-empty name.
fn sibling(path: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    match bytes.iter().rposition(|b| *b == b'/') {
        Some(i) => {
            let mut out = OsString::from(std::ffi::OsStr::from_bytes(&bytes[..=i]));
            out.push(name);
            PathBuf::from(out)
        }
        None => PathBuf::from(name),
    }
}

/// Per-thread lock bookkeeping. The descriptor lives here (not in a guard) so
/// the `flock` is released when the last guard of the thread is dropped, in
/// whatever order the guards are dropped.
struct LockState {
    depth: usize,
    exclusive: bool,
    file: File,
}

/// A JSON document on disk with atomic writes, cross-process locking and
/// schema migrations.
///
/// Every write serialises the document first, writes it to a temporary file
/// in the target directory, fsyncs it and renames it over the store, so
/// readers see either the previous complete document or the new one. Writers
/// coordinate through an advisory `flock` on a sidecar `<name>.lock` file, so
/// read-modify-write cycles from separate processes (Rust or the Python 1.x
/// implementation) never lose updates. Locks are re-entrant per thread.
///
/// The store is `Send + Sync`; share it between threads with `Arc`.
pub struct Store {
    path: PathBuf,
    lock_path: PathBuf,
    schema_version: u64,
    migrations: BTreeMap<u64, MigrationFn>,
    default: DefaultDoc,
    lock_timeout: Option<f64>,
    dump: DumpOptions,
    fsync: bool,
    on_corrupt: CorruptPolicy,
    file_mode: Option<u32>,
    lock_states: Mutex<HashMap<ThreadId, LockState>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path)
            .field("schema_version", &self.schema_version)
            .field("migrations", &self.migrations.keys().collect::<Vec<_>>())
            .field("lock_timeout", &self.lock_timeout)
            .field("on_corrupt", &self.on_corrupt)
            .finish_non_exhaustive()
    }
}

/// A held store lock. Dropping it releases the lock (or one level of a
/// re-entrant hold). It is tied to the thread that acquired it.
#[must_use = "the lock is released when the guard is dropped"]
pub struct LockGuard<'a> {
    store: &'a Store,
    _not_send: PhantomData<*const ()>,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        let tid = thread::current().id();
        let mut states = self
            .store
            .lock_states
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let released = match states.get_mut(&tid) {
            Some(state) => {
                state.depth -= 1;
                if state.depth == 0 {
                    states.remove(&tid)
                } else {
                    None
                }
            }
            None => None,
        };
        drop(states);
        if let Some(state) = released {
            sys::unflock(&state.file);
        }
    }
}

enum ReadError {
    NeedsExclusive,
    Fail(Error),
}

impl From<Error> for ReadError {
    fn from(e: Error) -> Self {
        ReadError::Fail(e)
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_REPLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Called once, inside the publish critical section, after the temporary
    /// file has been written and before it is renamed over the store.
    static BEFORE_NEXT_REPLACE: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}

fn valid_version(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(n)) => {
            let text = n.to_string();
            if text.bytes().all(|b| b.is_ascii_digit()) {
                text.parse().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_envelope(doc: &Value) -> bool {
    matches!(doc, Value::Object(map) if map.get("format").and_then(Value::as_str) == Some(FORMAT))
}

impl Store {
    /// A store at `path` with default settings.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::builder(path).build()
    }

    /// Start configuring a store at `path`. The path is normalised like
    /// Python's `pathlib` (`./a//b/` becomes `a/b`).
    pub fn builder(path: impl AsRef<Path>) -> StoreBuilder {
        StoreBuilder {
            path: normalize_path(path.as_ref().as_os_str()),
            schema_version: 1,
            migrations: BTreeMap::new(),
            default: DefaultDoc::Value(Value::Object(Map::new())),
            lock_timeout: Some(10.0),
            indent: Some("  ".to_owned()),
            sort_keys: false,
            ensure_ascii: false,
            fsync: true,
            on_corrupt: CorruptPolicy::Raise,
            file_mode: None,
        }
    }

    /// The store path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sidecar lock path (`<name>.lock` beside the store).
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// The schema version this store expects.
    pub fn schema_version(&self) -> u64 {
        self.schema_version
    }

    /// Whether the store file exists (and is a regular file).
    pub fn exists(&self) -> bool {
        fs::metadata(&self.path).is_ok_and(|m| m.is_file())
    }

    /// Read envelope metadata. Never migrates, never writes, never takes the
    /// lock and never creates the parent directory or the lock file. Writers
    /// replace the whole file atomically, so this always sees one complete
    /// generation.
    pub fn info(&self) -> Result<StoreInfo> {
        let mut info = StoreInfo {
            path: self.path.clone(),
            exists: false,
            format: None,
            schema_version: None,
            updated_at: None,
            size_bytes: 0,
        };
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(info),
            Err(e) => return Err(Error::io(e, Some(&self.path), None)),
        };
        info.exists = true;
        info.size_bytes = raw.len() as u64;
        let Ok(doc) = json::loads_bytes(&raw) else {
            return Ok(info);
        };
        if !is_envelope(&doc) {
            info.schema_version = Some(LEGACY_VERSION);
            return Ok(info);
        }
        info.format = Some(FORMAT.to_owned());
        info.schema_version = valid_version(doc.get("schema_version"));
        info.updated_at = doc
            .get("updated_at")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(info)
    }

    /// Return the current document, migrating and persisting it if needed.
    /// A missing file yields the default document without creating the file.
    pub fn load(&self) -> Result<Value> {
        {
            let _shared = self.lock(false)?;
            match self.read(false) {
                Ok(v) => return Ok(v),
                Err(ReadError::NeedsExclusive) => {}
                Err(ReadError::Fail(e)) => return Err(e),
            }
        }
        let _exclusive = self.lock(true)?;
        self.read_repair()
    }

    /// [`load`](Self::load) and deserialise into `T`. Documents nested more
    /// than 128 levels deep are refused with [`ErrorKind::Deserialize`], as
    /// `serde_json::from_str` refuses them.
    pub fn load_as<T: DeserializeOwned>(&self) -> Result<T> {
        let mut doc = self.load()?;
        // serde deserialises a Value recursively with large frames and, unlike
        // `serde_json::from_str`, without a depth limit; apply the same limit
        // (128) so a deeply nested file cannot overflow the stack.
        if json::nests_deeper_than(&doc, SERDE_RECURSION_LIMIT) {
            return Err(Error::new(
                ErrorKind::Deserialize,
                format!(
                    "recursion limit exceeded: the document nests deeper than {SERDE_RECURSION_LIMIT} levels"
                ),
            ));
        }
        json::normalize_numbers_for_serde(&mut doc);
        serde_json::from_value(doc)
            .map_err(|e| Error::new(ErrorKind::Deserialize, e.to_string()).with_source(e))
    }

    /// Replace the document atomically.
    pub fn save(&self, data: &Value) -> Result<()> {
        let _g = self.lock(true)?;
        self.write(data)
    }

    /// Serialise `data` with serde and replace the document atomically. A
    /// serialisation failure leaves the store untouched.
    pub fn save_as<T: Serialize + ?Sized>(&self, data: &T) -> Result<()> {
        let _g = self.lock(true)?;
        let value = serde_json::to_value(data)
            .map_err(|e| Error::new(ErrorKind::Serialize, e.to_string()).with_source(e))?;
        self.write(&value)
    }

    /// Apply `f` to the current document under the exclusive lock and store
    /// the result, which is also returned. `f` may mutate its argument in
    /// place (return `()`), or return a replacement document.
    pub fn update<F, R>(&self, f: F) -> Result<Value>
    where
        F: FnOnce(&mut Value) -> R,
        R: UpdateOutcome,
    {
        let _g = self.lock(true)?;
        let mut data = self.read_repair()?;
        let outcome = f(&mut data);
        let data = outcome.resolve(data);
        self.write(&data)?;
        Ok(data)
    }

    /// Run `f` on the document under the exclusive lock; the (mutated)
    /// document is committed only if `f` returns `Ok`. On `Err` nothing is
    /// written and the error is returned.
    pub fn transaction<T, E, F>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&mut Value) -> std::result::Result<T, E>,
        E: From<Error>,
    {
        let _g = self.lock(true)?;
        let mut data = self.read_repair()?;
        let out = f(&mut data)?;
        self.write(&data)?;
        Ok(out)
    }

    /// Read one top-level key of an object document (`None` when missing).
    pub fn get(&self, key: &str) -> Result<Option<Value>> {
        match self.load()? {
            Value::Object(mut map) => Ok(map.shift_remove(key)),
            _ => Err(Error::new(
                ErrorKind::NotAMapping,
                "get() requires a mapping document",
            )),
        }
    }

    /// Read one top-level key of an object document, or `default`.
    pub fn get_or(&self, key: &str, default: Value) -> Result<Value> {
        Ok(self.get(key)?.unwrap_or(default))
    }

    /// Write one top-level key of an object document.
    pub fn set(&self, key: &str, value: Value) -> Result<()> {
        self.transaction(|data| match data {
            Value::Object(map) => {
                map.insert(key.to_owned(), value);
                Ok(())
            }
            _ => Err(Error::new(
                ErrorKind::NotAMapping,
                "set() requires a mapping document",
            )),
        })
    }

    /// Replace the document with a fresh default and return it.
    pub fn reset(&self) -> Result<Value> {
        let _g = self.lock(true)?;
        let data = self.fresh();
        self.write(&data)?;
        Ok(data)
    }

    /// Acquire the store lock (shared or exclusive) and hold it until the
    /// guard is dropped. Store operations on the same thread re-enter it; an
    /// operation that needs the exclusive lock fails while only a shared lock
    /// is held.
    pub fn lock(&self, exclusive: bool) -> Result<LockGuard<'_>> {
        let tid = thread::current().id();
        {
            let mut states = self.lock_states.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(state) = states.get_mut(&tid) {
                if exclusive && !state.exclusive {
                    return Err(Error::new(
                        ErrorKind::Store,
                        "a shared lock cannot be upgraded to exclusive while held",
                    ));
                }
                state.depth += 1;
                return Ok(LockGuard {
                    store: self,
                    _not_send: PhantomData,
                });
            }
        }
        sys::mkdir_p(&path_parent(&self.lock_path))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock_path)
            .map_err(|e| Error::io(e, Some(&self.lock_path), None))?;
        self.acquire(&file, exclusive)?;
        self.lock_states
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                tid,
                LockState {
                    depth: 1,
                    exclusive,
                    file,
                },
            );
        Ok(LockGuard {
            store: self,
            _not_send: PhantomData,
        })
    }

    fn acquire(&self, file: &File, exclusive: bool) -> Result<()> {
        let deadline = self.lock_timeout.and_then(|t| {
            Duration::try_from_secs_f64(t)
                .ok()
                .and_then(|d| Instant::now().checked_add(d))
                .map(Some)
                // NaN, infinity and absurdly large timeouts never expire.
                .unwrap_or(None)
        });
        let has_deadline = deadline.is_some();
        let mut delay = 0.002_f64;
        while !sys::try_flock(file, exclusive).map_err(|e| Error::io(e, None, None))? {
            if has_deadline && deadline.is_some_and(|d| Instant::now() >= d) {
                let kind = if exclusive { "exclusive" } else { "shared" };
                return Err(Error::new(
                    ErrorKind::LockTimeout,
                    format!(
                        "timed out after {}s waiting for {kind} lock on {}",
                        float_repr(self.lock_timeout.unwrap_or(0.0)),
                        path_display(&self.lock_path)
                    ),
                ));
            }
            thread::sleep(Duration::from_secs_f64(delay));
            delay = (delay * 2.0).min(0.05);
        }
        Ok(())
    }

    // ── Internals (call only while holding the lock) ───────────────────

    fn fresh(&self) -> Value {
        match &self.default {
            DefaultDoc::Value(v) => v.clone(),
            DefaultDoc::Factory(f) => f(),
        }
    }

    fn read_repair(&self) -> Result<Value> {
        match self.read(true) {
            Ok(v) => Ok(v),
            Err(ReadError::Fail(e)) => Err(e),
            Err(ReadError::NeedsExclusive) => Err(Error::new(
                ErrorKind::Store,
                "internal error: repair read requested escalation",
            )),
        }
    }

    fn read(&self, repair: bool) -> std::result::Result<Value, ReadError> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(self.fresh()),
            Err(e) => return Err(Error::io(e, Some(&self.path), None).into()),
        };
        let document = match json::loads_bytes(&raw) {
            Ok(doc) => doc,
            Err(e) => return self.corrupt(&format!("invalid JSON: {e}"), repair),
        };
        let (version, data) = if is_envelope(&document) {
            let Value::Object(mut map) = document else {
                unreachable!()
            };
            let version = valid_version(map.get("schema_version"));
            match (version, map.shift_remove("data")) {
                (Some(v), Some(data)) => (v, data),
                _ => {
                    return self.corrupt("envelope is missing schema_version or data", repair);
                }
            }
        } else {
            (LEGACY_VERSION, document)
        };
        if version == self.schema_version {
            return Ok(data);
        }
        if version > self.schema_version {
            return Err(Error::new(
                ErrorKind::SchemaVersion,
                format!(
                    "{} is at schema version {version}, newer than the supported version {}",
                    path_display(&self.path),
                    self.schema_version
                ),
            )
            .into());
        }
        if !repair {
            return Err(ReadError::NeedsExclusive);
        }
        let migrated = self.migrate(data, version)?;
        self.write(&migrated)?;
        Ok(migrated)
    }

    fn corrupt(&self, reason: &str, repair: bool) -> std::result::Result<Value, ReadError> {
        if self.on_corrupt == CorruptPolicy::Raise {
            return Err(Error::new(
                ErrorKind::Corrupt,
                format!("{}: {reason}", path_display(&self.path)),
            )
            .into());
        }
        if !repair {
            return Err(ReadError::NeedsExclusive);
        }
        let mut name = OsString::from(path_name(&self.path));
        name.push(".corrupt-");
        name.push(UtcNow::now().compact());
        let quarantine = sibling(&self.path, &name);
        // Keep an interrupt from landing between moving the file aside and
        // publishing the fresh document.
        let _deferral = sys::SignalDeferral::begin();
        fs::rename(&self.path, &quarantine)
            .map_err(|e| Error::io(e, Some(&self.path), Some(&quarantine)))?;
        let data = self.fresh();
        self.write(&data)?;
        Ok(data)
    }

    fn migrate(&self, mut data: Value, version: u64) -> Result<Value> {
        for step in version..self.schema_version {
            let Some(f) = self.migrations.get(&step) else {
                return Err(Error::new(
                    ErrorKind::SchemaVersion,
                    format!(
                        "{} is at schema version {version} and no migration is registered for version {step} -> {}",
                        path_display(&self.path),
                        step + 1
                    ),
                ));
            };
            data = f(data)?;
        }
        Ok(data)
    }

    fn write(&self, data: &Value) -> Result<()> {
        // The envelope adds one level; never publish a file that the reader
        // would then refuse as too deeply nested (and possibly quarantine).
        if json::nests_deeper_than(data, json::MAX_DEPTH - 1) {
            return Err(Error::new(
                ErrorKind::Serialize,
                format!(
                    "the document nests deeper than {} levels and could not be read back",
                    json::MAX_DEPTH - 1
                ),
            ));
        }
        let version = Value::from(self.schema_version);
        let format = Value::String(FORMAT.to_owned());
        let updated = Value::String(UtcNow::now().isoformat());
        // Serialise before touching the filesystem.
        let mut text = json::dumps_entries(
            vec![
                ("format", &format),
                ("schema_version", &version),
                ("updated_at", &updated),
                ("data", data),
            ],
            &self.dump,
        );
        text.push('\n');
        let parent = path_parent(&self.path);
        sys::mkdir_p(&parent)?;
        let mode = match self.file_mode {
            Some(mode) => mode,
            None => match fs::metadata(&self.path) {
                Ok(meta) => meta.permissions().mode() & 0o777,
                Err(e) if e.kind() == io::ErrorKind::NotFound => 0o600,
                Err(e) => return Err(Error::io(e, Some(&self.path), None)),
            },
        };
        let mut prefix = b".".to_vec();
        prefix.extend_from_slice(path_name(&self.path).as_bytes());
        prefix.push(b'.');
        // From creating the temporary file until it has been renamed (or
        // removed after a failure) and the directory synced, terminating
        // signals stay pending, so an interrupt never leaves temp debris or a
        // half-finished publish behind. They are delivered when this returns.
        let _deferral = sys::SignalDeferral::begin();
        let (file, tmp) = sys::mkstemp(&parent, &prefix, ".tmp")?;
        let result = self.publish(file, &tmp, text.as_bytes(), mode);
        if let Err(e) = result {
            // Best effort: the temp file may already be gone.
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if self.fsync {
            sys::fsync_directory(&parent)?;
        }
        Ok(())
    }

    fn publish(&self, mut file: File, tmp: &Path, text: &[u8], mode: u32) -> Result<()> {
        file.write_all(text)
            .and_then(|()| file.flush())
            .map_err(|e| Error::io(e, None, None))?;
        if self.fsync {
            file.sync_all().map_err(|e| Error::io(e, None, None))?;
        }
        drop(file);
        fs::set_permissions(tmp, Permissions::from_mode(mode))
            .map_err(|e| Error::io(e, Some(tmp), None))?;
        #[cfg(test)]
        if let Some(hook) = BEFORE_NEXT_REPLACE.with(|h| h.take()) {
            hook(tmp);
        }
        #[cfg(test)]
        if FAIL_NEXT_REPLACE.with(|f| f.replace(false)) {
            return Err(Error::io(
                io::Error::other("disk went away"),
                Some(tmp),
                Some(&self.path),
            ));
        }
        fs::rename(tmp, &self.path).map_err(|e| Error::io(e, Some(tmp), Some(&self.path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_store_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        (dir, path)
    }

    #[test]
    fn failed_replace_leaves_original_and_no_temp_files() {
        let (dir, path) = tmp_store_path();
        let store = Store::new(&path).unwrap();
        store.save(&json!({"v": 1})).unwrap();
        FAIL_NEXT_REPLACE.with(|f| f.set(true));
        let err = store.save(&json!({"v": 2})).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io);
        let envelope: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(envelope["data"], json!({"v": 1}));
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    /// The envelope string on disk is part of the Python 1.x interop contract
    /// and must not follow the crate rename.
    #[test]
    fn format_marker_is_the_python_1x_value() {
        assert_eq!(FORMAT, "atomic-json-store/1");
    }

    static SIGHUP_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static SIGHUP_SEEN_DURING_PUBLISH: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static TERMINATING_SIGNALS_BLOCKED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    extern "C" fn record_sighup(_: libc::c_int) {
        SIGHUP_SEEN.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Runs inside the publish critical section: send this thread a SIGHUP,
    /// which must stay pending, and check the mask blocks every deferred
    /// signal.
    fn signal_self_mid_publish(tmp: &Path) {
        use std::sync::atomic::Ordering::SeqCst;
        assert!(tmp.exists());
        // SAFETY: plain libc calls on this thread; the mask query writes into
        // an initialised sigset_t.
        unsafe {
            libc::pthread_kill(libc::pthread_self(), libc::SIGHUP);
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut mask);
            let blocked = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM]
                .iter()
                .all(|&s| libc::sigismember(&mask, s) == 1);
            TERMINATING_SIGNALS_BLOCKED.store(blocked, SeqCst);
        }
        SIGHUP_SEEN_DURING_PUBLISH.store(SIGHUP_SEEN.load(SeqCst), SeqCst);
    }

    #[test]
    fn signal_during_publish_is_deferred_until_the_write_completes() {
        use std::sync::atomic::Ordering::SeqCst;
        let (dir, path) = tmp_store_path();
        let store = Store::new(&path).unwrap();
        store.save(&json!({"v": 1})).unwrap();
        // SAFETY: installs a handler that only stores to an atomic. SIGHUP is
        // not used by anything else in the test binary.
        let previous = unsafe {
            libc::signal(
                libc::SIGHUP,
                record_sighup as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
        let mut before: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: query-only call into an initialised sigset_t.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut before) };
        BEFORE_NEXT_REPLACE.with(|h| h.set(Some(signal_self_mid_publish)));
        store.save(&json!({"v": 2})).unwrap();
        // SAFETY: restore the previous disposition.
        unsafe { libc::signal(libc::SIGHUP, previous) };
        assert!(TERMINATING_SIGNALS_BLOCKED.load(SeqCst));
        assert!(
            !SIGHUP_SEEN_DURING_PUBLISH.load(SeqCst),
            "signal was not deferred"
        );
        assert!(
            SIGHUP_SEEN.load(SeqCst),
            "deferred signal was not delivered"
        );
        let mut after: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut after) };
        for s in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
            // SAFETY: reads initialised sigsets.
            unsafe { assert_eq!(libc::sigismember(&before, s), libc::sigismember(&after, s)) };
        }
        let envelope: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(envelope["data"], json!({"v": 2}));
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn guards_dropped_out_of_order_keep_the_lock_until_the_last() {
        let (_dir, path) = tmp_store_path();
        let store = Store::new(&path).unwrap();
        let other = Store::builder(&path)
            .lock_timeout_secs(Some(0.0))
            .build()
            .unwrap();
        let outer = store.lock(true).unwrap();
        let inner = store.lock(true).unwrap();
        drop(outer);
        assert_eq!(other.load().unwrap_err().kind(), ErrorKind::LockTimeout);
        drop(inner);
        assert!(other.load().is_ok());
    }

    #[test]
    fn sibling_and_lock_paths() {
        assert_eq!(
            sibling(Path::new("a/b.json"), "b.json.lock".as_ref()),
            Path::new("a/b.json.lock")
        );
        assert_eq!(
            sibling(Path::new("b.json"), "b.json.lock".as_ref()),
            Path::new("b.json.lock")
        );
        let err = Store::new(".").unwrap_err();
        assert_eq!(err.to_string(), "PosixPath('.') has an empty name");
    }
}
