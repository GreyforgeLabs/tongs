//! Library behaviour. Each test is a port of the same-named case in the
//! Python 1.x `tests/test_core.py` (see docs/test-mapping.md).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use tongs::{CorruptPolicy, Error, ErrorKind, FORMAT, Store, Value, json};

fn store_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    (dir, path)
}

fn read_envelope(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn tmp_leftovers(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "tmp"))
        .collect()
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn missing_file_yields_default_without_creating_it() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    assert_eq!(store.load().unwrap(), json!({}));
    assert!(!path.exists());
    assert!(!store.exists());
}

#[test]
fn default_value_is_copied_not_shared() {
    let (_d, path) = store_path();
    let template = json!({"items": []});
    let store = Store::builder(&path)
        .default_value(template.clone())
        .build()
        .unwrap();
    let mut loaded = store.load().unwrap();
    loaded["items"].as_array_mut().unwrap().push(json!(1));
    assert_eq!(store.load().unwrap(), json!({"items": []}));
    assert_eq!(template, json!({"items": []}));
}

#[test]
fn default_callable_is_invoked() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .default_with(|| json!(["fresh"]))
        .build()
        .unwrap();
    assert_eq!(store.load().unwrap(), json!(["fresh"]));
}

#[test]
fn save_and_load_roundtrip_with_envelope() {
    let (_d, path) = store_path();
    let store = Store::builder(&path).schema_version(3).build().unwrap();
    let doc = json!({"name": "forge", "count": 2, "nested": {"ok": true}});
    store.save(&doc).unwrap();
    assert_eq!(store.load().unwrap(), doc);
    let envelope = read_envelope(&path);
    assert_eq!(envelope["format"], json!(FORMAT));
    assert_eq!(envelope["schema_version"], json!(3));
    assert!(envelope["updated_at"].as_str().unwrap().ends_with("+00:00"));
    assert_eq!(
        keys(&envelope),
        ["format", "schema_version", "updated_at", "data"]
    );
    assert!(fs::read_to_string(&path).unwrap().ends_with('\n'));
}

#[test]
fn insertion_order_is_preserved_by_default() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!({"zeta": 1, "alpha": 2})).unwrap();
    assert_eq!(keys(&store.load().unwrap()), ["zeta", "alpha"]);
}

#[test]
fn sort_keys_option() {
    let (_d, path) = store_path();
    let store = Store::builder(&path).sort_keys(true).build().unwrap();
    store.save(&json!({"zeta": 1, "alpha": 2})).unwrap();
    assert_eq!(keys(&store.load().unwrap()), ["alpha", "zeta"]);
}

#[test]
fn unicode_is_written_verbatim() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!({"city": "Zürich", "mark": "✓"})).unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("Zürich"));
    assert_eq!(store.load().unwrap()["mark"], json!("✓"));
}

#[test]
fn update_with_in_place_mutation() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .default_with(|| json!({"n": 0}))
        .build()
        .unwrap();
    let bump = |data: &mut Value| {
        data["n"] = json!(data["n"].as_i64().unwrap() + 1);
    };
    assert_eq!(store.update(bump).unwrap(), json!({"n": 1}));
    assert_eq!(store.update(bump).unwrap(), json!({"n": 2}));
    assert_eq!(store.load().unwrap(), json!({"n": 2}));
}

#[test]
fn update_with_replacement_document() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!([1, 2])).unwrap();
    let replaced = store
        .update(|data: &mut Value| {
            let mut items = data.as_array().unwrap().clone();
            items.push(json!(3));
            Value::Array(items)
        })
        .unwrap();
    assert_eq!(replaced, json!([1, 2, 3]));
    assert_eq!(store.load().unwrap(), json!([1, 2, 3]));
}

#[test]
fn transaction_commits_on_clean_exit() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store
        .transaction(|data| {
            data["written"] = json!(true);
            Ok::<_, Error>(())
        })
        .unwrap();
    assert_eq!(store.load().unwrap(), json!({"written": true}));
}

