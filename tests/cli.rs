//! CLI behaviour. Each `#[test]` up to the "Additional" marker is a port of
//! the same-named case in the Python 1.x `tests/test_cli.py`
//! (see docs/test-mapping.md).

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use tongs::{FORMAT, Store, Value, json};

const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_MISSING: i32 = 3;
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn store_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    (dir, path)
}

/// Run the binary with a non-terminal stdout and no COLUMNS override, like
/// the Python test's `main(argv)` + capsys.
fn run<I, S>(args: I) -> (i32, String, String)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = Command::cargo_bin("tongs")
        .unwrap()
        .env_remove("COLUMNS")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn version_flag() {
    let (code, out, _) = run(["--version"]);
    assert_eq!(code, 0);
    assert!(out.contains(VERSION));
    assert_eq!(out, format!("tongs {VERSION}\n"));
}

#[test]
fn module_entry_point() {
    // Python 1.x ran `python -m atomic_json_store --version`; the Rust entry
    // point is the installed binary itself.
    Command::cargo_bin("tongs")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains(VERSION));
}

#[test]
fn init_creates_store_and_is_idempotent() {
    let (_d, path) = store_path();
    let (code, out, _) = run([p(&path), "init", "--schema-version", "2"]);
    assert_eq!(code, EXIT_OK);
    let info: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(info["exists"], json!(true));
    assert_eq!(info["schema_version"], json!(2));
    Store::builder(&path)
        .schema_version(2)
        .build()
        .unwrap()
        .save(&json!({"kept": true}))
        .unwrap();
    let (code, _, _) = run([p(&path), "init", "--schema-version", "2"]);
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        Store::builder(&path)
            .schema_version(2)
            .build()
            .unwrap()
            .load()
            .unwrap(),
        json!({"kept": true})
    );
}

#[test]
fn info_on_missing_and_existing() {
    let (_d, path) = store_path();
    let (code, out, _) = run([p(&path), "info"]);
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["exists"],
        json!(false)
    );
    Store::builder(&path)
        .schema_version(4)
        .build()
        .unwrap()
        .save(&json!({"a": 1}))
        .unwrap();
    let (_, out, _) = run([p(&path), "info"]);
    let info: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        info,
        json!({
            "path": p(&path),
            "exists": true,
            "format": FORMAT,
            "schema_version": 4,
            "updated_at": info["updated_at"],
            "size_bytes": fs::metadata(&path).unwrap().len(),
        })
    );
    // Key order and formatting are byte-identical to the Python CLI.
    assert_eq!(
        out,
        format!(
            "{{\n  \"path\": \"{}\",\n  \"exists\": true,\n  \"format\": \"atomic-json-store/1\",\n  \"schema_version\": 4,\n  \"updated_at\": \"{}\",\n  \"size_bytes\": {}\n}}\n",
            p(&path),
            info["updated_at"].as_str().unwrap(),
            fs::metadata(&path).unwrap().len()
        )
    );
}

#[test]
fn set_get_dump_delete_flow() {
    let (_d, path) = store_path();
    assert_eq!(run([p(&path), "set", "service.name", "api"]).0, EXIT_OK);
    assert_eq!(
        run([p(&path), "set", "service.port", "8080", "--json"]).0,
        EXIT_OK
    );
    assert_eq!(
        run([p(&path), "set", "tags", "[\"a\",\"b\"]", "--json"]).0,
        EXIT_OK
    );

    let (code, out, _) = run([p(&path), "get", "service.port"]);
    assert_eq!(code, EXIT_OK);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!(8080));

    let (_, out, _) = run([p(&path), "get", "tags.1"]);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!("b"));

    let (_, out, _) = run([p(&path), "dump"]);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!({"service": {"name": "api", "port": 8080}, "tags": ["a", "b"]})
    );

    assert_eq!(run([p(&path), "delete", "service.name"]).0, EXIT_OK);
    let (_, out, _) = run([p(&path), "dump"]);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!({"service": {"port": 8080}, "tags": ["a", "b"]})
    );
    assert_eq!(read_json(&path)["schema_version"], json!(1));
}

#[test]
fn set_respects_existing_schema_version() {
    let (_d, path) = store_path();
    Store::builder(&path)
        .schema_version(7)
        .build()
        .unwrap()
        .save(&json!({}))
        .unwrap();
    assert_eq!(run([p(&path), "set", "k", "v"]).0, EXIT_OK);
    assert_eq!(read_json(&path)["schema_version"], json!(7));
}

