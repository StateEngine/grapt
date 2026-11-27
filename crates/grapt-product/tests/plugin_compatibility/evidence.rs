//! Bounded failure evidence for the product journeys: a snapshot of a live
//! product, its direct children and, for plugin products, its runtime files.
//! Shared by the plugin compatibility and handover journeys. Each keeps its own
//! product lifecycle, fixtures and assertions.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use tokio::time::Instant;

/// Snapshot bounds: the product plus at most `SNAPSHOT_PROCESSES - 1` direct
/// children, at most `SNAPSHOT_THREADS` threads each, `SNAPSHOT_READ_BYTES` per
/// `/proc` read, and `SNAPSHOT_BUDGET` for the whole snapshot.
const SNAPSHOT_PROCESSES: usize = 8;
const SNAPSHOT_THREADS: usize = 256;
const SNAPSHOT_READ_BYTES: u64 = 512;
const SNAPSHOT_BUDGET: Duration = Duration::from_secs(2);
/// The product's CPU and IO counters are read twice, this far apart.
const ACTIVITY_WINDOW: Duration = Duration::from_millis(500);
/// Host memory evidence reads at most this much of `/proc/meminfo`.
const MEMINFO_BYTES: u64 = 4096;

/// Host IO pressure, then per process (the product and its direct children, such
/// as the plugin host) each thread's state, kernel wait channel and current
/// syscall, counted by kind. A file descriptor is resolved only for a syscall
/// whose first argument is one. Then the product's CPU and IO deltas over
/// `ACTIVITY_WINDOW`, and, when a runtime directory is given, the files plugin
/// runtime admission has left there. Read while the product still exists.
pub(super) fn product_snapshot(product: u32, runtime: Option<&Path>) -> String {
    let started = Instant::now();
    let io = proc_read("/proc/pressure/io".as_ref(), "missing");
    let first = activity(product);
    let mut processes = vec![process_snapshot(product, None, started)];
    let mut children = std::collections::BTreeSet::<u32>::new();
    let mut unlisted = std::collections::BTreeMap::<String, usize>::new();
    match fs::read_dir(format!("/proc/{product}/task")) {
        Ok(tasks) => {
            for task in tasks.take(SNAPSHOT_THREADS).flatten() {
                match proc_read(&task.path().join("children"), "disappeared") {
                    Ok(listed) => children.extend(
                        listed
                            .split_whitespace()
                            .filter_map(|pid| pid.parse::<u32>().ok()),
                    ),
                    Err(error) => *unlisted.entry(error).or_default() += 1,
                }
            }
        }
        Err(error) => {
            unlisted.insert(proc_error(&error, "missing"), 0);
        }
    }
    let listed = children.len();
    for pid in children.into_iter().take(SNAPSHOT_PROCESSES - 1) {
        processes.push(process_snapshot(pid, Some(product), started));
    }
    format!(
        "io_pressure={}; host_writeback={}; processes(product + {listed} direct children, <= {SNAPSHOT_PROCESSES})=[{}]; \
         children_unlisted={unlisted:?}; product_delta(cpu_ticks,rchar,wchar,read_bytes,write_bytes)={}; \
         runtime_files={}; snapshot_ms={}",
        io.unwrap_or_else(|error| error),
        host_writeback(),
        processes.join(" | "),
        activity_delta(first, product, started),
        runtime.map_or_else(|| "not-applicable".to_owned(), runtime_files),
        started.elapsed().as_millis()
    )
}

/// The host's dirty and under-writeback page totals, read at the stall: memory
/// waiting to be flushed that a journal commit may have to wait behind.
fn host_writeback() -> String {
    let mut bytes = Vec::new();
    match fs::File::open("/proc/meminfo")
        .and_then(|file| file.take(MEMINFO_BYTES).read_to_end(&mut bytes))
    {
        Ok(read) => writeback_fields(
            &String::from_utf8_lossy(&bytes),
            u64::try_from(read).map_or(true, |read| read >= MEMINFO_BYTES),
        ),
        Err(error) => proc_error(&error, "absent"),
    }
}

/// `Dirty` and `Writeback` from a bounded `/proc/meminfo` read. A field missing
/// from a truncated read is reported as truncated, not as absent.
fn writeback_fields(meminfo: &str, truncated: bool) -> String {
    let field = |name: &str| {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|| (if truncated { "truncated" } else { "absent" }).to_owned())
    };
    format!(
        "dirty={} writeback={}",
        field("Dirty:"),
        field("Writeback:")
    )
}

