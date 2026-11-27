//! Prepare the stable launcher before activation, under the staging writer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::{inventory::Inventory, io, package, snapshot::Stamp, stage, transition};

const BYTES: u64 = 100 * 1024 * 1024;
const RECORD_BYTES: u64 = 16 * 1024;
const SCHEMA: &str = "secode.install.bootstrap.v1";
const PRE_RENAME: &str = "a pre-rename build cannot provide the fixed launcher; install or update from a current package";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: String,
    source_commit: String,
    sha256: String,
    bytes: u64,
}

pub(in super::super) struct Prepared {
    path: PathBuf,
    stamp: Stamp,
    /// The launcher's own build, when it differs from the package's.
    differs: Option<String>,
}

impl Prepared {
    pub(in super::super) fn recheck(&self) -> Result<(), String> {
        self.stamp.recheck(&self.path)
    }
}

fn verify(path: &Path, receipt: &Receipt) -> Result<Stamp, String> {
    let (sha, bytes, _executable, stamp) = package::fingerprint(path, BYTES)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = stamp
            .file
            .metadata()
            .map_err(|error| error.to_string())?
            .permissions()
            .mode();
        if !_executable || mode & 0o022 != 0 {
            return Err(
                "fixed bundled launcher is not executable or is writable by other users".into(),
            );
        }
    }
    if receipt.schema != SCHEMA
        || !package::commit(&receipt.source_commit)
        || receipt.bytes > BYTES
        || receipt.bytes != bytes
        || receipt.sha256 != sha
    {
        return Err("fixed installation launcher differs from its receipt".into());
    }
    Ok(stamp)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn prepare_named(
    prefix: &Path,
    package: &package::Package,
    name: &str,
    record: &str,
) -> Result<Prepared, String> {
    let bin = prefix.join("bin");
    stage::directory(&bin)?;
    let path = bin.join(name);
    let temporary = bin.join(format!("{name}.new"));
    let receipt_path = prefix.join(record);
    let receipt = match std::fs::symlink_metadata(&receipt_path) {
        Ok(_) => Some(
            serde_json::from_slice::<Receipt>(&io::read(&receipt_path, RECORD_BYTES)?)
                .map_err(|_| "installation bootstrap receipt is malformed")?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "installation bootstrap is unreadable: {:?}",
                error.kind()
            ))
        }
    };
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let receipt = receipt.ok_or("fixed installation launcher has no receipt")?;
            let input = launcher_input(package, name)?;
            let differs = input.is_some_and(|input| {
                receipt.sha256 != input.sha256 || receipt.bytes != input.bytes
            });
            let stamp = match (verify(&path, &receipt), input) {
                (Ok(stamp), _) => stamp,
                #[cfg(target_os = "linux")]
                (Err(reason), Some(input))
                    if receipt.schema == SCHEMA
                        && package::commit(&receipt.source_commit)
                        && receipt.bytes <= BYTES
                        && receipt.sha256.len() == 64
                        && receipt
                            .sha256
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
                {
                    // Linux automatically refreshes this owned se launcher. A
                    // crash after its rename can leave the previous receipt.
                    // Repair only when the file is exactly the requested,
                    // verified CLI; an unknown replacement is never overwritten.
                    let replacement = Receipt {
                        schema: SCHEMA.into(),
                        source_commit: package.commit.clone(),
                        sha256: input.sha256.clone(),
                        bytes: input.bytes,
                    };
                    let stamp = verify(&path, &replacement).map_err(|_| reason)?;
                    io::replace(
                        &receipt_path,
                        &serde_json::to_vec(&replacement)
                            .map_err(|_| "launcher recovery receipt cannot be encoded")?,
                    )?;
                    stamp.recheck(&path)?;
                    return Ok(Prepared {
                        path,
                        stamp,
                        differs: None,
                    });
                }
                (Err(reason), _) => return Err(reason),
            };
            return Ok(Prepared {
                stamp,
                path,
                differs: differs.then_some(receipt.source_commit),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "fixed installation launcher is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    if name == "grapt.exe" && transition::pointer(prefix)?.is_some() {
        return Err("active installation is missing its fixed launcher; activation refused".into());
    }
    let receipt = match receipt {
        Some(receipt) => receipt,
        None => {
            // No launcher receipt exists. Discard only this owned partial
            // and copy from the already verified inactive package.
            match std::fs::symlink_metadata(&temporary) {
                Ok(_) => {
                    drop(io::regular(&temporary)?);
                    std::fs::remove_file(&temporary).map_err(|error| {
                        format!("launcher partial cannot be removed: {:?}", error.kind())
                    })?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "launcher partial is unreadable: {:?}",
                        error.kind()
                    ))
                }
            }
            package.recheck(&package.root)?;
            let input = launcher_input(package, name)?.ok_or(PRE_RENAME)?;
            stage::copy(&package.root.join(&input.relative), &temporary, input)?;
            io::sync_directory(&bin)?;
            let receipt = Receipt {
                schema: SCHEMA.to_owned(),
                source_commit: package.commit.clone(),
                sha256: input.sha256.clone(),
                bytes: input.bytes,
            };
            verify(&temporary, &receipt)?;
            io::replace(
                &receipt_path,
                &serde_json::to_vec(&receipt)
                    .map_err(|_| "installation bootstrap receipt cannot be encoded")?,
            )?;
            receipt
        }
    };
    // Receipt-before-file ordering makes a killed first install recover only
    // its previously verified partial. A missing/corrupt partial refuses.
    let stamp = verify(&temporary, &receipt)?;
    let differs = launcher_input(package, name)?
        .is_some_and(|input| receipt.sha256 != input.sha256 || receipt.bytes != input.bytes)
        .then(|| receipt.source_commit.clone());
    std::fs::rename(&temporary, &path).map_err(|error| {
        format!(
            "fixed installation launcher cannot be published: {:?}",
            error.kind()
        )
    })?;
    io::sync_directory(&bin)?;
    #[cfg(target_os = "linux")]
    let stamp = stamp.after_rename(&path)?;
    stamp.recheck(&path)?;
    Ok(Prepared {
        path,
        stamp,
        differs,
    })
}