#[test]
fn transaction_discards_on_exception() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!({"keep": 1})).unwrap();

    #[derive(Debug)]
    enum Abort {
        Runtime(&'static str),
        Store(#[allow(dead_code)] Error),
    }
    impl From<Error> for Abort {
        fn from(e: Error) -> Self {
            Abort::Store(e)
        }
    }
    let result = store.transaction(|data| {
        data["keep"] = json!(2);
        Err::<(), _>(Abort::Runtime("abort"))
    });
    assert!(matches!(result, Err(Abort::Runtime("abort"))));
    assert!(!matches!(result, Err(Abort::Store(_))));
    assert_eq!(store.load().unwrap(), json!({"keep": 1}));
}

#[test]
fn get_and_set_helpers() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    assert_eq!(
        store.get_or("missing", json!("fallback")).unwrap(),
        json!("fallback")
    );
    assert_eq!(store.get("missing").unwrap(), None);
    store.set("token", json!("abc")).unwrap();
    assert_eq!(store.get("token").unwrap(), Some(json!("abc")));
}

#[test]
fn get_and_set_reject_non_mapping_documents() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .default_with(|| json!([]))
        .build()
        .unwrap();
    let err = store.get("x").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotAMapping);
    assert_eq!(err.to_string(), "get() requires a mapping document");
    let err = store.set("x", json!(1)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotAMapping);
    assert_eq!(err.to_string(), "set() requires a mapping document");
}

#[test]
fn reset_restores_default() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .default_with(|| json!({"fresh": true}))
        .build()
        .unwrap();
    store.save(&json!({"fresh": false, "extra": 1})).unwrap();
    assert_eq!(store.reset().unwrap(), json!({"fresh": true}));
    assert_eq!(store.load().unwrap(), json!({"fresh": true}));
}

// test_failed_replace_leaves_original_and_no_temp_files lives in
// src/store.rs (it needs the crate-internal fault-injection hook).

#[test]
fn unserializable_data_never_touches_disk() {
    let (d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!({"v": 1})).unwrap();
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    // A map with non-string keys cannot be represented as a JSON object.
    let mut bad: BTreeMap<Vec<u8>, i32> = BTreeMap::new();
    bad.insert(vec![1, 2], 3);
    let err = store.save_as(&bad).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Serialize);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    assert_eq!(store.load().unwrap(), json!({"v": 1}));
    assert!(tmp_leftovers(d.path()).is_empty());
}

#[test]
fn custom_encoder() {
    // Python passed a JSONEncoder subclass that turned Path into str; in Rust
    // any serde::Serialize type is accepted (PathBuf serialises as a string).
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    let mut doc = BTreeMap::new();
    doc.insert("p", PathBuf::from("/tmp/x"));
    store.save_as(&doc).unwrap();
    assert_eq!(store.load().unwrap(), json!({"p": "/tmp/x"}));
    let typed: BTreeMap<String, PathBuf> = store.load_as().unwrap();
    assert_eq!(typed["p"], PathBuf::from("/tmp/x"));
}

#[test]
fn new_file_is_private_and_existing_mode_is_preserved() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!({})).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    store.save(&json!({"again": true})).unwrap();
    assert_eq!(mode(&path), 0o644);
}

#[test]
fn explicit_file_mode() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .file_mode(Some(0o640))
        .build()
        .unwrap();
    store.save(&json!({})).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn parent_directories_are_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep").join("er").join("state.json");
    Store::new(&path).unwrap().save(&json!({"ok": 1})).unwrap();
    assert!(path.exists());
}

#[test]
fn corrupt_file_raises_by_default() {
    let (_d, path) = store_path();
    fs::write(&path, "{not json").unwrap();
    let err = Store::new(&path).unwrap().load().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Corrupt);
    assert_eq!(
        err.to_string(),
        format!(
            "{}: invalid JSON: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            path.display()
        )
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
}

#[test]
fn envelope_missing_fields_is_corrupt() {
    let (_d, path) = store_path();
    fs::write(
        &path,
        json!({"format": FORMAT, "schema_version": 1}).to_string(),
    )
    .unwrap();
    let err = Store::new(&path).unwrap().load().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Corrupt);
    assert!(
        err.to_string()
            .ends_with("envelope is missing schema_version or data")
    );
    fs::write(
        &path,
        json!({"format": FORMAT, "schema_version": "1", "data": {}}).to_string(),
    )
    .unwrap();
    let err = Store::new(&path).unwrap().load().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Corrupt);
}