#[test]
fn missing_key_exit_code_and_default() {
    let (_d, path) = store_path();
    Store::new(&path).unwrap().save(&json!({"a": 1})).unwrap();
    let (code, _, err) = run([p(&path), "get", "b"]);
    assert_eq!(code, EXIT_MISSING);
    assert!(err.contains("not found"));
    assert_eq!(err, "tongs: b: not found\n");
    let (code, out, _) = run([p(&path), "get", "b", "--default", "null"]);
    assert_eq!(code, EXIT_OK);
    assert_eq!(out, "null\n");
    let (code, _, _) = run([p(&path), "delete", "b"]);
    assert_eq!(code, EXIT_MISSING);
}

#[test]
fn missing_store_is_an_error_for_reads() {
    let (_d, path) = store_path();
    let (code, _, err) = run([p(&path), "dump"]);
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("does not exist"));
    assert_eq!(err, format!("tongs: {} does not exist\n", p(&path)));
    let (code, _, _) = run([p(&path), "get", "a"]);
    assert_eq!(code, EXIT_ERROR);
}

#[test]
fn invalid_json_value_is_a_usage_error() {
    let (_d, path) = store_path();
    let (code, _, err) = run([p(&path), "set", "k", "{broken", "--json"]);
    assert_eq!(code, EXIT_USAGE);
    assert!(err.contains("invalid JSON value"));
    assert_eq!(
        err,
        "tongs: invalid JSON value: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)\n"
    );
    assert!(!path.exists());
}

#[test]
fn corrupt_store_is_an_error() {
    let (_d, path) = store_path();
    fs::write(&path, "nope").unwrap();
    let (code, _, err) = run([p(&path), "dump"]);
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("not a readable"));
}

#[test]
fn legacy_plain_file_can_be_edited() {
    let (_d, path) = store_path();
    fs::write(&path, json!({"plain": true}).to_string()).unwrap();
    let (code, out, _) = run([p(&path), "get", "plain"]);
    assert_eq!(code, EXIT_OK);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!(true));
    assert_eq!(run([p(&path), "set", "added", "1", "--json"]).0, EXIT_OK);
    let envelope = read_json(&path);
    assert_eq!(envelope["schema_version"], json!(0));
    assert_eq!(envelope["data"], json!({"plain": true, "added": 1}));
}

// test_path_helpers is ported as a unit test in src/cli/keypath.rs.

// ── Additional Rust-port coverage (argparse parity) ────────────────────

const MAIN_USAGE: &str = "usage: tongs [-h] [--version] [--lock-timeout LOCK_TIMEOUT]
             file {init,info,dump,get,set,delete} ...
";

#[test]
fn no_arguments_is_a_usage_error() {
    let (code, out, err) = run(Vec::<&str>::new());
    assert_eq!(code, EXIT_USAGE);
    assert_eq!(out, "");
    assert_eq!(
        err,
        format!("{MAIN_USAGE}tongs: error: the following arguments are required: file, command\n")
    );
}

#[test]
fn unknown_command_lists_choices() {
    let (code, _, err) = run(["x.json", "bogus"]);
    assert_eq!(code, EXIT_USAGE);
    assert_eq!(
        err,
        format!(
            "{MAIN_USAGE}tongs: error: argument command: invalid choice: 'bogus' (choose from 'init', 'info', 'dump', 'get', 'set', 'delete')\n"
        )
    );
}

#[test]
fn subcommand_errors_use_the_subcommand_usage() {
    let (code, _, err) = run(["x.json", "get"]);
    assert_eq!(code, EXIT_USAGE);
    assert_eq!(
        err,
        "usage: tongs file get [-h] [--default DEFAULT] key\ntongs file get: error: the following arguments are required: key\n"
    );
    let (code, _, err) = run(["x.json", "init", "--schema-version", "x"]);
    assert_eq!(code, EXIT_USAGE);
    assert!(
        err.ends_with(
            "tongs file init: error: argument --schema-version: invalid int value: 'x'\n"
        )
    );
}

