//! Avoid Git for Windows' extra wrapper process for builtin read commands.
//! Repository contents are still discovered afresh by the same Git installation.
use std::{ffi::OsString, path::Path};

#[cfg(not(windows))]
pub(crate) async fn resolve(_directory: &Path) -> OsString {
    "git".into()
}

#[cfg(windows)]
pub(crate) async fn resolve(directory: &Path) -> OsString {
    static NATIVE: tokio::sync::OnceCell<Option<std::path::PathBuf>> =
        tokio::sync::OnceCell::const_new();
    let native = NATIVE
        .get_or_init(|| async {
            // Ask the selected Git for its own layout, rather than guessing the
            // installation or architecture from PATH. This probe pays once.
            let mut probe =
                super::command(directory, &["--exec-path"], std::ffi::OsStr::new("git"));
            probe.env_remove("GIT_EXEC_PATH");
            let (status, path) = super::run(probe).await.ok()?;
            (status == 0).then(|| native_path(path.trim())).flatten()
        })
        .await;
    native
        .as_ref()
        .filter(|path| path.is_file())
        .map_or_else(|| "git".into(), |path| path.as_os_str().to_owned())
}

#[cfg(any(windows, test))]
fn native_path(exec_path: &str) -> Option<std::path::PathBuf> {
    if exec_path.len() > super::MAX_PATH_BYTES {
        return None;
    }
    let path = Path::new(exec_path);
    if !path.is_absolute()
        || path.file_name()? != "git-core"
        || path.parent()?.file_name()? != "libexec"
    {
        return None;
    }
    let architecture = path.parent()?.parent()?;
    if !["mingw64", "mingw32", "ucrt64", "clangarm64"]
        .iter()
        .any(|name| {
            architecture
                .file_name()
                .is_some_and(|actual| actual == *name)
        })
    {
        return None;
    }
    let native = architecture.join("bin/git.exe");
    native.is_file().then_some(native)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_installations_use_their_own_binary_and_unknown_layouts_fall_back(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("grapt-native-git-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        for architecture in ["mingw64", "mingw32", "ucrt64", "clangarm64"] {
            let directory = root.join(architecture);
            std::fs::create_dir_all(directory.join("libexec/git-core"))?;
            std::fs::create_dir(directory.join("bin"))?;
            let native = directory.join("bin/git.exe");
            std::fs::write(&native, b"owned layout fixture")?;
            let exec = directory.join("libexec/git-core");
            let text = exec.to_str().ok_or("fixture path is not UTF-8")?;
            assert_eq!(native_path(text), Some(native.clone()));
            std::fs::remove_file(&native)?;
            assert_eq!(
                native_path(text),
                None,
                "missing native binary must retain ordinary dispatch"
            );
        }
        let unknown = root.join("other");
        std::fs::create_dir_all(unknown.join("libexec/git-core"))?;
        std::fs::create_dir(unknown.join("bin"))?;
        std::fs::write(unknown.join("bin/git.exe"), b"unrecognized fixture")?;
        assert_eq!(
            native_path(
                unknown
                    .join("libexec/git-core")
                    .to_str()
                    .ok_or("fixture path")?
            ),
            None,
            "an unrelated installation layout must retain ordinary dispatch"
        );
        assert_eq!(native_path("relative/mingw64/libexec/git-core"), None);
        assert_eq!(native_path(root.to_str().ok_or("fixture path")?), None);
        assert_eq!(
            native_path(&"x".repeat(super::super::MAX_PATH_BYTES + 1)),
            None
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