#[test]
fn quarantine_policy_moves_bad_file_aside() {
    let (d, path) = store_path();
    fs::write(&path, "garbage").unwrap();
    let store = Store::builder(&path)
        .on_corrupt(CorruptPolicy::Quarantine)
        .default_with(|| json!({"fresh": 1}))
        .build()
        .unwrap();
    assert_eq!(store.load().unwrap(), json!({"fresh": 1}));
    let quarantined: Vec<PathBuf> = fs::read_dir(d.path())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("state.json.corrupt-")
        })
        .collect();
    assert_eq!(quarantined.len(), 1);
    let name = quarantined[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // state.json.corrupt-YYYYmmddTHHMMSSffffffZ
    let stamp = name.trim_start_matches("state.json.corrupt-");
    assert_eq!(stamp.len(), 22);
    assert!(stamp.ends_with('Z') && stamp.as_bytes()[8] == b'T');
    assert_eq!(fs::read_to_string(&quarantined[0]).unwrap(), "garbage");
    assert_eq!(read_envelope(&path)["data"], json!({"fresh": 1}));
}

#[test]
fn legacy_plain_json_is_version_zero() {
    let (_d, path) = store_path();
    fs::write(&path, json!({"legacy": true}).to_string()).unwrap();
    let err = Store::builder(&path)
        .schema_version(1)
        .build()
        .unwrap()
        .load()
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::SchemaVersion);
    assert!(err.to_string().contains("0 -> 1"));
    let adopted = Store::builder(&path).schema_version(0).build().unwrap();
    assert_eq!(adopted.load().unwrap(), json!({"legacy": true}));
    assert!(Store::new(&path).unwrap().info().unwrap().format.is_none());
    adopted
        .save(&json!({"legacy": true, "wrapped": true}))
        .unwrap();
    assert_eq!(read_envelope(&path)["schema_version"], json!(0));
}

#[test]
fn migrations_run_in_order_and_persist() {
    let (_d, path) = store_path();
    Store::new(&path)
        .unwrap()
        .save(&json!({"name": "a"}))
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (c2, c3) = (calls.clone(), calls.clone());
    let store = Store::builder(&path)
        .schema_version(3)
        .migration(1, move |data| {
            c2.lock().unwrap().push(2);
            json!({"name": data["name"], "tags": []})
        })
        .migration(2, move |mut data| {
            c3.lock().unwrap().push(3);
            data["version_seen"] = json!(3);
            data
        })
        .build()
        .unwrap();
    assert_eq!(
        store.load().unwrap(),
        json!({"name": "a", "tags": [], "version_seen": 3})
    );
    assert_eq!(*calls.lock().unwrap(), [2, 3]);
    assert_eq!(read_envelope(&path)["schema_version"], json!(3));
    store.load().unwrap();
    assert_eq!(*calls.lock().unwrap(), [2, 3]);
}

#[test]
fn migration_from_legacy_plain_file() {
    let (_d, path) = store_path();
    fs::write(&path, json!({"legacy": true}).to_string()).unwrap();
    let store = Store::builder(&path)
        .schema_version(1)
        .migration(0, |d| json!({"wrapped": d}))
        .build()
        .unwrap();
    assert_eq!(store.load().unwrap(), json!({"wrapped": {"legacy": true}}));
    assert_eq!(read_envelope(&path)["format"], json!(FORMAT));
}

#[test]
fn missing_migration_step_is_an_error() {
    let (_d, path) = store_path();
    Store::new(&path).unwrap().save(&json!({})).unwrap();
    let store = Store::builder(&path)
        .schema_version(3)
        .migration(1, |d| d)
        .build()
        .unwrap();
    let err = store.load().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::SchemaVersion);
    assert!(err.to_string().contains("2 -> 3"));
    assert_eq!(read_envelope(&path)["schema_version"], json!(1));
}

#[test]
fn migration_returning_none_is_an_error() {
    let (_d, path) = store_path();
    Store::new(&path).unwrap().save(&json!({})).unwrap();
    let store = Store::builder(&path)
        .schema_version(2)
        .migration(1, |_| Option::<Value>::None)
        .build()
        .unwrap();
    let err = store.load().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::SchemaVersion);
    assert!(err.to_string().contains("returned None"));
    // A JSON null result is rejected the same way, as in Python.
    let store = Store::builder(&path)
        .schema_version(2)
        .migration(1, |_| Value::Null)
        .build()
        .unwrap();
    assert!(
        store
            .load()
            .unwrap_err()
            .to_string()
            .contains("returned None")
    );
    // Fallible migrations report their own error and write nothing.
    let store = Store::builder(&path)
        .schema_version(2)
        .migration(1, |_| Err::<Value, _>("bad shape"))
        .build()
        .unwrap();
    let err = store.load().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Migration);
    assert_eq!(err.to_string(), "migration 1 -> 2 failed: bad shape");
    assert_eq!(read_envelope(&path)["schema_version"], json!(1));
}