/// The second activity reading, `ACTIVITY_WINDOW` after the snapshot began, less
/// the first.
fn activity_delta(first: Result<[u64; 5], String>, pid: u32, started: Instant) -> String {
    thread::sleep(ACTIVITY_WINDOW.saturating_sub(started.elapsed()));
    match (first, activity(pid)) {
        (Ok(first), Ok(second)) => format!(
            "{:?} over >={}ms",
            std::array::from_fn::<u64, 5, _>(|index| second[index].saturating_sub(first[index])),
            ACTIVITY_WINDOW.as_millis()
        ),
        (first, second) => format!("unavailable: {first:?} / {second:?}"),
    }
}

/// A process's CPU ticks (utime + stime), then its rchar, wchar, read_bytes and
/// write_bytes counters.
fn activity(pid: u32) -> Result<[u64; 5], String> {
    let stat = proc_read(format!("/proc/{pid}/stat").as_ref(), "disappeared")?;
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    let tick = |index: usize| {
        fields
            .get(index)
            .and_then(|field| field.parse::<u64>().ok())
    };
    let io = proc_read(format!("/proc/{pid}/io").as_ref(), "disappeared")?;
    let counter = |name: &str| {
        io.lines()
            .find_map(|line| line.strip_prefix(name)?.trim().parse::<u64>().ok())
            .ok_or_else(|| format!("io {name} unparsed"))
    };
    let (Some(user), Some(system)) = (tick(11), tick(12)) else {
        return Err("stat ticks unparsed".to_owned());
    };
    Ok([
        user + system,
        counter("rchar:")?,
        counter("wchar:")?,
        counter("read_bytes:")?,
        counter("write_bytes:")?,
    ])
}

/// One budget for every entry the runtime listing records, directories and
/// errors included.
const RUNTIME_ENTRIES: usize = 16;

/// What sits under the service root's private runtime directory: paths relative
/// to it, with sizes, descending two directory levels. Symlinks are listed and
/// never followed. Unreadable directories and entries are recorded, not skipped.
/// These are observations; which admission stages they fit is for the reader.
fn runtime_files(runtime: &Path) -> String {
    let mut seen = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        if seen.len() >= RUNTIME_ENTRIES {
            return format!("{seen:?} (truncated at {RUNTIME_ENTRIES} entries)");
        }
        // Slash-separated on every platform, so the record reads the same anywhere.
        let label = |path: &Path| {
            let parts: Vec<_> = path.iter().map(|part| part.to_string_lossy()).collect();
            format!("plugin-runtime/{}", parts.join("/"))
        };
        let entries = match fs::read_dir(runtime.join(&relative)) {
            Ok(entries) => entries,
            Err(error) => {
                seen.push(format!(
                    "{}:{}",
                    label(&relative),
                    proc_error(&error, "absent")
                ));
                continue;
            }
        };
        for entry in entries {
            if seen.len() >= RUNTIME_ENTRIES {
                return format!("{seen:?} (truncated at {RUNTIME_ENTRIES} entries)");
            }
            let path = match &entry {
                Ok(entry) => relative.join(entry.file_name()),
                Err(error) => {
                    let error = proc_error(error, "disappeared");
                    seen.push(format!("{}<entry>:{error}", label(&relative)));
                    continue;
                }
            };
            seen.push(match fs::symlink_metadata(runtime.join(&path)) {
                Ok(meta) if meta.file_type().is_symlink() => format!("{} symlink", label(&path)),
                Ok(meta) if meta.is_dir() && path.components().count() <= 2 => {
                    pending.push(path.clone());
                    format!("{}/", label(&path))
                }
                Ok(meta) if meta.is_dir() => format!("{}/ (not descended)", label(&path)),
                Ok(meta) => format!("{}={}B", label(&path), meta.len()),
                Err(error) => format!("{}:{}", label(&path), proc_error(&error, "disappeared")),
            });
        }
    }
    format!("{seen:?}")
}