#[test]
fn unrecognized_and_invalid_options() {
    let (code, _, err) = run(["x.json", "info", "--lock-timeout", "5"]);
    assert_eq!(code, EXIT_USAGE);
    assert!(err.ends_with("tongs: error: unrecognized arguments: --lock-timeout 5\n"));
    let (code, _, err) = run(["--lock-timeout", "abc", "x.json", "info"]);
    assert_eq!(code, EXIT_USAGE);
    assert!(err.ends_with("error: argument --lock-timeout: invalid float value: 'abc'\n"));
    let (code, _, err) = run(["--version=1"]);
    assert_eq!(code, EXIT_USAGE);
    assert!(err.ends_with("error: argument --version: ignored explicit argument '1'\n"));
}

#[test]
fn abbreviations_and_negative_numbers() {
    let (_d, path) = store_path();
    // --lock is a unique prefix of --lock-timeout; -5 is a value, not an option.
    assert_eq!(
        run(["--lock=1", p(&path), "set", "k", "-5", "--json"]).0,
        EXIT_OK
    );
    let (_, out, _) = run([p(&path), "get", "k"]);
    assert_eq!(out, "-5\n");
    let (code, out, _) = run(["--ver"]);
    assert_eq!(code, 0);
    assert_eq!(out, format!("tongs {VERSION}\n"));
}

#[test]
fn help_output_matches_argparse() {
    let (code, out, err) = run([p(Path::new("x.json")), "set", "--help"]);
    assert_eq!(code, 0);
    assert_eq!(err, "");
    assert_eq!(
        out,
        "usage: tongs file set [-h] [--json] key value

positional arguments:
  key
  value

options:
  -h, --help  show this help message and exit
  --json      parse VALUE as JSON instead of text
"
    );
    let out = Command::cargo_bin("tongs")
        .unwrap()
        .env("COLUMNS", "120")
        .arg("-h")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with(
        "usage: tongs [-h] [--version] [--lock-timeout LOCK_TIMEOUT] file {init,info,dump,get,set,delete} ...\n"
    ));
}

