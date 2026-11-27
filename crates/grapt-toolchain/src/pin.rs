//! The pin table: the committed artifact that vouches for a release.
//!
//! A pin is a COMMIT. An independently reviewed pin is a committed artifact
//! of its repository, and a pin that cannot be attributed to a commit is
//! refused as a workspace-state problem, never silently read:
//!
//! * not a Git worktree, or the attribution plumbing cannot be believed — `pin-source-not-git`;
//! * tracked nowhere at HEAD — `pin-source-untracked`;
//! * on-disk bytes differ from a clean checkout of the HEAD commit — `pin-source-dirty`.
//!
//! The table itself decodes strictly: one tool, at most 16 per-target
//! pins, every field through its typed value. A JSON parser silently
//! collapses duplicate object keys, so the decode walks the object with a
//! visitor that sees every key occurrence: a duplicate target is a
//! malformed pin (an attacker who could pick which duplicate survives could
//! pick which archive is verified), never a last-wins coin flip.

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};

use crate::refusal::{Refusal, RefusalReason};
use crate::value::{
    ArchiveFormat, EntryName, Sha256, SourceUrl, TargetTriple, ToolName, ToolVersion,
};

/// The pin table schema.
pub const PIN_SCHEMA: &str = "secode.toolchain.pins.v1";

/// The pin file bound: 64 KiB.
pub const PIN_MAX_BYTES: usize = 65536;

/// The pin table bound: 16 per-target entries.
pub const PIN_MAX_ENTRIES: usize = 16;

/// One per-target release pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub tool: ToolName,
    pub version: ToolVersion,
    pub target: TargetTriple,
    pub source: SourceUrl,
    pub archive_sha256: Sha256,
    pub format: ArchiveFormat,
    pub entry: EntryName,
}

/// One tool, at most 16 target pins, in a strict schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinTable {
    tool: ToolName,
    version: ToolVersion,
    pins: BTreeMap<TargetTriple, Pin>,
}

impl PinTable {
    pub fn tool(&self) -> &ToolName {
        &self.tool
    }

    pub fn version(&self) -> &ToolVersion {
        &self.version
    }

    /// The pin for a target; absent is `target-unsupported`, never a
    /// default.
    pub fn pin(&self, target: &TargetTriple) -> Option<&Pin> {
        self.pins.get(target)
    }

    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    /// Strict decode of the table from its committed bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, Refusal> {
        let malformed = |what: &'static str| Refusal::new(RefusalReason::PinMalformed, what);
        let top = RawDocument::parse(bytes)
            .map_err(|what| Refusal::new(RefusalReason::PinMalformed, what))?;
        let top = match top {
            Raw::Object(pairs) => pairs,
            _ => return Err(malformed("pin is not a JSON object")),
        };

        let mut keys: Vec<&str> = top.iter().map(|(key, _)| key.as_str()).collect();
        let key_count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        let expected = ["pins", "schema", "tool", "version"];
        if key_count != expected.len() || keys != expected {
            return Err(malformed(
                "pin carries a foreign, missing or duplicated key set",
            ));
        }

        let value_of = |name: &str| {
            top.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };

        let schema = match value_of("schema") {
            Some(Raw::String(value)) => value.as_str(),
            _ => return Err(malformed("pin schema is absent or not a string")),
        };
        if schema != PIN_SCHEMA {
            return Err(malformed("pin carries a foreign schema"));
        }
        let tool = ToolName::new(match value_of("tool") {
            Some(Raw::String(value)) => value.as_str(),
            _ => return Err(malformed("pin tool is absent or not a string")),
        })
        .ok_or_else(|| malformed("pin tool is out of bound"))?;
        let version = ToolVersion::new(match value_of("version") {
            Some(Raw::String(value)) => value.as_str(),
            _ => return Err(malformed("pin version is absent or not a string")),
        })
        .ok_or_else(|| malformed("pin version is out of bound"))?;