/// One process's identity and thread census. A child's parent and start time are
/// read before and after its threads, so a reused or vanished pid is reported
/// rather than mistaken for the product's child.
fn process_snapshot(pid: u32, parent: Option<u32>, started: Instant) -> String {
    let identity = |stat: &str| -> Option<(String, u32, String)> {
        let (head, rest) = stat.rsplit_once(')')?;
        let comm = head.split_once('(')?.1.to_owned();
        let fields: Vec<&str> = rest.split_whitespace().collect();
        Some((
            comm,
            fields.get(1)?.parse().ok()?,
            (*fields.get(19)?).to_owned(),
        ))
    };
    let stat = format!("/proc/{pid}/stat");
    let before = match proc_read(stat.as_ref(), "missing").map(|text| identity(&text)) {
        Ok(Some(identity)) => identity,
        Ok(None) => return format!("pid={pid} stat unparsed"),
        Err(error) => return format!("pid={pid} {error}"),
    };
    let (comm, ppid, start) = &before;
    if let Some(parent) = parent {
        if *ppid != parent {
            return format!("pid={pid} comm={comm} ppid={ppid} is not the product's child");
        }
    }
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    let mut note = String::new();
    match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(tasks) => {
            for task in tasks.take(SNAPSHOT_THREADS) {
                if started.elapsed() > SNAPSHOT_BUDGET {
                    note = format!(" budget {}ms exhausted", SNAPSHOT_BUDGET.as_millis());
                    break;
                }
                let kind = match task {
                    Ok(task) => thread_kind(pid, &task.path()),
                    Err(error) => format!("unlisted:{error}"),
                };
                *kinds.entry(kind).or_default() += 1;
            }
        }
        Err(error) => note = format!(" threads unreadable:{error}"),
    }
    let after = proc_read(stat.as_ref(), "disappeared").map(|text| identity(&text));
    let same = matches!(&after, Ok(Some(identity)) if *identity == before);
    format!(
        "pid={pid} comm={comm} ppid={ppid} start={start} identity_stable={same} threads={kinds:?}{note}"
    )
}

/// `state wchan syscall[ fd->target]` for one thread.
fn thread_kind(pid: u32, task: &Path) -> String {
    let state = proc_read(&task.join("stat"), "disappeared")
        .map(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .unwrap_or("?")
                .to_owned()
        })
        .unwrap_or_else(|error| error);
    let wchan = proc_read(&task.join("wchan"), "disappeared").unwrap_or_else(|error| error);
    let call = match proc_read(&task.join("syscall"), "disappeared") {
        Ok(text) => syscall_kind(pid, &text),
        Err(error) => format!("syscall {error}"),
    };
    if call == "futex" {
        return format!("{state} {wchan} {call}");
    }
    // Name the few threads that are not parked, so a busy one can be told apart.
    let comm = proc_read(&task.join("comm"), "disappeared").unwrap_or_else(|error| error);
    format!("{state} {wchan} {call} comm={comm}")
}

/// Decodes `/proc/<pid>/task/<tid>/syscall`: "running", "-1 sp pc" when not in
/// a syscall, or the number and six hex arguments.
fn syscall_kind(pid: u32, text: &str) -> String {
    let mut fields = text.split_whitespace();
    let Some(number) = fields.next().and_then(|field| field.parse::<i64>().ok()) else {
        return format!("syscall:{}", text.trim());
    };
    if number < 0 {
        return "not-in-syscall".to_owned();
    }
    let Some((name, fd_argument)) = syscall_name(number) else {
        return format!("syscall#{number}");
    };
    if !fd_argument {
        return name.to_owned();
    }
    let fd = fields
        .next()
        .and_then(|arg| i64::from_str_radix(arg.trim_start_matches("0x"), 16).ok());
    match fd {
        Some(fd) if (0..=65_535).contains(&fd) => {
            let target = match fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
                Ok(target) => {
                    let target = target.to_string_lossy().into_owned();
                    target.chars().take(160).collect::<String>()
                }
                Err(error) => proc_error(&error, "disappeared"),
            };
            format!("{name} fd{fd}->{target}")
        }
        _ => format!("{name} fd-argument-unreadable"),
    }
}

/// x86_64 syscall names this diagnostic distinguishes, and whether the first
/// argument is a file descriptor. `futex` takes none; any
/// other number is reported as `syscall#N`.
fn syscall_name(number: i64) -> Option<(&'static str, bool)> {
    if !cfg!(target_arch = "x86_64") {
        return None;
    }
    Some(match number {
        0 => ("read", true),
        1 => ("write", true),
        16 => ("ioctl", true),
        17 => ("pread64", true),
        18 => ("pwrite64", true),
        19 => ("readv", true),
        20 => ("writev", true),
        43 => ("accept", true),
        44 => ("sendto", true),
        45 => ("recvfrom", true),
        46 => ("sendmsg", true),
        47 => ("recvmsg", true),
        72 => ("fcntl", true),
        73 => ("flock", true),
        74 => ("fsync", true),
        75 => ("fdatasync", true),
        77 => ("ftruncate", true),
        202 => ("futex", false),
        217 => ("getdents64", true),
        232 => ("epoll_wait", true),
        277 => ("sync_file_range", true),
        281 => ("epoll_pwait", true),
        285 => ("fallocate", true),
        288 => ("accept4", true),
        _ => return None,
    })
}

