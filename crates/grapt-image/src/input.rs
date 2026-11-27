//! Explicit client-side attachments use the same bounded pixel preparation.
use crate::{prepare, Detail, PreparedImage};
use base64::Engine as _;
use std::{fs::OpenOptions, io::Read, path::Path};

const SOURCE_BYTES: usize = 16 * 1024 * 1024;

pub fn read_path(path: &Path) -> Result<PreparedImage, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Inspect the opened handle without hanging on a substituted FIFO.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| format!("image open: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("image metadata: {e}"))?;
    if !metadata.is_file() || metadata.len() > SOURCE_BYTES as u64 {
        return Err("image must be a regular file no larger than 16 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take((SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("image read: {e}"))?;
    prepare(bytes, Detail::High)
}

impl PreparedImage {
    pub fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.media.mime(),
            base64::engine::general_purpose::STANDARD.encode(&self.bytes)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_paths_prepare_pixels_and_refuse_nonregular_inputs() -> Result<(), String> {
        let root = std::env::temp_dir().join(format!("grapt-image-path-{}", std::process::id()));
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
        let result = (|| {
            let image = crate::prepare_rgba(2, 2, vec![255; 16])?;
            let path = root.join("image.png");
            std::fs::write(&path, &image.bytes).map_err(|e| e.to_string())?;
            let loaded = read_path(&path)?;
            assert_eq!((loaded.width, loaded.height), (2, 2));
            assert!(loaded.data_url().starts_with("data:image/png;base64,"));
            assert!(read_path(&root).is_err());
            #[cfg(unix)]
            {
                let fifo = root.join("fifo");
                let status = std::process::Command::new("mkfifo")
                    .arg(&fifo)
                    .status()
                    .map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err("FIFO fixture unavailable".into());
                }
                assert!(read_path(&fifo).is_err());
            }
            Ok(())
        })();
        std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }
}
