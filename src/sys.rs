//! Filesystem and OS primitives: advisory `flock`, `mkdir -p` with Python's
//! error reporting, private temporary files, directory fsync, deferral of
//! terminating signals and UTC time.
//!
//! `flock(2)` is called through the `libc` crate because the crate's minimum
//! Rust version (1.88) predates `std::fs::File::try_lock`, and
//! `pthread_sigmask(3)` has no `std` equivalent. Those calls (in this module)
//! are the only `unsafe` code in the library.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::python::path_parent;

/// Try to take a non-blocking `flock` (`LOCK_SH` or `LOCK_EX`). Returns
/// `Ok(false)` when the lock is held elsewhere, matching the Python
/// implementation, which treated `BlockingIOError` and `PermissionError` as
/// "busy".
pub(crate) fn try_flock(file: &File, exclusive: bool) -> io::Result<bool> {
    let op = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    } | libc::LOCK_NB;
    loop {
        // SAFETY: `file` owns a valid open descriptor for the duration of the
        // call; flock does not touch memory.
        let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
        if rc == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(code)
                if code == libc::EWOULDBLOCK
                    || code == libc::EAGAIN
                    || code == libc::EALREADY
                    || code == libc::EINPROGRESS
                    || code == libc::EACCES
                    || code == libc::EPERM =>
            {
                return Ok(false);
            }
            _ => return Err(err),
        }
    }
}

/// Release a `flock`. Closing the descriptor releases it as well.
pub(crate) fn unflock(file: &File) {
    // SAFETY: as above; the result is ignored because the descriptor is about
    // to be closed, which releases the lock regardless.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

/// `Path.mkdir(parents=True, exist_ok=True)` with the same recursion and the
/// same failing-path reporting as CPython's pathlib.
pub(crate) fn mkdir_p(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = path_parent(path);
            if parent == path {
                return Err(Error::io(e, Some(path), None));
            }
            mkdir_p(&parent)?;
            match fs::create_dir(path) {
                Ok(()) => Ok(()),
                Err(e) if path.is_dir() => {
                    let _ = e;
                    Ok(())
                }
                Err(e) => Err(Error::io(e, Some(path), None)),
            }
        }
        Err(e) => {
            if path.is_dir() {
                Ok(())
            } else {
                Err(Error::io(e, Some(path), None))
            }
        }
    }
}

const TMP_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";

fn random_suffix(attempt: u64) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u64(attempt);
    hasher.write_u32(std::process::id());
    if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
        hasher.write_u128(d.as_nanos());
    }
    let mut bits = hasher.finish();
    (0..8)
        .map(|_| {
            let c = TMP_CHARS[(bits % TMP_CHARS.len() as u64) as usize] as char;
            bits /= TMP_CHARS.len() as u64;
            c
        })
        .collect()
}

/// `tempfile.mkstemp(prefix, suffix, dir)`: create a new private (0600) file
/// with a random name, retrying on collisions.
pub(crate) fn mkstemp(dir: &Path, prefix: &[u8], suffix: &str) -> Result<(File, PathBuf)> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let base = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut last_err = None;
    for attempt in 0..10_000u64 {
        let mut name = prefix.to_vec();
        name.extend_from_slice(random_suffix(attempt).as_bytes());
        name.extend_from_slice(suffix.as_bytes());
        let path = base.join(OsString::from_vec(name));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_err = Some((e, path)),
            Err(e) => return Err(Error::io(e, Some(&path), None)),
        }
    }
    let (e, path) = last_err.expect("at least one attempt");
    Err(Error::io(e, Some(&path), None))
}

