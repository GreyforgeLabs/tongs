//! Differential and interoperability tests against the Python 1.x
//! implementation, which was released as the `atomic-json-store` package
//! (module `atomic_json_store`). They run only when `AJS_PYTHON_REF` points at
//! that package's `src` directory (e.g. a checkout of v1.0.1), and are skipped
//! otherwise; `AJS_PYTHON` selects the interpreter (default `python3`). The
//! variable names keep the `AJS_` prefix because they name the Python
//! reference, not this crate.
//!
//! ```sh
//! AJS_PYTHON_REF=/path/to/atomic-json-store-1.0.1/src cargo test --test differential
//! ```
//!
//! The CLI was renamed from `atomic-json-store` to `tongs` in 2.0.0, so CLI
//! output is compared with the program name normalised (see [`normalize_prog`]);
//! files are compared byte for byte, including the unchanged
//! `"atomic-json-store/1"` envelope marker.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use tongs::{Store, Value, json};

fn reference() -> Option<PathBuf> {
    let path = std::env::var_os("AJS_PYTHON_REF")?;
    let path = PathBuf::from(path);
    path.join("atomic_json_store").is_dir().then_some(path)
}

fn python() -> String {
    std::env::var("AJS_PYTHON").unwrap_or_else(|_| "python3".to_owned())
}

fn rust_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tongs"))
}

fn normalize(text: &str) -> String {
    // Replace ISO timestamps and the version string, which legitimately differ.
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        if rest.len() >= 32
            && rest.as_bytes()[4] == b'-'
            && rest.as_bytes()[10] == b'T'
            && rest[26..32] == *"+00:00"
            && rest[..4].bytes().all(|b| b.is_ascii_digit())
        {
            out.push_str("<TS>");
            i += 32;
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out.replace("1.0.1", "<V>")
        .replace("1.0.0", "<V>")
        .replace(env!("CARGO_PKG_VERSION"), "<V>")
}

/// Map the program name to `<PROG>` in CLI output: Python 1.x says
/// `atomic-json-store` ("an atomic-json-store document"), the Rust CLI says
/// `tongs` ("a tongs document"). argparse indents continuation lines of a
/// wrapped usage message to line up after the program name, so those indents
/// are normalised as well. The `"atomic-json-store/1"` envelope marker in
/// `info` output is file format, not the program name, and stays as is.
fn normalize_prog(text: &str) -> String {
    const MARKER: &str = "\u{0}FORMAT\u{0}";
    let text = text
        .replace("atomic-json-store/1", MARKER)
        .replace("an atomic-json-store document", "a <PROG> document")
        .replace("a tongs document", "a <PROG> document")
        .replace("atomic-json-store", "<PROG>")
        .replace("tongs", "<PROG>")
        .replace(MARKER, "atomic-json-store/1");
    let mut out = String::with_capacity(text.len());
    let mut in_usage = false;
    for line in text.split_inclusive('\n') {
        if line.starts_with("usage: ") {
            in_usage = true;
            out.push_str(line);
        } else if in_usage && line.starts_with(' ') {
            out.push_str("<INDENT>");
            out.push_str(line.trim_start_matches(' '));
        } else {
            in_usage = false;
            out.push_str(line);
        }
    }
    out
}

fn snapshot(dir: &Path) -> Vec<(String, u32, String)> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if meta.is_dir() {
                stack.push(path);
                out.push((rel, meta.permissions().mode() & 0o777, "<dir>".to_owned()));
            } else {
                let body = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
                out.push((rel, meta.permissions().mode() & 0o777, normalize(&body)));
            }
        }
    }
    out.sort();
    out
}

type Outcome = (Option<i32>, String, String, Vec<(String, u32, String)>);

fn run_impl(python_impl: bool, src: &Path, args: &[&str], setup: &dyn Fn(&Path)) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let mut cmd = if python_impl {
        let mut c = Command::new(python());
        c.args(["-m", "atomic_json_store"]).env("PYTHONPATH", src);
        c
    } else {
        Command::new(rust_bin())
    };
    let out = cmd
        .args(args)
        .current_dir(dir.path())
        .env_remove("COLUMNS")
        .output()
        .unwrap();
    (
        out.status.code(),
        normalize_prog(&normalize(&String::from_utf8_lossy(&out.stdout))),
        normalize_prog(&normalize(&String::from_utf8_lossy(&out.stderr))),
        snapshot(dir.path()),
    )
}