        let pins_value = value_of("pins").ok_or_else(|| malformed("pin entries are absent"))?;
        let pins_value = match pins_value {
            Raw::Object(entries) => entries,
            _ => return Err(malformed("pin entries are not an object")),
        };
        if pins_value.len() > PIN_MAX_ENTRIES {
            return Err(malformed("pin table exceeds the 16-entry bound"));
        }
        let mut keys: Vec<&str> = pins_value.iter().map(|(key, _)| key.as_str()).collect();
        let key_count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        if key_count != keys.len() {
            return Err(malformed("pin table carries a duplicate target key"));
        }

        let mut pins = BTreeMap::new();
        for (target_key, entry) in pins_value {
            let pin = decode_entry(&tool, &version, target_key, entry)?;
            pins.insert(pin.target.clone(), pin);
        }
        Ok(Self {
            tool,
            version,
            pins,
        })
    }
}

fn decode_entry(
    tool: &ToolName,
    version: &ToolVersion,
    target_key: &str,
    entry: &Raw,
) -> Result<Pin, Refusal> {
    let malformed = |what: &'static str| Refusal::new(RefusalReason::PinMalformed, what);
    let pairs = match entry {
        Raw::Object(pairs) => pairs,
        _ => return Err(malformed("pin entry is not an object")),
    };
    let mut keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    let key_count = keys.len();
    keys.sort_unstable();
    keys.dedup();
    let expected = ["archive_sha256", "entry", "format", "source"];
    if key_count != expected.len() || keys != expected {
        return Err(malformed(
            "pin entry carries a foreign, missing or duplicated key set",
        ));
    }
    let value_of = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    };
    let str_of = |name: &str, what: &'static str| -> Result<&str, Refusal> {
        match value_of(name) {
            Some(Raw::String(value)) => Ok(value.as_str()),
            _ => Err(malformed(what)),
        }
    };
    let source = SourceUrl::new(str_of(
        "source",
        "pin entry source is absent or not a string",
    )?)
    .ok_or_else(|| malformed("pin entry source is out of bound"))?;
    let archive_sha256 = Sha256::from_hex(str_of(
        "archive_sha256",
        "pin entry archive digest is absent or not a string",
    )?)
    .ok_or_else(|| malformed("pin entry archive digest is not 64 lowercase hex"))?;
    let format = ArchiveFormat::from_wire(str_of(
        "format",
        "pin entry format is absent or not a string",
    )?)
    .ok_or_else(|| {
        malformed("pin entry declares an archive format outside the closed vocabulary")
    })?;
    let entry = EntryName::new(str_of("entry", "pin entry name is absent or not a string")?)
        .ok_or_else(|| malformed("pin entry name is out of bound"))?;
    let target =
        TargetTriple::new(target_key).ok_or_else(|| malformed("pin target key is out of bound"))?;
    Ok(Pin {
        tool: tool.clone(),
        version: version.clone(),
        target,
        source,
        archive_sha256,
        format,
        entry,
    })
}

/// The commit attribution of a pin: an independently reviewed pin is a
/// COMMITTED artifact of its repository, and a pin that cannot be attributed
/// to a commit is refused as a workspace-state problem, never silently read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinAttribution {
    /// The full 40-hex sha of the HEAD commit carrying the pin.
    pub commit: String,
    /// The git object id of the pin's HEAD blob, as git reports it.
    pub blob: String,
}