#[test]
fn newer_file_version_is_refused() {
    let (_d, path) = store_path();
    Store::builder(&path)
        .schema_version(5)
        .build()
        .unwrap()
        .save(&json!({"future": true}))
        .unwrap();
    let old = Store::builder(&path).schema_version(2).build().unwrap();
    let err = old.load().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::SchemaVersion);
    assert!(err.to_string().contains("newer"));
    assert_eq!(
        old.update(|_: &mut Value| {}).unwrap_err().kind(),
        ErrorKind::SchemaVersion
    );
    assert_eq!(read_envelope(&path)["schema_version"], json!(5));
}

#[test]
fn update_migrates_before_applying() {
    let (_d, path) = store_path();
    Store::new(&path).unwrap().save(&json!({"n": 1})).unwrap();
    let store = Store::builder(&path)
        .schema_version(2)
        .migration(1, |d| json!({"n": d["n"], "m": 0}))
        .build()
        .unwrap();
    let out = store
        .update(|data: &mut Value| {
            data["m"] = json!(data["m"].as_i64().unwrap() + 1);
        })
        .unwrap();
    assert_eq!(out, json!({"n": 1, "m": 1}));
}

#[test]
fn info_reports_metadata_without_migrating() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    let info = store.info().unwrap();
    assert!(!info.exists);
    assert_eq!(info.schema_version, None);
    store.save(&json!({"a": 1})).unwrap();
    let info = Store::builder(&path)
        .schema_version(9)
        .build()
        .unwrap()
        .info()
        .unwrap();
    assert!(info.exists);
    assert_eq!(info.format.as_deref(), Some(FORMAT));
    assert_eq!(info.schema_version, Some(1));
    assert!(info.updated_at.is_some());
    assert_eq!(info.size_bytes, fs::metadata(&path).unwrap().len());
    assert_eq!(read_envelope(&path)["schema_version"], json!(1));
}

#[test]
fn info_does_not_create_parent_or_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uncreated").join("state.json");
    let info = Store::new(&path).unwrap().info().unwrap();
    assert!(!info.exists);
    assert!(!path.parent().unwrap().exists());

    fs::create_dir(path.parent().unwrap()).unwrap();
    Store::new(&path)
        .unwrap()
        .save(&json!({"ready": true}))
        .unwrap();
    let lock_path = path.with_file_name("state.json.lock");
    fs::remove_file(&lock_path).unwrap();
    assert!(Store::new(&path).unwrap().info().unwrap().exists);
    assert!(!lock_path.exists());
}

#[test]
fn info_on_corrupt_file() {
    let (_d, path) = store_path();
    fs::write(&path, "nope").unwrap();
    let info = Store::new(&path).unwrap().info().unwrap();
    assert!(info.exists);
    assert!(info.format.is_none());
    assert!(info.schema_version.is_none());
}

#[test]
fn constructor_validation() {
    // schema_version and migration keys are u64 and migrations are closures,
    // so Python's TypeError cases for "1", -1, {"1": ...} and non-callables
    // cannot be expressed; on_corrupt is an enum. What remains is checked at
    // build time.
    let (_d, path) = store_path();
    let err = Store::builder(&path)
        .lock_timeout_secs(Some(-1.0))
        .build()
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert_eq!(err.to_string(), "lock_timeout must be >= 0 or None");
    assert!(
        Store::builder(&path)
            .lock_timeout_secs(None)
            .build()
            .is_ok()
    );
    let err = Store::new("/").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert_eq!(err.to_string(), "PosixPath('/') has an empty name");
}

#[test]
fn lock_file_lives_beside_the_store() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    assert_eq!(store.lock_path(), path.with_file_name("state.json.lock"));
    store.save(&json!({})).unwrap();
    assert!(store.lock_path().exists());
}

#[test]
fn reentrant_lock_within_a_thread() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .default_with(|| json!({"n": 0}))
        .build()
        .unwrap();
    let out = store
        .update(|data: &mut Value| {
            data["n"] = json!(data["n"].as_i64().unwrap() + 1);
            data["inner"] = store.load().unwrap()["n"].clone();
        })
        .unwrap();
    assert_eq!(out, json!({"n": 1, "inner": 0}));
}

