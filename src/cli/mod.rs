//! The `tongs FILE get|set|delete|dump|info|init` command.

mod argparse;
mod keypath;

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::sync::atomic::{AtomicBool, Ordering};

use tongs::json::{self, DumpOptions, JsonError};
use tongs::{Error, ErrorKind, LEGACY_VERSION, Map, Store, Value, python};

use argparse::{Action, Exit, Kind, Namespace, Parser, SubCommand, Type, Val};
use keypath::{KeyPathError, delete_path, get_path, set_path};

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_MISSING: i32 = 3;

const PROG: &str = "tongs";

/// First code point used to carry bytes of a non-UTF-8 argument through the
/// (string based) parser, so file paths stay byte-exact.
const ESCAPE_BASE: u32 = 0x10_ff00;

/// Set once any argument was not valid UTF-8. Only then are characters in
/// the escape range decoded back into bytes, so a valid argument that
/// happens to contain `U+10FF00..U+10FFFF` is never altered.
static ESCAPES_USED: AtomicBool = AtomicBool::new(false);

fn arg_to_string(arg: OsString) -> String {
    match arg.into_string() {
        Ok(s) => s,
        Err(os) => {
            ESCAPES_USED.store(true, Ordering::Relaxed);
            let bytes = os.into_vec();
            let mut out = String::new();
            let mut rest: &[u8] = &bytes;
            while !rest.is_empty() {
                match std::str::from_utf8(rest) {
                    Ok(s) => {
                        out.push_str(s);
                        break;
                    }
                    Err(e) => {
                        let (good, bad) = rest.split_at(e.valid_up_to());
                        out.push_str(std::str::from_utf8(good).unwrap_or_default());
                        let n = e.error_len().unwrap_or(bad.len());
                        for b in &bad[..n] {
                            out.push(char::from_u32(ESCAPE_BASE + u32::from(*b)).unwrap_or('?'));
                        }
                        rest = &bad[n..];
                    }
                }
            }
            out
        }
    }
}

fn escaped_byte(c: char) -> Option<u8> {
    let u = c as u32;
    (ESCAPES_USED.load(Ordering::Relaxed) && (ESCAPE_BASE..ESCAPE_BASE + 0x100).contains(&u))
        .then(|| (u - ESCAPE_BASE) as u8)
}

/// `repr()` of an argument-derived string as CPython shows it: bytes that
/// were not valid UTF-8 appear as `\udcXX`.
pub(crate) fn repr(s: &str) -> String {
    python::os_str_repr(&string_to_os(s))
}

fn string_to_os(s: &str) -> OsString {
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        match escaped_byte(c) {
            Some(b) => bytes.push(b),
            None => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    OsString::from_vec(bytes)
}

/// Reject arguments that were not valid UTF-8 where the Python CLI would
/// have failed to encode them (keys and values written into the document).
fn ensure_encodable(s: &str) -> Result<(), CliError> {
    for (pos, c) in s.chars().enumerate() {
        if let Some(b) = escaped_byte(c) {
            return Err(CliError::Store(Error::new(
                ErrorKind::Serialize,
                format!(
                    "'utf-8' codec can't encode character '\\udc{b:02x}' in position {pos}: surrogates not allowed"
                ),
            )));
        }
    }
    Ok(())
}

/// The `info` JSON for stdout. CPython's stdout is strict UTF-8, so v1 failed
/// with `UnicodeEncodeError` (exit 1) when the store path was not valid
/// UTF-8; report the same message instead of printing a lossy path.
fn info_json(info: &tongs::StoreInfo) -> Result<Value, CliError> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = info.path.as_os_str().as_bytes();
    if let Some(chunk) = bytes.utf8_chunks().find(|c| !c.invalid().is_empty()) {
        // Offset of the first undecodable byte in the emitted text: the
        // characters before it plus `{\n  "path": "`.
        let start = chunk.valid().as_ptr() as usize - bytes.as_ptr() as usize;
        let before = String::from_utf8_lossy(&bytes[..start + chunk.valid().len()]);
        let escaped = json::dumps(&Value::String(before.into_owned()), &DumpOptions::indent(2));
        let chars_before = escaped.chars().count() - 2;
        let byte = chunk.invalid()[0];
        return Err(CliError::Store(Error::new(
            ErrorKind::Serialize,
            format!(
                "'utf-8' codec can't encode character '\\udc{byte:02x}' in position {}: surrogates not allowed",
                13 + chars_before
            ),
        )));
    }
    Ok(info.to_json())
}