/// The verified input the launcher `name` copies. The bundled `se` launcher
/// is the package's own CLI under either inventory, as before the rename.
/// The fixed CLI launcher is recognised by its current name, so only a
/// current package can provide it; a pre-rename package (a retained revert
/// target) keeps the existing launcher and yields none.
fn launcher_input<'a>(
    package: &'a package::Package,
    name: &str,
) -> Result<Option<&'a package::Input>, String> {
    if name == Inventory::Current.cli() && package.inventory != Inventory::Current {
        return Ok(None);
    }
    let cli = package.inventory.cli();
    package
        .inputs
        .iter()
        .find(|input| input.relative == Path::new(cli))
        .map(Some)
        .ok_or_else(|| "verified package has no CLI launcher".into())
}

pub(in super::super) enum Update {
    Retain(Box<Prepared>),
    Refresh(Box<Refresh>),
}

pub(in super::super) struct Refresh {
    path: PathBuf,
    temporary: PathBuf,
    stamp: Stamp,
    previous: Option<Stamp>,
    receipt: Receipt,
    receipt_path: PathBuf,
}

impl Update {
    /// Publish or recheck the launcher. The result names the launcher's own
    /// build when it was retained and differs from the activated one.
    pub(in super::super) fn finish(&self) -> Result<Option<String>, String> {
        match self {
            Self::Retain(prepared) => {
                prepared.recheck()?;
                Ok(prepared.differs.clone())
            }
            Self::Refresh(prepared) => {
                prepared.publish(|| Ok(()))?;
                Ok(None)
            }
        }
    }

    #[cfg(test)]
    pub(in super::super) fn interrupt_after_launcher(&self) -> Result<(), String> {
        match self {
            Self::Refresh(prepared) => {
                prepared.publish(|| Err("named launcher publication interruption".into()))
            }
            Self::Retain(_) => Err("fixture requires explicit launcher refresh".into()),
        }
    }
}

