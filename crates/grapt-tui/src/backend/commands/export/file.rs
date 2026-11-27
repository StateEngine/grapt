//! Publish only a complete staged export; hard_link never replaces a destination.
use super::{markdown, Format};
use grapt_client::client::SessionExport;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::Path,
};

const MAX_OUTPUT_BYTES: usize = 128 * 1024 * 1024;

struct Output {
    file: File,
    remaining: usize,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("export exceeds the 128 MiB output bound"));
        }
        let written = self.file.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub(super) fn publish(history: &SessionExport, format: Format, path: &Path) -> Result<(), String> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".grapt-export-tmp");
    let stage = std::path::PathBuf::from(name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&stage)
        .map_err(|error| format!("Cannot create staging file {}: {error}", stage.display()))?;
    // All failures below close the file before cleanup (also on Windows).
    let result = (|| -> io::Result<()> {
        let mut out = BufWriter::new(Output {
            file,
            remaining: MAX_OUTPUT_BYTES,
        });
        match format {
            Format::Json => {
                serde_json::to_writer_pretty(&mut out, history).map_err(io::Error::other)?
            }
            Format::Markdown => markdown::render(history, &mut out)?,
        }
        out.write_all(b"\n")?;
        out.flush()?;
        out.get_ref().file.sync_all()?;
        drop(out);
        // Same-directory staging: atomic, no-clobber publication on supported filesystems.
        // Never fall back to rename or copy, either of which could replace user data.
        fs::hard_link(&stage, path)
    })();
    let cleanup = fs::remove_file(&stage);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(format!("Cannot publish {}: {error}", path.display())),
        (result, Err(error)) => Err(format!(
            "{}; could not remove staging file {}: {error}",
            result.map_or_else(
                |e| format!("Export failed: {e}"),
                |()| format!("Complete export saved to {}", path.display())
            ),
            stage.display(),
        )),
    }
}
