use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub use grapt_toolchain::{
    activate, attribute, ActivationReceipt, ActivationRequest, ArchiveSource, FileSource,
    InstalledIdentity, Refusal, RefusalReason, Sha256, SourceUrl, TargetTriple, LAYOUT,
};
pub use grapt_types::{ClockAuthority, Timestamp};

pub const TARGET: &str = "x86_64-pc-windows-msvc";
pub const OTHER_TARGET: &str = "aarch64-unknown-linux-musl";
pub const PLATFORM: &str = "windows";
pub const ARCH: &str = "x86_64";
pub const PIN_REL: &str = "pins.json";
pub const SOURCE_REL: &str = "archives/rg.tar";
pub const ENTRY: &str = "rg.exe";
pub const FIXED_MILLIS: i64 = 1_700_000_000_000;
pub const BLOCK: usize = 512;

/// The deterministic clock every scenario injects: no ambient time.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(FIXED_MILLIS)
    }
}

/// A counting source: proves reuse does not refetch.
#[derive(Debug)]
pub struct CountingSource {
    pub inner: FileSource,
    pub calls: Arc<AtomicUsize>,
}

impl ArchiveSource for CountingSource {
    fn fetch(&self, source: &SourceUrl) -> Result<Vec<u8>, Refusal> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch(source)
    }
}

/// One octal ustar field: 7 digits + NUL.
fn write_octal(field: &mut [u8], value: u64) {
    let text = format!("{value:07o}");
    field[..text.len()].copy_from_slice(text.as_bytes());
    field[text.len()] = 0;
}

fn header_block(name: &str, size: u64, typeflag: u8) -> Vec<u8> {
    let mut block = [0u8; BLOCK];
    block[..name.len()].copy_from_slice(name.as_bytes());
    write_octal(&mut block[124..132], size);
    block[148] = typeflag;
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    let mut sum = 0u64;
    for (index, byte) in block.iter().enumerate() {
        sum += if (140..148).contains(&index) {
            0x20
        } else {
            u64::from(*byte)
        };
    }
    write_octal(&mut block[140..148], sum);
    block.to_vec()
}

fn data_blocks(bytes: &[u8]) -> Vec<u8> {
    let padded = bytes.len().div_ceil(BLOCK) * BLOCK;
    let mut out = bytes.to_vec();
    out.resize(padded, 0);
    out
}

pub fn file_entry(name: &str, bytes: &[u8]) -> Vec<u8> {
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let mut out = header_block(name, size, 0);
    out.extend(data_blocks(bytes));
    out
}

/// A raw entry with an explicit typeflag and size: the unsafe-entry
/// fixtures (symlinks) and the bad-checksum fixture.
pub fn raw_entry(name: &str, size: u64, typeflag: u8) -> Vec<u8> {
    header_block(name, size, typeflag)
}