impl Refresh {
    // Only the installation command, holding prefix EX, calls this. The
    // closure is the same test-only boundary technique as transition.rs.
    fn publish(&self, after_file: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        self.stamp.recheck(&self.temporary)?;
        match &self.previous {
            Some(previous) => previous.recheck(&self.path)?,
            None => match std::fs::symlink_metadata(&self.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err("fixed launcher appeared during preparation; retry".into()),
            },
        }
        std::fs::rename(&self.temporary, &self.path).map_err(|error| {
            let remedy = if cfg!(windows) {
                "quit installed Grapt apps and run the extracted package's grapt.exe with --refresh-launcher"
            } else {
                "retry installation from the extracted package"
            };
            format!("fixed launcher cannot be refreshed: {:?}; {remedy}", error.kind())
        })?;
        io::sync_directory(
            self.path
                .parent()
                .ok_or("fixed launcher has no directory")?,
        )?;
        #[cfg(target_os = "linux")]
        let stamp = self.stamp.after_rename(&self.path)?;
        #[cfg(windows)]
        let stamp = &self.stamp;
        after_file()?;
        io::replace(
            &self.receipt_path,
            &serde_json::to_vec(&self.receipt)
                .map_err(|_| "installation bootstrap receipt cannot be encoded")?,
        )?;
        stamp.recheck(&self.path)
    }
}

pub(in super::super) fn prepare_update_named(
    prefix: &Path,
    package: &package::Package,
    refresh: bool,
    name: &str,
    record: &str,
) -> Result<Update, String> {
    if !refresh {
        return prepare_named(prefix, package, name, record)
            .map(Box::new)
            .map(Update::Retain);
    }
    package.recheck(&package.root)?;
    let bin = prefix.join("bin");
    stage::directory(&bin)?;
    let path = bin.join(name);
    let temporary = bin.join(format!("{name}.new"));
    let previous = match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            #[cfg(windows)]
            {
                let file = io::snapshot_regular(&path)?;
                Some(Stamp::capture(&path, &file)?)
            }
            #[cfg(target_os = "linux")]
            {
                Some(Stamp::capture(io::regular(&path)?)?)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("fixed launcher is unreadable: {:?}", error.kind())),
    };
    match std::fs::symlink_metadata(prefix.join(record)) {
        Ok(_) => {
            io::read(&prefix.join(record), RECORD_BYTES)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "installation bootstrap is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    match std::fs::symlink_metadata(&temporary) {
        Ok(_) => {
            drop(io::regular(&temporary)?);
            std::fs::remove_file(&temporary).map_err(|error| {
                format!("launcher partial cannot be removed: {:?}", error.kind())
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "launcher partial is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    let input = launcher_input(package, name)?.ok_or(PRE_RENAME)?;
    stage::copy(&package.root.join(&input.relative), &temporary, input)?;
    io::sync_directory(&bin)?;
    let receipt = Receipt {
        schema: SCHEMA.to_owned(),
        source_commit: package.commit.clone(),
        sha256: input.sha256.clone(),
        bytes: input.bytes,
    };
    let stamp = verify(&temporary, &receipt)?;
    Ok(Update::Refresh(Box::new(Refresh {
        path,
        temporary,
        stamp,
        previous,
        receipt_path: prefix.join(record),
        receipt,
    })))
}

#[cfg(all(test, windows))]
pub(in super::super) fn prepare(
    prefix: &Path,
    package: &package::Package,
) -> Result<Prepared, String> {
    prepare_named(prefix, package, "grapt.exe", "bootstrap.json")
}

#[cfg(windows)]
pub(in super::super) fn prepare_update(
    prefix: &Path,
    package: &package::Package,
    refresh: bool,
) -> Result<Update, String> {
    prepare_update_named(prefix, package, refresh, "grapt.exe", "bootstrap.json")
}