fn build_parser(version: &str, width: usize) -> Parser {
    let mut parser = Parser {
        prog: PROG.to_owned(),
        description: Some("Inspect and edit a tongs document from the shell."),
        version: Some(format!("{PROG} {version}")),
        actions: Vec::new(),
        subcommands: Vec::new(),
    };
    let help = || {
        Action::flag(
            &["-h", "--help"],
            "help",
            Kind::Help,
            "show this help message and exit",
        )
    };
    let sub = |name: &'static str, help_text: &'static str, actions: Vec<Action>| SubCommand {
        name,
        help: help_text,
        parser: Parser {
            prog: String::new(),
            description: None,
            version: None,
            actions,
            subcommands: Vec::new(),
        },
    };
    let mut subcommands = vec![
        sub(
            "init",
            "create the store if it does not exist",
            vec![
                help(),
                Action::option(
                    "--schema-version",
                    "schema_version",
                    Type::Int,
                    Val::Int(1),
                    None,
                ),
            ],
        ),
        sub("info", "print envelope metadata as JSON", vec![help()]),
        sub("dump", "print the document as JSON", vec![help()]),
        sub(
            "get",
            "print the value at a dotted key path",
            vec![
                help(),
                Action::positional("key", None),
                Action::option(
                    "--default",
                    "default",
                    Type::Str,
                    Val::None,
                    Some("JSON value to print when the key is missing"),
                ),
            ],
        ),
        sub(
            "set",
            "write a value at a dotted key path",
            vec![
                help(),
                Action::positional("key", None),
                Action::positional("value", None),
                Action::flag(
                    &["--json"],
                    "json",
                    Kind::StoreTrue,
                    "parse VALUE as JSON instead of text",
                ),
            ],
        ),
        sub(
            "delete",
            "remove a dotted key path",
            vec![help(), Action::positional("key", None)],
        ),
    ];
    let mut command = Action::positional("command", None);
    command.kind = Kind::Subcommand;
    command.nargs = argparse::Nargs::Parser;
    parser.actions = vec![
        help(),
        Action::flag(
            &["--version"],
            "version",
            Kind::Version,
            "show program's version number and exit",
        ),
        Action::option(
            "--lock-timeout",
            "lock_timeout",
            Type::Float,
            Val::Float(10.0),
            Some("seconds to wait for the store lock (default: 10)"),
        ),
        Action::positional("file", Some("path to the JSON store")),
        command,
    ];
    let prefix = parser.subcommand_prefix(width);
    for sub in &mut subcommands {
        sub.parser.prog = format!("{prefix} {}", sub.name);
    }
    parser.subcommands = subcommands;
    parser
}

enum CliError {
    Json(JsonError),
    Store(Error),
    KeyPath(KeyPathError),
}

impl From<Error> for CliError {
    fn from(e: Error) -> Self {
        CliError::Store(e)
    }
}

impl From<JsonError> for CliError {
    fn from(e: JsonError) -> Self {
        CliError::Json(e)
    }
}

impl From<KeyPathError> for CliError {
    fn from(e: KeyPathError) -> Self {
        CliError::KeyPath(e)
    }
}

struct Io<'a> {
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
}

impl Io<'_> {
    fn emit(&mut self, value: &Value) -> Result<(), CliError> {
        let mut text = json::dumps(value, &DumpOptions::indent(2));
        text.push('\n');
        self.out
            .write_all(text.as_bytes())
            .and_then(|()| self.out.flush())
            .map_err(|e| CliError::Store(Error::io(e, None, None)))
    }

    /// Write to stderr the way CPython does (`backslashreplace`): bytes of a
    /// non-UTF-8 argument appear as `\udcXX`.
    fn eprint(&mut self, text: &str) {
        let text = python::os_str_display(&string_to_os(text));
        let _ = self.err.write_all(text.as_bytes());
        let _ = self.err.flush();
    }
}

fn str_arg<'n>(ns: &'n Namespace, key: &str) -> Option<&'n str> {
    match ns.get(key) {
        Some(Val::Str(s)) => Some(s),
        _ => None,
    }
}

fn store_at(file: &str, schema_version: u64, lock_timeout: f64) -> Result<Store, Error> {
    Store::builder(string_to_os(file))
        .schema_version(schema_version)
        .lock_timeout_secs(Some(lock_timeout))
        .build()
}

/// `_open`: adopt the schema version the file already carries.
fn open_store(file: &str, lock_timeout: f64, create_version: Option<u64>) -> Result<Store, Error> {
    let probe = store_at(file, 1, lock_timeout)?;
    let info = probe.info()?;
    let version = if info.exists {
        info.schema_version.ok_or_else(|| {
            Error::new(
                ErrorKind::Store,
                format!("{file} is not a readable tongs document"),
            )
        })?
    } else if let Some(v) = create_version {
        v
    } else {
        return Err(Error::new(
            ErrorKind::Store,
            format!("{file} does not exist"),
        ));
    };
    if version == LEGACY_VERSION && info.format.is_none() && info.exists {
        // Adopt a plain JSON file at version 0 without forcing a migration.
        return store_at(file, LEGACY_VERSION, lock_timeout);
    }
    store_at(file, version, lock_timeout)
}

