//! The TUI these tests drive is a sibling binary that the product's test
//! build does not rebuild, and a shared target directory may hold one built
//! from another tree. Cargo's dependency file beside it names every source
//! the build read; each must belong to this tree and be no newer than the
//! binary, or the run would test code other than what is checked out.
use super::*;

/// Fails loudly unless `binary` was built from this tree's current sources.
pub(super) fn require_fresh_build(binary: &Path) {
    let rebuild = "run `cargo build -p grapt-tui` from this tree with the same target directory";
    let depfile = binary.with_extension("d");
    let listing = fs::read_to_string(&depfile).unwrap_or_else(|error| {
        panic!(
            "TUI build record {} unreadable ({error}); {rebuild}",
            depfile.display()
        )
    });
    let built = modified(binary);
    let tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| panic!("the product crate sits two levels below its workspace"));
    let target = binary
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("the TUI binary sits inside a profile directory"));
    let (_, sources) = listing
        .split_once(": ")
        .unwrap_or_else(|| panic!("TUI build record {} has no sources", depfile.display()));
    let mut checked = 0_usize;
    for source in sources.replace("\\ ", "\u{0}").split_whitespace() {
        let source = PathBuf::from(source.replace('\u{0}', " "));
        if source.starts_with(target) {
            continue;
        }
        assert!(
            source.starts_with(tree),
            "the TUI binary {} was built from another tree ({}); {rebuild}",
            binary.display(),
            source.display()
        );
        assert!(
            modified(&source) <= built,
            "the TUI binary {} is older than {}; {rebuild}",
            binary.display(),
            source.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "TUI build record {} names no sources",
        depfile.display()
    );
}

fn modified(path: &Path) -> std::time::SystemTime {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or_else(|error| panic!("{} has no modification time: {error}", path.display()))
}