#[test]
fn list_indexes_and_python_int_semantics() {
    let (_d, path) = store_path();
    assert_eq!(
        run([p(&path), "set", "tags", "[\"a\",\"b\",\"c\"]", "--json"]).0,
        0
    );
    assert_eq!(run([p(&path), "get", "tags.-1"]).1, "\"c\"\n");
    assert_eq!(run([p(&path), "set", "tags.-1", "z"]).0, 0);
    assert_eq!(run([p(&path), "delete", "tags.0"]).0, 0);
    assert_eq!(
        run([p(&path), "get", "tags"]).1,
        "[\n  \"b\",\n  \"z\"\n]\n"
    );
    let (code, _, err) = run([p(&path), "set", "tags.9", "x"]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(err, "tongs: key path 'tags.9' not found\n");
}

#[test]
fn info_and_reads_never_write_the_store() {
    let (dir, path) = store_path();
    let (code, _, _) = run([p(&path), "info"]);
    assert_eq!(code, 0);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    let nested = dir.path().join("a").join("b.json");
    assert_eq!(run([p(&nested), "info"]).0, 0);
    assert!(!dir.path().join("a").exists());
}

#[test]
fn lock_timeout_reports_python_message() {
    let (_d, path) = store_path();
    let store = Store::new(&path).unwrap();
    store.save(&json!({})).unwrap();
    let _g = store.lock(true).unwrap();
    let (code, _, err) = run(["--lock-timeout", "0", p(&path), "set", "a", "1"]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(
        err,
        format!(
            "tongs: timed out after 0.0s waiting for exclusive lock on {}\n",
            store.lock_path().display()
        )
    );
}

#[test]
fn unreadable_envelope_and_os_errors() {
    let (dir, path) = store_path();
    fs::write(
        &path,
        json!({"format": FORMAT, "schema_version": 1}).to_string(),
    )
    .unwrap();
    let (code, _, err) = run([p(&path), "dump"]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(
        err,
        format!(
            "tongs: {}: envelope is missing schema_version or data\n",
            p(&path)
        )
    );
    let sub = dir.path().join("dir.json");
    fs::create_dir(&sub).unwrap();
    let (code, _, err) = run([p(&sub), "info"]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(
        err,
        format!("tongs: [Errno 21] Is a directory: '{}'\n", p(&sub))
    );
}

#[test]
fn unicode_decimal_digits_follow_python_int_and_float() {
    // Python's int()/float() accept any Unicode decimal digit (U+0663 is
    // ARABIC-INDIC DIGIT THREE, U+FF11 is FULLWIDTH DIGIT ONE).
    let (_d, path) = store_path();
    assert_eq!(
        run([p(&path), "set", "tags", "[\"a\",\"b\",\"c\"]", "--json"]).0,
        0
    );
    assert_eq!(run([p(&path), "get", "tags.\u{ff11}"]).1, "\"b\"\n");
    assert_eq!(run([p(&path), "get", "tags.-\u{661}"]).1, "\"c\"\n");
    assert_eq!(run([p(&path), "delete", "tags.\u{660}"]).0, 0);
    assert_eq!(
        run([p(&path), "get", "tags"]).1,
        "[\n  \"b\",\n  \"c\"\n]\n"
    );
    let (_d2, fresh) = store_path();
    let (code, out, _) = run([p(&fresh), "init", "--schema-version", "\u{663}"]);
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["schema_version"],
        json!(3)
    );
    assert_eq!(
        run(["--lock-timeout", "\u{ff11}.\u{ff15}", p(&fresh), "info"]).0,
        0
    );
}

#[test]
fn non_utf8_arguments_are_reported_like_python() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let raw = |bytes: &[u8]| OsString::from_vec(bytes.to_vec());
    let missing = dir.path().join(raw(b"st\xffte.json"));
    let shown = format!("{}/st\\udcffte.json", p(dir.path()));
    let (code, _, err) = run([missing.as_os_str(), "dump".as_ref()]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(err, format!("tongs: {shown} does not exist\n"));
    // CPython's stdout is strict UTF-8, so v1 could not print such a path.
    let (code, out, err) = run([missing.as_os_str(), "info".as_ref()]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(out, "");
    let pos = "{\n  \"path\": \"".len() + p(dir.path()).len() + "/st".len();
    assert_eq!(
        err,
        format!(
            "tongs: 'utf-8' codec can't encode character '\\udcff' in position {pos}: surrogates not allowed\n"
        )
    );
    // Keys and repr()s use \udcXX too.
    let (_d, path) = store_path();
    Store::new(&path).unwrap().save(&json!({"s": 1})).unwrap();
    let (code, _, err) = run([path.as_os_str().to_owned(), raw(b"get"), raw(b"k\xfe")]);
    assert_eq!(code, EXIT_MISSING);
    assert_eq!(err, "tongs: k\\udcfe: not found\n");
    let (code, _, err) = run([
        path.as_os_str().to_owned(),
        raw(b"set"),
        raw(b"s.x\xfe"),
        raw(b"v"),
    ]);
    assert_eq!(code, EXIT_ERROR);
    assert_eq!(
        err,
        "tongs: key path 's.x\\udcfe' does not resolve through a scalar\n"
    );
    let (code, _, err) = run([path.as_os_str().to_owned(), raw(b"bog\xfe")]);
    assert_eq!(code, EXIT_USAGE);
    assert!(err.ends_with("invalid choice: 'bog\\udcfe' (choose from 'init', 'info', 'dump', 'get', 'set', 'delete')\n"));
    // A valid UTF-8 name that contains U+10FFFF is not mistaken for an escape.
    let odd = dir.path().join("x\u{10ffff}.json");
    assert_eq!(run([p(&odd), "set", "k", "v"]).0, 0);
    assert!(odd.exists());
}

#[test]
fn deeply_nested_files_are_refused_without_crashing() {
    let (_d, path) = store_path();
    fs::write(&path, "[".repeat(1_000_000)).unwrap();
    let (code, out, _) = run([p(&path), "info"]);
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["schema_version"],
        Value::Null
    );
    let (code, _, err) = run([p(&path), "dump"]);
    assert_eq!(code, EXIT_ERROR);
    assert!(err.ends_with("is not a readable tongs document\n"));
}

// ── Rename to tongs (2.0.0) ────────────────────────────────────────────

const ALIAS_NOTE: &str =
    "atomic-json-store: renamed to tongs; this alias will be removed in the next release\n";

fn run_bin<I, S>(bin: &str, args: I) -> (i32, String, String)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = Command::cargo_bin(bin)
        .unwrap()
        .env_remove("COLUMNS")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn deprecated_alias_prints_one_note_then_behaves_like_tongs() {
    let (_d, path) = store_path();
    let (code, out, err) = run_bin("atomic-json-store", [p(&path), "set", "a.b", "1", "--json"]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (EXIT_OK, "", ALIAS_NOTE)
    );
    let cases: Vec<Vec<&str>> = vec![
        vec!["--version"],
        vec![p(&path), "get", "a.b"],
        vec![p(&path), "get", "missing"],
        vec![p(&path), "dump"],
        vec![p(&path), "bogus"],
        vec![p(&path), "set", "--help"],
        vec![],
    ];
    for args in cases {
        let (tc, tout, terr) = run_bin("tongs", &args);
        let (ac, aout, aerr) = run_bin("atomic-json-store", &args);
        assert_eq!(ac, tc, "{args:?}");
        assert_eq!(aout, tout, "{args:?}");
        assert_eq!(aerr, format!("{ALIAS_NOTE}{terr}"), "{args:?}");
    }
    assert_eq!(
        run_bin("atomic-json-store", ["--version"]).1,
        format!("tongs {VERSION}\n")
    );
}

/// The renamed CLI still writes the envelope Python 1.x (released as
/// atomic-json-store) recognises, byte for byte, and reads one it wrote.
#[test]
fn envelope_on_disk_keeps_the_python_1x_format_string() {
    let (_d, path) = store_path();
    assert_eq!(run([p(&path), "set", "k", "v"]).0, EXIT_OK);
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("{\n  \"format\": \"atomic-json-store/1\",\n  \"schema_version\": 1,\n"),
        "{text}"
    );
    assert!(!text.contains("tongs"));
    // A file written by Python 1.0.1.
    fs::write(
        &path,
        "{\n  \"format\": \"atomic-json-store/1\",\n  \"schema_version\": 1,\n  \"updated_at\": \"2026-09-27T00:00:00.000000+00:00\",\n  \"data\": {\n    \"k\": \"from python\"\n  }\n}\n",
    )
    .unwrap();
    assert_eq!(run([p(&path), "get", "k"]).1, "\"from python\"\n");
    let (_, out, _) = run([p(&path), "info"]);
    assert_eq!(read_json_str(&out)["format"], json!("atomic-json-store/1"));
    assert_eq!(read_json_str(&out)["schema_version"], json!(1));
}

fn read_json_str(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn temp_files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "tmp"))
        .collect()
}

