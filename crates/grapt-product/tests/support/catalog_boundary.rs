//! Bounded marker context for failed catalog fixtures; never read file contents.
use std::path::Path;

pub fn diagnostics(root: &Path) -> String {
    let resolved = std::fs::canonicalize(root);
    let path = resolved.as_deref().unwrap_or(root);
    let mut lines = vec![format!(
        "catalog fixture ancestor scan: root={} canonical={:?}",
        root.to_string_lossy()
            .chars()
            .take(1024)
            .collect::<String>(),
        resolved.as_ref().err().map(std::io::Error::kind)
    )];
    for (depth, ancestor) in path.ancestors().take(64).enumerate() {
        let marker = |name| match std::fs::symlink_metadata(ancestor.join(name)) {
            Ok(info) if info.file_type().is_symlink() => "symlink".to_owned(),
            Ok(info) if info.is_dir() => "directory".to_owned(),
            Ok(info) if info.is_file() => "file".to_owned(),
            Ok(_) => "other".to_owned(),
            Err(error) => format!("{:?}", error.kind()),
        };
        let suffix = ancestor
            .to_string_lossy()
            .chars()
            .rev()
            .take(128)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        lines.push(format!(
            "ancestor[{depth}] …{suffix:?}: .git={} .grapt={}",
            marker(".git"),
            marker(".grapt")
        ));
    }
    lines.join("\n")
}