/// Reads at most `SNAPSHOT_READ_BYTES` of a `/proc` file. An absent file is
/// reported as `absent` (`missing` for a process never seen, `disappeared` for
/// one seen a moment earlier), and a refused read as `denied`.
fn proc_read(path: &Path, absent: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(SNAPSHOT_READ_BYTES).read_to_end(&mut bytes))
        .map(|_| String::from_utf8_lossy(&bytes).trim().to_owned())
        .map_err(|error| proc_error(&error, absent))
}

fn proc_error(error: &std::io::Error, absent: &str) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => absent.to_owned(),
        std::io::ErrorKind::PermissionDenied => "denied".to_owned(),
        _ => format!("error:{error}"),
    }
}

/// The failure snapshot's syscall decoding, on fixed `/proc/<pid>/task/<tid>/syscall`
/// lines: only an fd-bearing syscall resolves a descriptor, and futex never does.
#[cfg(target_arch = "x86_64")]
#[test]
fn failure_snapshot_decodes_syscalls_and_resolves_only_descriptor_arguments() {
    let pid = std::process::id();
    let futex = "202 0x7f00000000a8 0x80 0x0 0x0 0x0 0x0 0x7ffd00000000 0x7f0000000000";
    assert_eq!(syscall_kind(pid, futex), "futex");
    assert_eq!(
        syscall_kind(pid, "-1 0x7ffd00000000 0x7f0000000000"),
        "not-in-syscall"
    );
    assert_eq!(syscall_kind(pid, "running"), "syscall:running");
    assert_eq!(
        syscall_kind(pid, "999 0x0 0x0 0x0 0x0 0x0 0x0 0x0 0x0"),
        "syscall#999"
    );
    let fsync = syscall_kind(pid, "74 0x7 0x0 0x0 0x0 0x0 0x0 0x0 0x0");
    assert!(fsync.starts_with("fsync fd7->"), "{fsync}");
    let wild = syscall_kind(pid, "1 0xffffffffffffff9c 0x0 0x0 0x0 0x0 0x0 0x0 0x0");
    assert_eq!(wild, "write fd-argument-unreadable");
}

/// The runtime listing on a fixed tree: an absent root, relative paths and sizes,
/// a directory below two levels named but not descended, a symlink named but never
/// followed, and one global budget with explicit truncation.
#[test]
fn runtime_listing_is_bounded_relative_and_never_follows_symlinks() -> std::io::Result<()> {
    let base = std::env::temp_dir().join(format!("secode-plugin-evidence-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let runtime = base.join("plugin-runtime");
    assert!(runtime_files(&runtime).contains("plugin-runtime/:absent"));
    let pins = runtime.join("aaaa").join("bbbb");
    fs::create_dir_all(pins.join("deeper").join("hidden"))?;
    fs::write(pins.join("node.partial-7"), b"abc")?;
    let outside = base.join("outside");
    fs::create_dir_all(&outside)?;
    fs::write(outside.join("secret-outside"), b"x")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, pins.join("link"))?;
    let listed = runtime_files(&runtime);
    assert!(
        listed.contains("plugin-runtime/aaaa/bbbb/node.partial-7=3B"),
        "{listed}"
    );
    assert!(
        listed.contains("plugin-runtime/aaaa/bbbb/deeper/ (not descended)"),
        "{listed}"
    );
    assert!(
        !listed.contains("hidden") && !listed.contains("secret-outside"),
        "{listed}"
    );
    #[cfg(unix)]
    assert!(
        listed.contains("plugin-runtime/aaaa/bbbb/link symlink"),
        "{listed}"
    );
    for index in 0..RUNTIME_ENTRIES {
        fs::write(runtime.join(format!("filler-{index}")), b"")?;
    }
    let crowded = runtime_files(&runtime);
    assert!(
        crowded.ends_with(&format!("(truncated at {RUNTIME_ENTRIES} entries)")),
        "{crowded}"
    );
    fs::remove_dir_all(&base)
}

/// Host writeback evidence on fixed `/proc/meminfo` text: present fields, a
/// field absent from a complete read, and a field lost to truncation.
#[test]
fn host_writeback_distinguishes_present_absent_and_truncated_fields() {
    let meminfo = "MemTotal:       1 kB\nDirty:           2048 kB\nWriteback:        512 kB\n";
    assert_eq!(
        writeback_fields(meminfo, false),
        "dirty=2048 kB writeback=512 kB"
    );
    assert_eq!(
        writeback_fields("MemTotal: 1 kB\nDirty: 7 kB\n", false),
        "dirty=7 kB writeback=absent"
    );
    assert_eq!(
        writeback_fields("MemTotal: 1 kB\nDirty: 7 kB\n", true),
        "dirty=7 kB writeback=truncated"
    );
}
