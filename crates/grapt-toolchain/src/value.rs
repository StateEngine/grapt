//! The typed values the activation contract admits.
//!
//! Every constructor returns `Option`: a value that cannot be named
//! precisely is not a value this boundary can reason about. The shapes are
//! the admitted contract (`fixtures/toolchain-activation/contract.json`,
//! revision 3); anything outside them is refused at decode, before any
//! bytes are trusted.

/// A tool name: 1..=64 bytes of `[a-z0-9._-]`, leading byte `[a-z0-9]`,
/// and never a Windows reserved device name.
///
/// The name is the first layout segment of the activation tree: the
/// leading-byte rule and the reserved-device rejection keep it safe as
/// one, because a segment a host filesystem treats as a device is not a
/// name. The identity record and the pin must agree on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolName(String);

impl ToolName {
    pub fn new(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        if bytes.is_empty() || bytes.len() > 64 {
            return None;
        }
        if !matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9') {
            return None;
        }
        if !bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
        {
            return None;
        }
        if is_windows_reserved_name(raw) {
            return None;
        }
        Some(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An upstream release version: 1..=32 printable ASCII, no whitespace.
///
/// An equal version with a changed archive digest or source is a RE-CUT, not
/// an upgrade, and refuses reuse (the identity match law).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolVersion(String);

impl ToolVersion {
    pub fn new(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        if bytes.is_empty() || bytes.len() > 32 {
            return None;
        }
        if !bytes.iter().all(|b| (0x21..=0x7e).contains(b)) {
            return None;
        }
        Some(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A per-target pin key: 1..=128 of `[a-z0-9._-]`.
///
/// The target is the pin key, not an environment read: an absent entry is
/// `target-unsupported`, never a default.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TargetTriple(String);

impl TargetTriple {
    pub fn new(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        if bytes.is_empty() || bytes.len() > 128 {
            return None;
        }
        if !bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
        {
            return None;
        }
        Some(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The exact source the archive was verified against: 1..=1024 bytes, no
/// control bytes.
///
/// The typed value names a source; it does not decide what a source MAY
/// be. The shipped deterministic adapter ([`crate::source::FileSource`])
/// admits scheme-less filesystem paths only; any scheme prefix is an
/// explicit release-input boundary refused `source-unsupported`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceUrl(String);

impl SourceUrl {
    pub fn new(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        if bytes.is_empty() || bytes.len() > 1024 {
            return None;
        }
        if bytes.iter().any(|b| *b < 0x20 || *b == 0x7f) {
            return None;
        }
        Some(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A SHA-256 digest: exactly 32 bytes, spelled as 64 lowercase hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    /// Hash `data`. The only way to mint a digest from bytes.
    pub fn of(data: &[u8]) -> Self {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(data);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    /// Strict decode: exactly 64 lowercase hex characters.
    pub fn from_hex(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        if bytes
            .iter()
            .any(|b| !matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks(2).enumerate() {
            let value = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
            out[i] = value;
        }
        Some(Self(out))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Canonical spelling: 64 lowercase hex characters.
    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

/// The archive format vocabulary: closed, and `tar` only.
///
/// The shipped bounded decoder admits uncompressed ustar tar and nothing
/// else; a pin declaring any other format is a MALFORMED pin (the
/// vocabulary is a closed typed value, so the foreign string is refused at
/// pin decode), never a format to discover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    Tar,
}

impl ArchiveFormat {
    /// Strict decode of the closed vocabulary.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "tar" => Some(Self::Tar),
            _ => None,
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::Tar => "tar",
        }
    }
}

/// The pinned location of the binary inside the archive: 1..=256 bytes,
/// relative, `[a-z0-9._-A-Z]` segments, no dot segments, no absolute path,
/// at most four segments deep (the decoder admits no deeper tree, so a pin
/// naming one is out of bound). The final segment is where the executable
/// lands on disk, and a device name there is not a file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryName(String);

impl EntryName {
    pub fn new(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        if bytes.is_empty() || bytes.len() > 256 {
            return None;
        }
        if raw.starts_with('/') {
            return None;
        }
        let mut segments = 0usize;
        for segment in raw.split('/') {
            segments += 1;
            if segments > 4 || segment.is_empty() || segment == "." || segment == ".." {
                return None;
            }
            if !segment
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
            {
                return None;
            }
        }
        let file_name = raw.rsplit('/').next().unwrap_or(raw);
        if is_windows_reserved_name(file_name) {
            return None;
        }
        Some(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The executable file name: the final segment of the entry path.
    /// The canonical layout names the executable after it.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

/// The Windows reserved device names, case-insensitive: `con`, `prn`,
/// `aux`, `nul`, and `com1`..`com9` / `lpt1`..`lpt9`. A layout
/// segment that is one of these names is a device, not a file, on the host
/// this boundary must be able to run on.
fn is_windows_reserved_name(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    matches!(lowered.as_str(), "con" | "prn" | "aux" | "nul")
        || (lowered.starts_with("com")
            && lowered.len() == 4
            && matches!(lowered.as_bytes()[3], b'1'..=b'9'))
        || (lowered.starts_with("lpt")
            && lowered.len() == 4
            && matches!(lowered.as_bytes()[3], b'1'..=b'9'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_admits_the_contraction_charset_and_rejects_the_rest() {
        assert_eq!(
            ToolName::new("ripgrep").map(|t| t.as_str().to_owned()),
            Some("ripgrep".to_owned())
        );
        assert!(ToolName::new("a.b-c_d9").is_some());
        assert!(ToolName::new("").is_none());
        assert!(ToolName::new(&"a".repeat(65)).is_none());
        assert!(ToolName::new("Ripgrep").is_none());
        assert!(ToolName::new("ripg rep").is_none());
        assert!(ToolName::new("ripg/rep").is_none());
        assert!(ToolName::new("ripg\nrep").is_none());
        assert!(ToolName::new("ripgép").is_none());
        assert!(ToolName::new(".").is_none());
        assert!(ToolName::new("..").is_none());
        assert!(ToolName::new(".hidden").is_none());
        assert!(ToolName::new("con").is_none());
        assert!(ToolName::new("prn").is_none());
        assert!(ToolName::new("aux").is_none());
        assert!(ToolName::new("nul").is_none());
        assert!(ToolName::new("com7").is_none());
        assert!(ToolName::new("lpt9").is_none());
        assert!(ToolName::new("NUL").is_none());
        assert!(ToolName::new("Com7").is_none());
    }

    #[test]
    fn tool_version_admits_printable_ascii_and_rejects_the_rest() {
        assert!(ToolVersion::new("15.1.0").is_some());
        assert!(ToolVersion::new("15.1.0+meta").is_some());
        assert!(ToolVersion::new("").is_none());
        assert!(ToolVersion::new(&"v".repeat(33)).is_none());
        assert!(ToolVersion::new("15.1 0").is_none());
        assert!(ToolVersion::new("15.1\x000").is_none());
    }

    #[test]
    fn target_triple_admits_release_triples_and_rejects_the_rest() {
        assert!(TargetTriple::new("x86_64-pc-windows-msvc").is_some());
        assert!(TargetTriple::new("aarch64-unknown-linux-musl").is_some());
        assert!(TargetTriple::new("").is_none());
        assert!(TargetTriple::new(&"a".repeat(129)).is_none());
        assert!(TargetTriple::new("X86_64-pc-windows-msvc").is_none());
        assert!(TargetTriple::new("x86_64/pc").is_none());
    }

    #[test]
    fn source_url_admits_bounded_paths_and_rejects_control_bytes() {
        assert!(SourceUrl::new("archives/rg-15.1.0.tar").is_some());
        assert!(SourceUrl::new(&"a".repeat(1024)).is_some());
        assert!(SourceUrl::new("").is_none());
        assert!(SourceUrl::new(&"a".repeat(1025)).is_none());
        assert!(SourceUrl::new("archives/\x01rg.tar").is_none());
    }

    #[test]
    fn sha256_hashes_and_decodes_strictly() {
        // sha256("abc") — the FIPS 180-2 test vector.
        let known = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(Sha256::of(b"abc").hex(), known);
        assert!(Sha256::from_hex(known).is_some());
        assert_eq!(
            Sha256::from_hex(known).map(|d| d == Sha256::of(b"abc")),
            Some(true)
        );
        assert!(Sha256::from_hex(&known.to_uppercase()).is_none());
        assert!(Sha256::from_hex(&known[..63]).is_none());
        assert!(Sha256::from_hex(&format!("{known}g")).is_none());
        assert!(Sha256::from_hex(&"g".repeat(64)).is_none());
    }

    #[test]
    fn archive_format_is_closed_and_admits_tar_only() {
        assert_eq!(ArchiveFormat::from_wire("tar"), Some(ArchiveFormat::Tar));
        assert_eq!(ArchiveFormat::from_wire("zip"), None);
        assert_eq!(ArchiveFormat::from_wire("tar.gz"), None);
        assert_eq!(ArchiveFormat::from_wire("TAR"), None);
        assert_eq!(ArchiveFormat::Tar.wire(), "tar");
    }

    #[test]
    fn entry_name_admits_relative_bounded_paths_and_rejects_the_rest() {
        assert!(EntryName::new("rg.exe").is_some());
        assert!(EntryName::new("a/b/c/rg").is_some());
        assert_eq!(
            EntryName::new("rg.exe").map(|e| e.file_name().to_owned()),
            Some("rg.exe".to_owned())
        );
        assert_eq!(
            EntryName::new("a/b/rg.exe").map(|e| e.file_name().to_owned()),
            Some("rg.exe".to_owned())
        );
        assert!(EntryName::new("").is_none());
        assert!(EntryName::new(&"a".repeat(256)).is_some());
        assert!(EntryName::new(&"a".repeat(257)).is_none());
        assert!(EntryName::new("/absolute").is_none());
        assert!(EntryName::new("./relative").is_none());
        assert!(EntryName::new("a/../b").is_none());
        assert!(EntryName::new("a//b").is_none());
        assert!(EntryName::new("a b/c").is_none());
        assert!(EntryName::new("a/b/c/d/e").is_none());
        assert!(EntryName::new("CON").is_none());
        assert!(EntryName::new("a/con").is_none());
        assert!(EntryName::new("con.exe").is_some());
        assert!(EntryName::new("a/con.exe").is_some());
    }
}