/// Concatenate entry blocks and append the two-block terminator.
pub fn archive_stream(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(part);
    }
    out.extend_from_slice(&[0u8; BLOCK * 2]);
    out
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub fn temp_root(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "grapt-toolchain-act-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)
        .unwrap_or_else(|error| panic!("corpus: create temp root: {error}"));
    root
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "toolchain-test")
        .env("GIT_AUTHOR_EMAIL", "toolchain-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "toolchain-test")
        .env("GIT_COMMITTER_EMAIL", "toolchain-test@example.invalid")
        .output()
        .unwrap_or_else(|error| panic!("corpus: git is unavailable: {error}"));
    assert!(
        output.status.success(),
        "corpus: git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

pub fn must_target() -> TargetTriple {
    TargetTriple::new(TARGET).unwrap_or_else(|| panic!("corpus: target fixture failed to parse"))
}

/// One pin table: schema, tool, version, and the named per-target pins.
pub fn pin_text(version: &str, targets: &[(&str, &str, &str, &str)]) -> Vec<u8> {
    let entries: Vec<String> = targets
        .iter()
        .map(|(triple, digest, entry, source)| {
            format!(
                "\"{triple}\":{{\"archive_sha256\":\"{digest}\",\"entry\":\"{entry}\",\"format\":\"tar\",\"source\":\"{source}\"}}"
            )
        })
        .collect();
    format!(
        "{{\"schema\":\"secode.toolchain.pins.v1\",\"tool\":\"ripgrep\",\"version\":\"{version}\",\"pins\":{{{entries}}}}}",
        entries = entries.join(",")
    )
    .into_bytes()
}

/// One committed scene: root, git state, pin and release input.
pub struct Scene {
    pub root: PathBuf,
    pub clock: FixedClock,
}

impl Scene {
    pub fn fresh(label: &str, git_repo: bool) -> Self {
        let root = temp_root(label);
        if git_repo {
            git(root.as_path(), &["init", "-q"]);
        }
        Self {
            root,
            clock: FixedClock,
        }
    }

    pub fn write(&self, rel: &str, bytes: Vec<u8>) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("corpus: parent for {rel}: {error}"));
        }
        std::fs::write(&path, bytes).unwrap_or_else(|error| panic!("corpus: write {rel}: {error}"));
    }

    pub fn commit(&self, rels: &[&str]) -> String {
        let mut add_args: Vec<&str> = vec!["add", "--"];
        add_args.extend_from_slice(rels);
        git(self.root.as_path(), &add_args);
        git(
            self.root.as_path(),
            &["commit", "-q", "-m", "corpus: pin and release input"],
        );
        git(self.root.as_path(), &["rev-parse", "HEAD"])
    }

    pub fn run(&self, source: &dyn ArchiveSource) -> Result<ActivationReceipt, Refusal> {
        let request = ActivationRequest {
            root: self.root.clone(),
            pin: PathBuf::from(PIN_REL),
            target: must_target(),
            platform: PLATFORM.to_owned(),
            arch: ARCH.to_owned(),
            clock: &self.clock,
            source,
        };
        activate(&request)
    }

    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A valid committed pin naming the given targets plus the release input
/// the pin points at.
pub fn scene_with_input(
    label: &str,
    version: &str,
    targets: &[(&str, &str, &str, &str)],
    archive: Vec<u8>,
) -> Scene {
    let scene = Scene::fresh(label, true);
    scene.write(PIN_REL, pin_text(version, targets));
    scene.write(SOURCE_REL, archive);
    scene.commit(&[PIN_REL, SOURCE_REL]);
    scene
}

/// Commit one pin table change into a scene that already carries its tree.
pub fn commit_pin(scene: &Scene, version: &str, targets: &[(&str, &str, &str, &str)]) {
    scene.write(PIN_REL, pin_text(version, targets));
    scene.commit(&[PIN_REL]);
}

/// A fresh install at the named version: the workhorse of the positive
/// witnesses and of every refusal that needs a valid committed pin.
pub fn install(scene: &Scene, version: &str, binary: &[u8]) -> ActivationReceipt {
    let archive = archive_stream(&[file_entry(ENTRY, binary)]);
    let digest = Sha256::of(&archive).hex();
    scene.write(
        PIN_REL,
        pin_text(version, &[(TARGET, &digest, ENTRY, SOURCE_REL)]),
    );
    scene.write(SOURCE_REL, archive);
    scene.commit(&[PIN_REL, SOURCE_REL]);
    let source = FileSource::new(scene.root.clone());
    scene
        .run(&source)
        .unwrap_or_else(|refusal| panic!("corpus: {version} activation must succeed: {refusal}"))
}

pub fn refused(result: Result<ActivationReceipt, Refusal>) -> Refusal {
    match result {
        Ok(receipt) => panic!("corpus: expected a refusal, got {}", receipt.outcome),
        Err(refusal) => refusal,
    }
}

pub fn expect_reason(refusal: &Refusal, reason: RefusalReason, label: &str) {
    assert_eq!(
        refusal.reason, reason,
        "{label}: wrong refusal reason: {refusal}"
    );
}

/// The attempt must end clean after any refusal: lock released, staging
/// cleared. The refusal named what failed; nothing of the attempt may
/// stand.
pub fn assert_attempt_clean(scene: &Scene) {
    assert!(
        !LAYOUT.lock(&scene.root, "ripgrep").exists(),
        "corpus: the lock must be released after a refusal"
    );
    assert!(
        !LAYOUT.staging_dir(&scene.root, "ripgrep").exists(),
        "corpus: staging must be cleared after a refusal"
    );
}