fn run(ns: &Namespace, io: &mut Io<'_>) -> Result<i32, CliError> {
    let file = str_arg(ns, "file").unwrap_or_default();
    let lock_timeout = match ns.get("lock_timeout") {
        Some(Val::Float(f)) => *f,
        _ => 10.0,
    };
    let command = str_arg(ns, "command").unwrap_or_default();
    match command {
        "init" => {
            let requested = match ns.get("schema_version") {
                Some(Val::Int(n)) => *n,
                _ => 1,
            };
            if requested < 0 {
                return Err(Error::new(ErrorKind::Store, "--schema-version must be >= 0").into());
            }
            let version = u64::try_from(requested).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidArgument,
                    "--schema-version is larger than 18446744073709551615",
                )
            })?;
            let store = store_at(file, version, lock_timeout)?;
            if !store.exists() {
                store.save(&Value::Object(Map::new()))?;
            }
            io.emit(&info_json(&store.info()?)?)?;
            Ok(EXIT_OK)
        }
        "info" => {
            let info = store_at(file, 1, lock_timeout)?.info()?;
            io.emit(&info_json(&info)?)?;
            Ok(EXIT_OK)
        }
        "dump" => {
            let data = open_store(file, lock_timeout, None)?.load()?;
            io.emit(&data)?;
            Ok(EXIT_OK)
        }
        "get" => {
            let key = str_arg(ns, "key").unwrap_or_default();
            let data = open_store(file, lock_timeout, None)?.load()?;
            match get_path(&data, key) {
                Ok(v) => io.emit(v)?,
                Err(_) => match str_arg(ns, "default") {
                    None => {
                        io.eprint(&format!("{PROG}: {key}: not found\n"));
                        return Ok(EXIT_MISSING);
                    }
                    Some(default) => {
                        let value = json::loads(default)?;
                        ensure_encodable(default)?;
                        io.emit(&value)?;
                    }
                },
            }
            Ok(EXIT_OK)
        }
        "set" => {
            let key = str_arg(ns, "key").unwrap_or_default();
            let raw = str_arg(ns, "value").unwrap_or_default();
            let value = if matches!(ns.get("json"), Some(Val::Bool(true))) {
                json::loads(raw)?
            } else {
                Value::String(raw.to_owned())
            };
            let store = open_store(file, lock_timeout, Some(1))?;
            store.transaction(|data| -> Result<(), CliError> {
                set_path(data, key, value)?;
                ensure_encodable(key)?;
                ensure_encodable(raw)?;
                Ok(())
            })?;
            Ok(EXIT_OK)
        }
        "delete" => {
            let key = str_arg(ns, "key").unwrap_or_default();
            let store = open_store(file, lock_timeout, None)?;
            match store.transaction(|data| delete_path(data, key).map_err(CliError::from)) {
                Err(CliError::KeyPath(_)) => {
                    io.eprint(&format!("{PROG}: {key}: not found\n"));
                    Ok(EXIT_MISSING)
                }
                other => other.map(|()| EXIT_OK),
            }
        }
        other => Err(Error::new(ErrorKind::Store, format!("unknown command {other}")).into()),
    }
}

/// Run the CLI with `args` (without the program name) and return the exit
/// code.
pub fn main(args: Vec<OsString>) -> i32 {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    let mut io = Io {
        out: &mut out,
        err: &mut err,
    };
    let args: Vec<String> = args.into_iter().map(arg_to_string).collect();
    let width = (argparse::terminal_columns() as isize - 2).max(1) as usize;
    let parser = build_parser(env!("CARGO_PKG_VERSION"), width);
    let ns = match parser.parse_args(&args, width) {
        Ok(ns) => ns,
        Err(Exit {
            code,
            stdout,
            stderr,
        }) => {
            let _ = io.out.write_all(stdout.as_bytes());
            let _ = io.out.flush();
            io.eprint(&stderr);
            return code;
        }
    };
    match run(&ns, &mut io) {
        Ok(code) => code,
        Err(CliError::Json(e)) => {
            io.eprint(&format!("{PROG}: invalid JSON value: {e}\n"));
            EXIT_USAGE
        }
        Err(CliError::Store(e)) => {
            io.eprint(&format!("{PROG}: {e}\n"));
            EXIT_ERROR
        }
        Err(CliError::KeyPath(e)) => {
            io.eprint(&format!("{PROG}: {e}\n"));
            EXIT_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_utf8_arguments_round_trip() {
        let raw = OsString::from_vec(b"st\xffate.json".to_vec());
        let s = arg_to_string(raw.clone());
        assert_eq!(string_to_os(&s), raw);
        assert!(ensure_encodable(&s).is_err());
        assert!(ensure_encodable("fine").is_ok());
    }

    #[test]
    fn help_matches_python_argparse_at_80_columns() {
        let parser = build_parser("2.0.0", 78);
        let want = "usage: tongs [-h] [--version] [--lock-timeout LOCK_TIMEOUT]
             file {init,info,dump,get,set,delete} ...

Inspect and edit a tongs document from the shell.

positional arguments:
  file                  path to the JSON store
  {init,info,dump,get,set,delete}
    init                create the store if it does not exist
    info                print envelope metadata as JSON
    dump                print the document as JSON
    get                 print the value at a dotted key path
    set                 write a value at a dotted key path
    delete              remove a dotted key path

options:
  -h, --help            show this help message and exit
  --version             show program's version number and exit
  --lock-timeout LOCK_TIMEOUT
                        seconds to wait for the store lock (default: 10)
";
        assert_eq!(parser.format_help(78), want);
    }
}
