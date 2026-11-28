//! Explain an absent protected path without changing its deny policy.
use super::*;

impl Plan {
    pub(in crate::dogfood::tools) fn notes(&self, stderr: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(stderr);
        let mut notes = Vec::new();
        if text.contains("NtCreateDirectoryObject") && text.contains("0xC0000022") {
            notes.push(if self.git_metadata_read_only {
                "MSYS named-object creation denied by the Windows sandbox. The read-only Git policy requires containment; use a contained native command or select a profile that permits Git writes. Review partial effects before rerunning.".into()
            } else {
                "MSYS named-object creation denied by the Windows sandbox. Request msys_unconfined=true to choose Once (this command tree) or Session (later Windows shell command trees); review partial effects before rerunning. No automatic retry occurred.".into()
            });
        }
        if !stderr
            .windows(b"Permission denied".len())
            .any(|part| part == b"Permission denied")
        {
            return notes;
        }
        let stderr = normalize(&String::from_utf8_lossy(stderr));
        for path in &self.protected {
            if !matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
            {
                continue;
            }
            let normalized = normalize(&path.to_string_lossy());
            let warnings = [
                format!("{normalized}: permission denied"),
                format!("{normalized}': permission denied"),
                format!("{normalized}\": permission denied"),
            ];
            if !stderr
                .lines()
                .any(|line| warnings.iter().any(|warning| line.contains(warning)))
            {
                continue;
            }
            // The leaf explains the warning without repeating a long local path.
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            let name: String = name.chars().filter(|c| !c.is_control()).take(128).collect();
            notes.push(format!(
                "sandbox: protected path {name} (absent, creation blocked)"
            ));
            if notes.len() == 8 {
                break;
            }
        }
        notes
    }
}

fn normalize(text: &str) -> String {
    text.replace('\\', "/")
        .replace("//?/", "")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_missing_metadata_is_explained_without_creating_it(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("grapt-deny-note-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let path = root.join("config.worktree");
        let plan = Plan {
            git_metadata_read_only: false,
            readable: vec![root.clone()],
            writable: vec![root.clone()],
            protected: vec![path.clone()],
            denied: Vec::new(),
            network: true,
        };
        let warning = format!(
            "warning: unable to access '{}': Permission denied",
            path.display()
        );
        assert_eq!(
            plan.notes(warning.as_bytes()),
            ["sandbox: protected path config.worktree (absent, creation blocked)"]
        );
        assert!(!path.exists());
        assert!(plan.notes(b"unrelated: Permission denied").is_empty());
        let namespace = plan.notes(b"sh: NtCreateDirectoryObject(msys-2.0): 0xC0000022");
        assert!(namespace[0].contains("MSYS named-object creation denied by the Windows sandbox"));
        assert!(namespace[0].contains("msys_unconfined=true"));
        std::fs::write(&path, b"[core]\n")?;
        assert!(plan.notes(warning.as_bytes()).is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