/// An interrupt that arrives while the CLI is publishing a write (its
/// `.state.json.XXXXXXXX.tmp` file exists) is deferred until the rename is
/// done: the process then dies from the signal, the store holds the new
/// document and no temporary file is left behind. Python 1.x cleaned up on
/// Ctrl-C; 2.0.0 must not regress that.
#[test]
fn interrupt_during_a_write_leaves_no_temp_file_and_no_torn_store() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    // A large document widens the publish window so the test can observe the
    // temp file. Disk-backed (not tmpfs) so fsync takes real time.
    let filler = "x".repeat(24 * 1024 * 1024);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let mut hit = false;
        for attempt in 0..20 {
            let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
            let path = dir.path().join("state.json");
            Store::builder(&path)
                .fsync(false)
                .build()
                .unwrap()
                .save(&json!({"filler": filler, "n": 0}))
                .unwrap();
            let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_tongs"))
                .args([p(&path), "set", "n", "1", "--json"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut signalled = false;
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if !signalled && !temp_files(dir.path()).is_empty() {
                    // SAFETY: plain kill(2) on our own child.
                    unsafe { libc::kill(child.id() as libc::pid_t, signal) };
                    signalled = true;
                }
                assert!(Instant::now() < deadline, "CLI did not finish");
            };
            assert!(
                temp_files(dir.path()).is_empty(),
                "temp file left behind (signal {signal}, attempt {attempt})"
            );
            let envelope = read_json(&path);
            assert_eq!(envelope["format"], json!("atomic-json-store/1"));
            assert_eq!(
                envelope["data"]["filler"].as_str().map(str::len),
                Some(filler.len())
            );
            if signalled {
                // The signal landed inside the publish, so the write finished
                // first and the deferred signal then terminated the process.
                assert_eq!(status.signal(), Some(signal), "{status:?}");
                assert_eq!(envelope["data"]["n"], json!(1));
                hit = true;
                break;
            }
            // The write finished before the temp file was seen; try again.
            assert!(status.success());
        }
        assert!(hit, "never caught the CLI mid-write for signal {signal}");
    }
}