const ENVELOPE: &str = "{\n  \"format\": \"atomic-json-store/1\",\n  \"schema_version\": 1,\n  \"updated_at\": \"2026-01-01T00:00:00.000000+00:00\",\n  \"data\": {\"a\": 1, \"list\": [1, {\"x\": null}], \"s\": \"Z\\u00fcrich\", \"f\": 1e-07, \"big\": 123456789012345678901234567890}\n}\n";

#[test]
fn prog_normalisation_only_touches_the_program_name() {
    let py = "usage: atomic-json-store [-h] [--version] [--lock-timeout LOCK_TIMEOUT]\n                         file {init,info,dump,get,set,delete} ...\natomic-json-store: error: x\n\"format\": \"atomic-json-store/1\"\n";
    let rs = "usage: tongs [-h] [--version] [--lock-timeout LOCK_TIMEOUT]\n             file {init,info,dump,get,set,delete} ...\ntongs: error: x\n\"format\": \"atomic-json-store/1\"\n";
    assert_eq!(normalize_prog(py), normalize_prog(rs));
    assert!(normalize_prog(rs).ends_with("\"format\": \"atomic-json-store/1\"\n"));
    assert_ne!(
        normalize_prog("\"format\": \"atomic-json-store/1\""),
        normalize_prog("\"format\": \"tongs/1\"")
    );
}