/// Attribute the pin (a root-relative path) to a commit of the git worktree
/// rooted at `root`, and prove its disk bytes ARE the committed bytes. The
/// three workspace-state distinctions the contract keeps apart:
///
/// * `pin-source-not-git` — not a worktree, no git, or the attribution
///   plumbing cannot be believed;
/// * `pin-source-untracked` — in a worktree but not tracked at HEAD;
/// * `pin-source-dirty` — tracked, but the on-disk bytes differ from what a
///   clean checkout of the HEAD commit produces: an uncommitted re-cut is
///   not an admissible pin.
///
/// The dirty check compares the disk bytes to `git cat-file --filters
/// <commit>:./<path>` — the bytes a clean checkout of that commit writes to
/// disk, the worktree smudge filters (autocrlf and friends) applied — byte
/// for byte: no trimming, no normalization. A checkout whose disk bytes
/// differ from the raw blob because the worktree converts line endings
/// (the Git for Windows default) is the committed state, not a re-cut; on
/// a worktree without conversion the filtered bytes are the raw blob.
/// Every git call is fixed-argv with no shell and no interpolation: the
/// path crosses as one argument.
pub fn attribute(root: &Path, pin: &Path, disk_bytes: &[u8]) -> Result<PinAttribution, Refusal> {
    let rel = pin.to_str().ok_or_else(|| {
        Refusal::new(
            RefusalReason::PinUnreadable,
            "the pin path is not valid UTF-8",
        )
    })?;
    // Forward slashes only: git's tree machinery takes a /-separated path
    // relative to the worktree root, on every platform.
    let rel = rel.replace('\\', "/");
    let not_git = || {
        Refusal::new(
            RefusalReason::PinSourceNotGit,
            "the pin's directory is not a Git worktree, or the git attribution plumbing cannot be believed",
        )
    };
    let worktree = run_git(root, &["rev-parse", "--git-dir"]).ok_or_else(not_git)?;
    if worktree.trim().is_empty() {
        return Err(not_git());
    }
    // The worktree is git and git works: rev-parse --git-dir succeeded above.
    // From here, any way the pin is absent from HEAD — uncommitted, or no HEAD
    // commit at all — is the same admissibility fact: untracked.
    match run_git(root, &["ls-tree", "HEAD", "--", rel.as_str()]) {
        Some(out) if !out.trim().is_empty() => {}
        _ => {
            return Err(Refusal::new(
                RefusalReason::PinSourceUntracked,
                "the pin is not tracked at HEAD: a pin that is not a committed artifact is refused",
            ))
        }
    }
    let commit = run_git(root, &["rev-parse", "HEAD"]).ok_or_else(not_git)?;
    let commit = commit.trim().to_owned();
    // The bytes a clean checkout of the exact HEAD commit produces for the
    // pin path: the single comparison fact. `--filters` applies the
    // worktree smudge the checkout would apply (autocrlf and friends), so
    // a clean checkout on a CRLF-converting worktree — disk CRLF, blob LF
    // — is the committed state, not a re-cut; on a worktree without
    // conversion the filtered bytes equal the raw blob. A pin is
    // admissible only when its disk bytes ARE the checkout bytes.
    let checkout_bytes = run_git_bytes(
        root,
        &["cat-file", "--filters", &format!("{commit}:./{rel}")],
    )
    .ok_or_else(not_git)?;
    if checkout_bytes != disk_bytes {
        return Err(Refusal::new(
            RefusalReason::PinSourceDirty,
            "the pin's on-disk bytes differ from the HEAD blob: an uncommitted re-cut is not an admissible pin",
        ));
    }
    let blob = run_git(root, &["rev-parse", &format!("{commit}:./{rel}")]).ok_or_else(not_git)?;
    Ok(PinAttribution {
        commit,
        blob: blob.trim().to_owned(),
    })
}

fn git_command(root: &Path, args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("SSH_ASKPASS_REQUIRE", "never");
    command
}

