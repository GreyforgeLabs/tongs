# Benchmarks: Python 1.0.1 vs Rust 2.0.0

Python 1.0.1 is the package released as `atomic-json-store`; Rust 2.0.0 is tongs.

Measured on 2026-10-05 on **greyarch** (Linux 7.2.5 x86_64, 12 cores, 16 GB RAM, btrfs on NVMe), with CPython 3.14.7 for 1.0.1 and rustc 1.98.1 for 2.0.0 (release profile: `opt-level = 3`, fat LTO, one codegen unit, stripped, `panic = "abort"`). Numbers are medians of 41 runs after 3 warm-up runs; the store is the two-key document created by `set service.name api` and `set service.port 8080 --json`, kept on the btrfs home volume so `set` pays a real fsync.

## Cold-start wall time

| Command | Python 1.0.1 | Rust 2.0.0 | Speed-up |
|---|---:|---:|---:|
| baseline: `/usr/bin/true` | 1.84 ms | 1.84 ms | - |
| `--version` | 57.44 ms | 2.36 ms | 24x |
| `state.json get service.port` | 56.41 ms | 1.96 ms | 29x |
| `state.json set k v` (fsync file + directory) | 59.18 ms | 5.05 ms | 12x |

The baseline row is the cost of the measurement loop itself (fork/exec plus two `date` calls), so the Rust commands spend well under a millisecond of their own; `set` is dominated by the two fsyncs. Python's time is almost entirely interpreter start-up and imports.

An independent re-run of the same loop on the same host (1-minute load average about 4.5, so slightly noisier) gave 60.2 / 58.3 / 60.6 ms for Python and 2.51 / 2.21 / 4.65 ms for Rust (`--version` / `get` / `set`), with a 1.72-1.88 ms baseline, in line with the table above.

## Peak memory (maximum resident set size)

| Command | Python 1.0.1 | Rust 2.0.0 |
|---|---:|---:|
| baseline: `/usr/bin/true` | 2,324 KB | 2,324 KB |
| `--version` | 17,468 KB | 2,576 KB |
| `state.json get service.port` | 17,412 KB | 2,536 KB |
| `state.json set k v` | 17,864 KB | 2,532 KB |

GNU `time` is not installed on the host, so peak RSS was read with `getrusage(RUSAGE_CHILDREN)` from a tiny spawning helper (source below), the same mechanism `/usr/bin/time -v` uses. Linux carries the parent's pre-`exec` RSS into the child's maximum, which is why even `/usr/bin/true` reports about 2.3 MB: the Rust CLI sits within ~0.25 MB of that floor.

## Installed footprint

| | Python 1.0.1 | Rust 2.0.0 |
|---|---:|---:|
| Package | 23,514 bytes of `.py` source (4 files; 67,652 bytes on disk once `__pycache__` is written) | 578,792-byte stripped binary (`target/release/tongs`; measured before the rename, when it was built as `target/release/atomic-json-store`) |
| Runtime dependency | a CPython >= 3.11 interpreter: here `libpython3.14.so.1.0` (6,360,840 bytes) plus ~31 MB of standard library | none beyond libc (dynamically links `libc.so.6` and `libgcc_s.so.1`) |

## Commands used

```bash
cargo build --release --locked
# Python ran from the v1.0.1 sources:
export PYTHONPATH=/path/to/atomic-json-store-v1.0.1/src
PY="python3 -m atomic_json_store"
RS=target/release/tongs

median() { sort -n | awk '{a[NR]=$1} END {if (NR%2) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2}'; }
bench() {               # bench LABEL COMMAND...
  label=$1; shift
  for _ in 1 2 3; do "$@" >/dev/null 2>&1 || true; done
  for _ in $(seq 41); do
    s=$(date +%s%N); "$@" >/dev/null 2>&1 || true; e=$(date +%s%N)
    echo $(( (e - s) / 1000 ))
  done | median | awk -v l="$label" '{printf "%-34s %8.2f ms\n", l, $1/1000}'
}
$RS state.json set service.name api && $RS state.json set service.port 8080 --json
bench "baseline"              /usr/bin/true
bench "python --version"      $PY --version
bench "rust   --version"      $RS --version
bench "python get"            $PY state.json get service.port
bench "rust   get"            $RS state.json get service.port
bench "python set"            $PY state.json set k v
bench "rust   set"            $RS state.json set k v

# Peak RSS (helper compiled with: rustc -O --edition 2024 -o rss-helper rss.rs)
./rss-helper $PY state.json get service.port
./rss-helper $RS state.json get service.port
stat -c %s $RS
```

`rss.rs`:

```rust
// Minimal /usr/bin/time -v stand-in: run a command, print the child's peak RSS (KB).
#[repr(C)]
struct Rusage { ru_utime: [i64; 2], ru_stime: [i64; 2], ru_maxrss: i64, rest: [i64; 13] }
unsafe extern "C" { fn getrusage(who: i32, usage: *mut Rusage) -> i32; }
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = std::process::Command::new(&args[0]).args(&args[1..])
        .stdout(std::process::Stdio::null()).status().unwrap();
    let mut ru: Rusage = unsafe { std::mem::zeroed() };
    unsafe { getrusage(-1, &mut ru) };
    println!("{} {}", ru.ru_maxrss, status.code().unwrap_or(-1));
}
```