#[test]
fn shared_lock_cannot_upgrade_to_exclusive() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!({})).unwrap();
    let _shared = store.lock(false).unwrap();
    let err = store.save(&json!({"x": 1})).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Store);
    assert_eq!(
        err.to_string(),
        "a shared lock cannot be upgraded to exclusive while held"
    );
}

#[test]
fn lock_timeout_when_another_thread_holds_the_lock() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .lock_timeout(Some(Duration::from_millis(200)))
        .build()
        .unwrap();
    store.save(&json!({})).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder_path = path.clone();
    let holder = thread::spawn(move || {
        let holder = Store::new(&holder_path).unwrap();
        let _g = holder.lock(true).unwrap();
        entered_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    let err = store.save(&json!({"blocked": true})).unwrap_err();
    let waited = started.elapsed();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(err.kind(), ErrorKind::LockTimeout);
    assert_eq!(
        err.to_string(),
        format!(
            "timed out after 0.2s waiting for exclusive lock on {}",
            store.lock_path().display()
        )
    );
    assert!(waited >= Duration::from_millis(200));
    assert_eq!(store.load().unwrap(), json!({}));
}

#[test]
fn zero_timeout_fails_fast() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .lock_timeout(Some(Duration::ZERO))
        .build()
        .unwrap();
    let holder = Store::new(&path).unwrap();
    let _g = holder.lock(true).unwrap();
    let err = store.save(&json!({})).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::LockTimeout);
}

#[test]
fn threads_never_lose_increments() {
    let (_d, path) = store_path();
    let store = Arc::new(
        Store::builder(&path)
            .default_with(|| json!({"n": 0}))
            .fsync(false)
            .build()
            .unwrap(),
    );
    let (threads, rounds) = (8, 40);
    let barrier = Arc::new(Barrier::new(threads));
    let pool: Vec<_> = (0..threads)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..rounds {
                    store
                        .update(|d: &mut Value| d["n"] = json!(d["n"].as_i64().unwrap() + 1))
                        .unwrap();
                }
            })
        })
        .collect();
    for t in pool {
        t.join().unwrap();
    }
    assert_eq!(store.load().unwrap(), json!({"n": threads * rounds}));
}

/// Worker for `processes_never_lose_increments`; runs only when spawned by it.
#[test]
#[ignore = "helper process for processes_never_lose_increments"]
fn process_worker() {
    let Ok(path) = std::env::var("TONGS_WORKER_PATH") else {
        return;
    };
    let rounds: usize = std::env::var("TONGS_WORKER_ROUNDS")
        .unwrap()
        .parse()
        .unwrap();
    let store = Store::builder(&path)
        .default_with(|| json!({"n": 0}))
        .lock_timeout(Some(Duration::from_secs(60)))
        .fsync(false)
        .build()
        .unwrap();
    for _ in 0..rounds {
        store
            .update(|d: &mut Value| d["n"] = json!(d["n"].as_i64().unwrap() + 1))
            .unwrap();
    }
}

#[test]
fn processes_never_lose_increments() {
    let (_d, path) = store_path();
    let (processes, rounds) = (6, 30);
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..processes)
        .map(|_| {
            std::process::Command::new(&exe)
                .args(["--ignored", "--exact", "process_worker", "--test-threads=1"])
                .env("TONGS_WORKER_PATH", &path)
                .env("TONGS_WORKER_ROUNDS", rounds.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        Store::new(&path).unwrap().load().unwrap(),
        json!({"n": processes * rounds})
    );
}

#[test]
fn reader_sees_only_complete_documents_during_concurrent_writes() {
    let (_d, path) = store_path();
    let store = Arc::new(Store::builder(&path).fsync(false).build().unwrap());
    let payload = "x".repeat(20_000);
    store.save(&json!({"seq": 0, "payload": payload})).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let problems = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut threads = Vec::new();
    {
        let (store, stop, payload) = (store.clone(), stop.clone(), payload.clone());
        threads.push(thread::spawn(move || {
            let mut seq = 1;
            while !stop.load(Ordering::Relaxed) {
                store
                    .save(&json!({"seq": seq, "payload": payload}))
                    .unwrap();
                seq += 1;
            }
        }));
    }
    for _ in 0..3 {
        let (store, stop, problems) = (store.clone(), stop.clone(), problems.clone());
        threads.push(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match store.load() {
                    Ok(data) => {
                        if data["payload"].as_str().map(str::len) != Some(20_000) {
                            problems.lock().unwrap().push("truncated".into());
                        }
                    }
                    Err(e) => problems.lock().unwrap().push(e.to_string()),
                }
            }
        }));
    }
    thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        t.join().unwrap();
    }
    assert!(
        problems.lock().unwrap().is_empty(),
        "{:?}",
        problems.lock().unwrap()
    );
}