/// Fixed-argv git plumbing: no shell, no interpolation. `None` covers both a
/// missing git executable and a non-zero exit; the caller maps that to the
/// workspace-state refusal that names it.
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let output = git_command(root, args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// The byte form of the same plumbing: raw stdout, no UTF-8 assumption. The
/// dirty check compares committed bytes to disk bytes, and bytes are not
/// text.
fn run_git_bytes(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = git_command(root, args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

/// A JSON document with every key occurrence retained, so duplicate keys —
/// which a map-typed parse would silently collapse — stay visible.
#[derive(Debug)]
enum Raw {
    Object(Vec<(String, Raw)>),
    String(String),
    Other,
}

impl<'de> Deserialize<'de> for Raw {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RawVisitor;
        impl<'de> Visitor<'de> for RawVisitor {
            type Value = Raw;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON document")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Raw, A::Error> {
                let mut out = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Raw>()? {
                    out.push((key, value));
                }
                Ok(Raw::Object(out))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Raw, E> {
                Ok(Raw::String(value.to_owned()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Raw, E> {
                Ok(Raw::String(value))
            }
            fn visit_bool<E: serde::de::Error>(self, _value: bool) -> Result<Raw, E> {
                Ok(Raw::Other)
            }
            fn visit_i64<E: serde::de::Error>(self, _value: i64) -> Result<Raw, E> {
                Ok(Raw::Other)
            }
            fn visit_u64<E: serde::de::Error>(self, _value: u64) -> Result<Raw, E> {
                Ok(Raw::Other)
            }
            fn visit_f64<E: serde::de::Error>(self, _value: f64) -> Result<Raw, E> {
                Ok(Raw::Other)
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Raw, E> {
                Ok(Raw::Other)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Raw, E> {
                Ok(Raw::Other)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Raw, A::Error> {
                // A top-level array is a malformed pin: drain it so the strict
                // shape check refuses it, and carry no payload forward.
                while seq.next_element::<Raw>()?.is_some() {}
                Ok(Raw::Other)
            }
        }
        deserializer.deserialize_any(RawVisitor)
    }
}

/// The whole-document wrapper: the pin is one JSON document, and the wrapper
/// exists so the parse can name it.
#[derive(Debug)]
struct RawDocument(Raw);

impl RawDocument {
    /// Strict decode of the whole document, every key occurrence retained.
    fn parse(bytes: &[u8]) -> Result<Raw, String> {
        let document = serde_json::from_slice::<RawDocument>(bytes)
            .map_err(|error| format!("pin is not a JSON document: {error}"))?;
        Ok(document.0)
    }
}

impl<'de> Deserialize<'de> for RawDocument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Raw::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ArchiveFormat, TargetTriple};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const DIGEST: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn must<T>(name: &str, value: Option<T>) -> T {
        match value {
            Some(value) => value,
            None => panic!("fixture {name} failed to parse"),
        }
    }

    fn entry(target: &str) -> String {
        format!(
            r#""{target}":{{"archive_sha256":"{DIGEST}","entry":"rg.exe","format":"tar","source":"archives/rg.tar"}}"#
        )
    }

    fn table(pins: &str) -> String {
        format!(
            r#"{{"schema":"secode.toolchain.pins.v1","tool":"ripgrep","version":"15.1.0","pins":{{{pins}}}}}"#
        )
    }

    fn refusal_of(bytes: &[u8]) -> Refusal {
        match PinTable::decode(bytes) {
            Ok(_) => panic!("this pin must be refused"),
            Err(refusal) => refusal,
        }
    }

    #[test]
    fn a_well_formed_table_decodes_every_field_and_exposes_its_pins() {
        let decoded = PinTable::decode(table(&entry("x86_64-pc-windows-msvc")).as_bytes())
            .unwrap_or_else(|refusal| panic!("a valid table must decode: {refusal}"));
        assert_eq!(decoded.tool().as_str(), "ripgrep");
        assert_eq!(decoded.version().as_str(), "15.1.0");
        assert_eq!(decoded.pin_count(), 1);
        let target = must("target", TargetTriple::new("x86_64-pc-windows-msvc"));
        let pin = decoded
            .pin(&target)
            .unwrap_or_else(|| panic!("the pinned target must be present"));
        assert_eq!(pin.tool.as_str(), "ripgrep");
        assert_eq!(pin.version.as_str(), "15.1.0");
        assert_eq!(pin.archive_sha256.hex(), DIGEST);
        assert_eq!(pin.format, ArchiveFormat::Tar);
        assert_eq!(pin.entry.as_str(), "rg.exe");
        assert_eq!(pin.source.as_str(), "archives/rg.tar");
        let other = must("other", TargetTriple::new("aarch64-unknown-linux-musl"));
        assert!(
            decoded.pin(&other).is_none(),
            "an absent target must be absent, never defaulted"
        );
    }

    #[test]
    fn a_foreign_schema_refuses_malformed() {
        let bytes =
            r#"{"schema":"grapt.toolchain.pins.v9","tool":"ripgrep","version":"1","pins":{}}"#;
        assert_eq!(
            refusal_of(bytes.as_bytes()).reason,
            RefusalReason::PinMalformed
        );
    }

    #[test]
    fn a_foreign_key_refuses_malformed() {
        let bytes = r#"{"schema":"secode.toolchain.pins.v1","tool":"ripgrep","version":"1","pins":{},"surprise":1}"#;
        assert_eq!(
            refusal_of(bytes.as_bytes()).reason,
            RefusalReason::PinMalformed
        );
    }

    #[test]
    fn a_missing_key_refuses_malformed() {
        let bytes = r#"{"schema":"secode.toolchain.pins.v1","tool":"ripgrep","pins":{}}"#;
        assert_eq!(
            refusal_of(bytes.as_bytes()).reason,
            RefusalReason::PinMalformed
        );
    }

    #[test]
    fn a_duplicate_target_key_refuses_malformed() {
        // A map-typed parser would collapse this to one entry; the strict
        // decode sees both occurrences, because which duplicate survives is
        // not a coin the boundary may leave to the parser.
        let bytes = table(&format!("{},{}", entry("t1"), entry("t1")));
        assert_eq!(
            refusal_of(bytes.as_bytes()).reason,
            RefusalReason::PinMalformed
        );
    }

    #[test]
    fn a_foreign_entry_key_refuses_malformed() {
        let pin = format!(
            r#""t1":{{"archive_sha256":"{DIGEST}","entry":"rg.exe","format":"tar","source":"archives/rg.tar","surprise":1}}"#
        );
        assert_eq!(
            refusal_of(table(&pin).as_bytes()).reason,
            RefusalReason::PinMalformed
        );
    }

    #[test]
    fn an_entry_field_out_of_bound_refuses_malformed() {
        let pin = r#""t1":{"archive_sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad","entry":"/absolute","format":"tar","source":"a/b.tar"}"#;
        assert_eq!(
            refusal_of(table(pin).as_bytes()).reason,
            RefusalReason::PinMalformed
        );
        let bad_digest =
            r#""t1":{"archive_sha256":"nothex","entry":"rg","format":"tar","source":"a/b.tar"}"#;
        assert_eq!(
            refusal_of(table(bad_digest).as_bytes()).reason,
            RefusalReason::PinMalformed
        );
        let bad_format = r#""t1":{"archive_sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad","entry":"rg","format":"zip","source":"a/b.tar"}"#;
        assert_eq!(
            refusal_of(table(bad_format).as_bytes()).reason,
            RefusalReason::PinMalformed
        );
    }

    #[test]
    fn a_seventeenth_entry_refuses_malformed() {
        let mut pins = Vec::new();
        for index in 0..17usize {
            pins.push(entry(&format!("t{index}")));
        }
        let bytes = table(&pins.join(","));
        assert_eq!(
            refusal_of(bytes.as_bytes()).reason,
            RefusalReason::PinMalformed
        );
    }

    #[test]
    fn a_non_object_document_refuses_malformed() {
        assert_eq!(refusal_of(b"[1,2,3]").reason, RefusalReason::PinMalformed);
        assert_eq!(
            refusal_of(b"\"just a string\"").reason,
            RefusalReason::PinMalformed
        );
        assert_eq!(refusal_of(b"").reason, RefusalReason::PinMalformed);
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "grapt-toolchain-{}-pin-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("test: create temp root: {error}"));
        root
    }

    fn git(cwd: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "toolchain-test")
            .env("GIT_AUTHOR_EMAIL", "toolchain-test@example.invalid")
            .env("GIT_COMMITTER_NAME", "toolchain-test")
            .env("GIT_COMMITTER_EMAIL", "toolchain-test@example.invalid")
            .output()
            .unwrap_or_else(|error| panic!("test: git is unavailable: {error}"));
        assert!(
            output.status.success(),
            "test: git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap_or_default()
            .trim()
            .to_owned()
    }

    #[test]
    fn a_directory_that_is_not_a_git_worktree_refuses_not_git() {
        let root = temp_root("notgit");
        std::fs::write(root.join("pins.json"), b"{}")
            .unwrap_or_else(|error| panic!("test: write pin: {error}"));
        // The temp root must not sit inside some other worktree, or the
        // evidence this test owes does not exist: fail loudly, never skip.
        let inside = std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|error| panic!("test: git is unavailable: {error}"));
        assert!(
            !inside.status.success(),
            "test: temp root is inside a git worktree; the not-git evidence cannot be produced"
        );
        let refusal = match attribute(root.as_path(), Path::new("pins.json"), b"{}") {
            Ok(_) => panic!("a non-git directory must refuse attribution"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.reason, RefusalReason::PinSourceNotGit);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_untracked_pin_refuses_untracked() {
        let root = temp_root("untracked");
        git(root.as_path(), &["init", "-q"]);
        std::fs::write(root.join("pins.json"), b"{}")
            .unwrap_or_else(|error| panic!("test: write pin: {error}"));
        let refusal = match attribute(root.as_path(), Path::new("pins.json"), b"{}") {
            Ok(_) => panic!("an untracked pin must refuse attribution"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.reason, RefusalReason::PinSourceUntracked);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pin_that_differs_from_its_head_blob_refuses_dirty() {
        let root = temp_root("dirty");
        git(root.as_path(), &["init", "-q"]);
        std::fs::write(root.join("pins.json"), b"original")
            .unwrap_or_else(|error| panic!("test: write pin: {error}"));
        git(root.as_path(), &["add", "pins.json"]);
        git(root.as_path(), &["commit", "-q", "-m", "commit the pin"]);
        let attributed = match attribute(root.as_path(), Path::new("pins.json"), b"original") {
            Ok(attribution) => attribution,
            Err(refusal) => panic!("a committed pin must attribute: {refusal}"),
        };
        assert_eq!(attributed.commit.len(), 40);
        assert_eq!(attributed.blob.len(), 40);
        std::fs::write(root.join("pins.json"), b"recut")
            .unwrap_or_else(|error| panic!("test: recut pin: {error}"));
        let refusal = match attribute(root.as_path(), Path::new("pins.json"), b"recut") {
            Ok(_) => panic!("a dirty pin must refuse attribution"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.reason, RefusalReason::PinSourceDirty);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_clean_checkout_under_autocrlf_attributes_and_a_recut_still_refuses() {
        // The N1 witness: a worktree whose smudge converts line
        // endings (the Git for Windows default) holds CRLF on disk for a
        // clean checkout of an LF blob. The disk bytes ARE the committed
        // state, so attribution must succeed; only a recut that a clean
        // checkout would not produce is refused.
        //
        // The setting is persisted in the test repository, NOT passed
        // per command: attribute() reads the repository's effective
        // config, so only a persisted setting is seen by both the
        // checkout and the attribution, on every host.
        let root = temp_root("autocrlf");
        git(root.as_path(), &["init", "-q"]);
        git(root.as_path(), &["config", "core.autocrlf", "true"]);
        let lf = b"{\n\"a\":1\n}\n";
        std::fs::write(root.join("pins.json"), lf)
            .unwrap_or_else(|error| panic!("test: write pin: {error}"));
        git(root.as_path(), &["add", "pins.json"]);
        git(root.as_path(), &["commit", "-q", "-m", "commit the pin"]);
        // Remove and re-check-out: the disk bytes are what the smudge
        // writes, NOT the raw blob.
        std::fs::remove_file(root.join("pins.json"))
            .unwrap_or_else(|error| panic!("test: remove pin: {error}"));
        git(root.as_path(), &["checkout", "--", "pins.json"]);
        let disk = std::fs::read(root.join("pins.json"))
            .unwrap_or_else(|error| panic!("test: read checked-out pin: {error}"));
        let attributed = match attribute(root.as_path(), Path::new("pins.json"), &disk) {
            Ok(attribution) => attribution,
            Err(refusal) => panic!("a clean autocrlf checkout must attribute: {refusal}"),
        };
        assert_eq!(attributed.commit.len(), 40);
        assert_eq!(attributed.blob.len(), 40);
        // The exact-byte dirty witness survives: a recut that a clean
        // checkout would not produce is still refused.
        std::fs::write(root.join("pins.json"), b"{\n\"a\":2\n}\n")
            .unwrap_or_else(|error| panic!("test: recut pin: {error}"));
        let recut = std::fs::read(root.join("pins.json"))
            .unwrap_or_else(|error| panic!("test: read recut pin: {error}"));
        let refusal = match attribute(root.as_path(), Path::new("pins.json"), &recut) {
            Ok(_) => panic!("a dirty pin must refuse attribution"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.reason, RefusalReason::PinSourceDirty);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pin_in_a_subdirectory_of_the_worktree_attributes_and_dirty_still_refuses() {
        let root = temp_root("subdir");
        git(root.as_path(), &["init", "-q"]);
        std::fs::create_dir_all(root.join("pins"))
            .unwrap_or_else(|error| panic!("test: create pins dir: {error}"));
        std::fs::write(root.join("pins").join("pins.json"), b"{\"sub\":true}")
            .unwrap_or_else(|error| panic!("test: write pin: {error}"));
        git(root.as_path(), &["add", "pins/pins.json"]);
        git(root.as_path(), &["commit", "-q", "-m", "commit the pin"]);
        // Multi-segment path from the worktree root:
        let attributed = match attribute(
            root.as_path(),
            Path::new("pins/pins.json"),
            b"{\"sub\":true}",
        ) {
            Ok(attribution) => attribution,
            Err(refusal) => panic!("a committed subdirectory pin must attribute: {refusal}"),
        };
        assert_eq!(attributed.commit.len(), 40);
        assert_eq!(attributed.blob.len(), 40);
        // The B3 shape: the activation root is itself a subdirectory of the
        // worktree, so the pin path must resolve from the root, not the top.
        let sub_root = root.join("pins");
        let attributed = match attribute(
            sub_root.as_path(),
            Path::new("pins.json"),
            b"{\"sub\":true}",
        ) {
            Ok(attribution) => attribution,
            Err(refusal) => panic!("a subdirectory-root pin must attribute: {refusal}"),
        };
        assert_eq!(attributed.blob.len(), 40);
        std::fs::write(root.join("pins").join("pins.json"), b"{\"recut\":true}")
            .unwrap_or_else(|error| panic!("test: recut pin: {error}"));
        let refusal = match attribute(
            sub_root.as_path(),
            Path::new("pins.json"),
            b"{\"recut\":true}",
        ) {
            Ok(_) => panic!("a dirty subdirectory pin must refuse attribution"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.reason, RefusalReason::PinSourceDirty);
        let _ = std::fs::remove_dir_all(&root);
    }
}