/// Signals whose default action terminates the process and that a user or a
/// supervisor sends to interrupt it: `SIGHUP` (hang-up), `SIGINT` (Ctrl-C),
/// `SIGQUIT` (Ctrl-Backslash) and `SIGTERM` (`kill`).
const DEFERRED_SIGNALS: [libc::c_int; 4] =
    [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

/// Blocks [`DEFERRED_SIGNALS`] on the calling thread until dropped, then
/// restores the previous signal mask exactly.
///
/// A store write holds one of these from creating its temporary file until the
/// rename (and directory fsync) are done, so an interrupt can neither leave a
/// `.<name>.XXXXXXXX.tmp` file behind nor stop a publish half way: a signal
/// that arrives meanwhile stays pending and is delivered, with whatever
/// disposition the program has set, as soon as the write has finished or has
/// cleaned up after a failure. Nested deferrals are harmless: the inner one
/// restores the (still blocking) outer mask.
///
/// The mask is per thread. A signal sent to the whole process can still be
/// delivered to another thread that does not block it, so multi-threaded
/// programs that need the same guarantee should block these signals in every
/// thread (for example in `main` before spawning) and handle them on a
/// dedicated thread. The `tongs` CLI is single-threaded.
pub(crate) struct SignalDeferral {
    previous: Option<libc::sigset_t>,
}

impl SignalDeferral {
    /// Start deferring. If the mask cannot be changed (which `pthread_sigmask`
    /// only reports for invalid arguments) the write proceeds undeferred.
    pub(crate) fn begin() -> Self {
        let mut set = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        let mut previous = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: `sigemptyset` initialises `set` before `sigaddset` and
        // `pthread_sigmask` read it; `pthread_sigmask` writes the old mask into
        // `previous`, which is only assumed initialised when it succeeded.
        unsafe {
            if libc::sigemptyset(set.as_mut_ptr()) != 0 {
                return SignalDeferral { previous: None };
            }
            for signal in DEFERRED_SIGNALS {
                libc::sigaddset(set.as_mut_ptr(), signal);
            }
            if libc::pthread_sigmask(libc::SIG_BLOCK, set.as_ptr(), previous.as_mut_ptr()) != 0 {
                return SignalDeferral { previous: None };
            }
            SignalDeferral {
                previous: Some(previous.assume_init()),
            }
        }
    }
}

impl Drop for SignalDeferral {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            // SAFETY: `previous` is the mask `pthread_sigmask` returned in
            // `begin`; restoring it delivers any signal that became pending.
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
            }
        }
    }
}

/// fsync a directory so a rename inside it is durable.
pub(crate) fn fsync_directory(path: &Path) -> Result<()> {
    let dir = File::open(path).map_err(|e| Error::io(e, Some(path), None))?;
    dir.sync_all().map_err(|e| Error::io(e, None, None))
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (y, m, d).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Broken-down UTC time with microseconds.
pub(crate) struct UtcNow {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    micros: u32,
}

impl UtcNow {
    pub(crate) fn now() -> Self {
        let (secs, micros) = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, d.subsec_micros()),
            Err(e) => {
                let d = e.duration();
                let total = -(d.as_micros() as i128);
                (
                    total.div_euclid(1_000_000) as i64,
                    total.rem_euclid(1_000_000) as u32,
                )
            }
        };
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        UtcNow {
            year,
            month,
            day,
            hour: rem / 3600,
            minute: rem / 60 % 60,
            second: rem % 60,
            micros,
        }
    }

    /// `datetime.now(UTC).isoformat(timespec="microseconds")`.
    pub(crate) fn isoformat(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}+00:00",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.micros
        )
    }

    /// `strftime("%Y%m%dT%H%M%S%fZ")`.
    pub(crate) fn compact(&self) -> String {
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}{:06}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.micros
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_format_shapes() {
        let now = UtcNow::now();
        let iso = now.isoformat();
        assert_eq!(iso.len(), "2026-09-06T21:14:03.512345+00:00".len());
        assert!(iso.ends_with("+00:00"));
        assert_eq!(now.compact().len(), "20260906T211403512345Z".len());
    }

    #[test]
    fn civil_dates_are_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(20_702), (2026, 9, 6));
    }
}