// ── Additional Rust-port coverage ──────────────────────────────────────

#[test]
fn on_disk_format_is_python_compatible() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store
        .save(&json!({"s": "é\n", "f": 1.5, "e": 1e-7, "l": [], "o": {}}))
        .unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let updated = read_envelope(&path)["updated_at"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        text,
        format!(
            "{{\n  \"format\": \"atomic-json-store/1\",\n  \"schema_version\": 1,\n  \"updated_at\": \"{updated}\",\n  \"data\": {{\n    \"s\": \"é\\n\",\n    \"f\": 1.5,\n    \"e\": 1e-07,\n    \"l\": [],\n    \"o\": {{}}\n  }}\n}}\n"
        )
    );
}

#[test]
fn compact_ascii_and_sorted_options() {
    let (_d, path) = store_path();
    let store = Store::builder(&path)
        .indent(None)
        .ensure_ascii(true)
        .sort_keys(true)
        .build()
        .unwrap();
    store.save(&json!({"z": "é", "a": 1})).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let updated = read_envelope(&path)["updated_at"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        text,
        format!(
            "{{\"data\": {{\"a\": 1, \"z\": \"\\u00e9\"}}, \"format\": \"atomic-json-store/1\", \"schema_version\": 1, \"updated_at\": \"{updated}\"}}\n"
        )
    );
}

#[test]
fn big_integers_and_non_finite_numbers_survive_rewrites() {
    let (_d, path) = store_path();
    fs::write(
        &path,
        "{\"big\": 123456789012345678901234567890, \"n\": NaN, \"i\": -Infinity}",
    )
    .unwrap();
    let store = Store::builder(&path).schema_version(0).build().unwrap();
    store.update(|d: &mut Value| d["k"] = json!(1)).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"big\": 123456789012345678901234567890"));
    assert!(text.contains("\"n\": NaN"));
    assert!(text.contains("\"i\": -Infinity"));
}

#[test]
fn transaction_returns_closure_value_and_lock_is_released() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    let n = store
        .transaction(|d| {
            d["a"] = json!(1);
            Ok::<_, Error>(42)
        })
        .unwrap();
    assert_eq!(n, 42);
    let other = Store::builder(&path)
        .lock_timeout(Some(Duration::ZERO))
        .build()
        .unwrap();
    assert_eq!(other.load().unwrap(), json!({"a": 1}));
}

#[test]
fn deep_documents_are_corrupt_not_a_crash() {
    // Run on a small (2 MiB) thread stack: dropping or deserialising a
    // deeply nested serde_json::Value recurses.
    let (_d, path) = store_path();
    let deep = "[".repeat(200_000) + &"]".repeat(200_000);
    fs::write(&path, &deep).unwrap();
    let p = path.clone();
    thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let store = Store::builder(&p).schema_version(0).build().unwrap();
            assert_eq!(store.info().unwrap().schema_version, None);
            assert_eq!(store.load().unwrap_err().kind(), ErrorKind::Corrupt);
            let max = tongs::json::MAX_DEPTH;
            // A legacy file at the limit loads, but wrapping it in the
            // envelope would exceed it: the write is refused, nothing changes.
            let at_limit = "[".repeat(max) + &"]".repeat(max);
            fs::write(&p, &at_limit).unwrap();
            assert!(store.load().is_ok());
            let err = store.update(|_: &mut Value| {}).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Serialize);
            assert_eq!(fs::read_to_string(&p).unwrap(), at_limit);
            // One level less round-trips through the envelope.
            let fits = "[".repeat(max - 1) + &"]".repeat(max - 1);
            fs::write(&p, &fits).unwrap();
            store.update(|_: &mut Value| {}).unwrap();
            assert!(store.load().is_ok());
            let err = store.load_as::<Value>().unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Deserialize);
        })
        .unwrap()
        .join()
        .unwrap();
}