#[test]
fn cli_matches_python_byte_for_byte() {
    let Some(src) = reference() else {
        eprintln!("AJS_PYTHON_REF not set; skipping differential test");
        return;
    };
    let write = |name: &'static str, body: &'static str| {
        move |d: &Path| fs::write(d.join(name), body).unwrap()
    };
    let none = |_: &Path| {};
    type Setup = Box<dyn Fn(&Path)>;
    let setups: Vec<(&str, Setup)> = vec![
        ("missing", Box::new(none)),
        ("envelope", Box::new(write("s.json", ENVELOPE))),
        (
            "plain",
            Box::new(write("s.json", "{\"plain\": true, \"n\": [1, 2]}")),
        ),
        ("corrupt", Box::new(write("s.json", "nope"))),
        ("list", Box::new(write("s.json", "[1, 2, 3]"))),
        (
            "nan",
            Box::new(write(
                "s.json",
                "{\"a\": NaN, \"b\": -Infinity, \"c\": 1E2}",
            )),
        ),
    ];
    let argvs: Vec<Vec<&str>> = vec![
        vec![],
        vec!["-h"],
        vec!["s.json", "get", "-h"],
        vec!["s.json", "bogus"],
        vec!["s.json", "info"],
        vec!["s.json", "dump"],
        vec!["s.json", "init", "--schema-version", "3"],
        vec!["s.json", "get", "a"],
        vec!["s.json", "get", "list.-1"],
        vec![
            "s.json",
            "get",
            "missing",
            "--default",
            "{\"d\": [1.5, 1e16]}",
        ],
        vec!["s.json", "get", "missing", "--default", "{bad"],
        vec!["s.json", "set", "a.b", "1", "--json"],
        vec!["s.json", "set", "new.deep", "ünï ✓"],
        vec![
            "s.json",
            "set",
            "k",
            "[1e-5, -0.0, 12345678901234567890123]",
            "--json",
        ],
        vec!["s.json", "delete", "a"],
        vec!["s.json", "delete", "missing"],
        vec!["--lock=2", "s.json", "set", "k", "-5", "--json"],
        vec!["s.json", "info", "--extra"],
        vec!["sub/dir/s.json", "set", "k", "v"],
    ];
    let mut mismatches = Vec::new();
    for (name, setup) in &setups {
        for argv in &argvs {
            let py = run_impl(true, &src, argv, setup.as_ref());
            let rs = run_impl(false, &src, argv, setup.as_ref());
            if py != rs {
                mismatches.push(format!("{name} {argv:?}\n  py: {py:?}\n  rs: {rs:?}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

fn py_store(src: &Path, code: &str, arg: &Path, doc: &str) {
    let out = Command::new(python())
        .args(["-c", code])
        .arg(arg)
        .arg(doc)
        .env("PYTHONPATH", src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn files_written_by_either_implementation_are_identical() {
    let Some(src) = reference() else {
        return;
    };
    let doc = tongs::json::loads(
        r#"{"z": "é\u2028𝄞\u0001", "a": [1, 2.5, 1e-7, 1e16, null, true, -0.0], "o": {}, "big": 18446744073709551616, "n": NaN}"#,
    )
    .unwrap();
    let doc_text = tongs::json::dumps(&doc, &Default::default());
    let configs: [(&str, Option<usize>, bool, bool); 4] = [
        ("indent=2", Some(2), false, false),
        (
            "indent=None, sort_keys=True, ensure_ascii=True",
            None,
            true,
            true,
        ),
        ("indent=0", Some(0), false, false),
        ("indent=4, sort_keys=True", Some(4), true, false),
    ];
    for (kwargs, indent, sort_keys, ensure_ascii) in configs {
        let dir = tempfile::tempdir().unwrap();
        let py_path = dir.path().join("py.json");
        let rs_path = dir.path().join("rs.json");
        let code = format!(
            "import sys, json\nfrom atomic_json_store import AtomicJsonStore\nAtomicJsonStore(sys.argv[1], {kwargs}).save(json.loads(sys.argv[2]))"
        );
        py_store(&src, &code, &py_path, &doc_text);
        Store::builder(&rs_path)
            .indent(indent)
            .sort_keys(sort_keys)
            .ensure_ascii(ensure_ascii)
            .build()
            .unwrap()
            .save(&doc)
            .unwrap();
        let py = normalize(&fs::read_to_string(&py_path).unwrap());
        let rs = normalize(&fs::read_to_string(&rs_path).unwrap());
        assert_eq!(py, rs, "{kwargs}");
        // And each implementation reads the other's file.
        let expected = tongs::json::dumps(
            &doc,
            &tongs::json::DumpOptions {
                sort_keys,
                ..Default::default()
            },
        );
        let back = Store::new(&py_path).unwrap().load().unwrap();
        assert_eq!(tongs::json::dumps(&back, &Default::default()), expected);
        py_store(
            &src,
            "import sys, json\nfrom atomic_json_store import AtomicJsonStore\nd = AtomicJsonStore(sys.argv[1]).load()\nassert json.dumps(d, indent=2, ensure_ascii=False) == sys.argv[2], d",
            &rs_path,
            &expected,
        );
    }
}

/// Helper process: increments `n` with the Rust library.
#[test]
#[ignore = "helper process for mixed_processes_never_lose_increments"]
fn rust_increment_worker() {
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
fn mixed_processes_never_lose_increments() {
    let Some(src) = reference() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.json");
    let rounds = 40;
    let py_worker = "import sys\nfrom atomic_json_store import AtomicJsonStore\nstore = AtomicJsonStore(sys.argv[1], default=lambda: {'n': 0}, lock_timeout=60, fsync=False)\ndef bump(d):\n    d['n'] += 1\nfor _ in range(int(sys.argv[2])):\n    store.update(bump)\n";
    let mut children = Vec::new();
    for _ in 0..3 {
        children.push(
            Command::new(python())
                .args(["-c", py_worker])
                .arg(&path)
                .arg(rounds.to_string())
                .env("PYTHONPATH", &src)
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "rust_increment_worker",
                    "--test-threads=1",
                ])
                .env("TONGS_WORKER_PATH", &path)
                .env("TONGS_WORKER_ROUNDS", rounds.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
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
        json!({"n": 6 * rounds})
    );
}

#[test]
fn python_lock_blocks_rust_and_rust_lock_blocks_python() {
    let Some(src) = reference() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.json");
    Store::new(&path).unwrap().save(&json!({})).unwrap();

    // Python holds the exclusive lock; the Rust CLI must time out.
    let holder = "import sys, time\nfrom atomic_json_store import AtomicJsonStore\ns = AtomicJsonStore(sys.argv[1])\nwith s._lock.held(exclusive=True):\n    print('held', flush=True)\n    time.sleep(3)\n";
    let mut child = Command::new(python())
        .args(["-c", holder])
        .arg(&path)
        .env("PYTHONPATH", &src)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(child.stdout.as_mut().unwrap()),
        &mut line,
    )
    .unwrap();
    assert_eq!(line.trim(), "held");
    let out = Command::new(rust_bin())
        .args(["--lock-timeout", "0.1"])
        .arg(&path)
        .args(["set", "a", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("timed out after 0.1s waiting for exclusive lock")
    );
    child.kill().ok();
    child.wait().ok();

    // Rust holds the exclusive lock; Python must time out.
    let store = Store::new(&path).unwrap();
    let _g = store.lock(true).unwrap();
    let out = Command::new(python())
        .args(["-m", "atomic_json_store", "--lock-timeout", "0.1"])
        .arg(&path)
        .args(["set", "a", "1"])
        .env("PYTHONPATH", &src)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("timed out after 0.1s waiting for exclusive lock")
    );
}
