//! Committed removal finishes before the next writer can install anything.
//! Extends the private auth-file deletion.
//! Copyright 2025 OpenAI. Apache-2.0; modified by Grapt: bounded intent,
//! exact before/after metadata and the same OAuth renewal lock.
use super::*;
use grapt_login::{
    chatgpt::SignInFiles,
    vault::{CredentialVault, SealedFileVault, VaultKey},
};
use grapt_types::ProviderId;
use serde::{Deserialize, Serialize};

const INTENT: &str = "connection-removal.json";
const INTENT_CAP: usize = 4 * BYTE_CAP + 1024;
const MATERIAL_CAP: usize = 64 * 1024;
const LISTING_CAP: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    connection: String,
    before: String,
    after: String,
}

struct Material {
    record: Connection,
    credential: String,
    oauth: Option<String>,
    listing: String,
}
impl Material {
    fn files(&self, root: &Path) -> io::Result<Option<SignInFiles>> {
        if self.record.method != AuthMethod::OauthDevice {
            if let Some(state) = &self.oauth {
                match std::fs::symlink_metadata(root.join(state)) {
                    Ok(_) => (),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error),
                }
            }
        }
        self.oauth
            .as_ref()
            .map(|state| SignInFiles::new(state, &self.credential).map_err(io::Error::other))
            .transpose()
    }
    fn paths(&self) -> Vec<String> {
        let mut paths = if matches!(self.record.method, AuthMethod::Env { .. })
            && !self.credential.starts_with("connect-")
        {
            vec![self.listing.clone()]
        } else {
            vec![self.credential.clone(), self.listing.clone()]
        };
        if let Some(state) = &self.oauth {
            paths.push(state.clone());
            paths.push(format!("{}.owner", state.trim_end_matches(".oauth.json")));
        }
        paths
    }
}

impl Intent {
    fn checked(&self) -> io::Result<(Connections, Connections, Vec<Material>)> {
        let before = Connections::from_slice(self.before.as_bytes())?;
        let after = Connections::from_slice(self.after.as_bytes())?;
        let id = ProviderId::new(&self.connection)
            .ok_or_else(|| invalid("invalid removed connection"))?;
        let selected = before
            .connections
            .iter()
            .find(|record| record.id == id)
            .ok_or_else(|| invalid("removed connection was not published"))?;
        let remaining: Vec<_> = before
            .connections
            .iter()
            .filter(|record| record.id != id)
            .cloned()
            .collect();
        if after.connections != remaining {
            return Err(invalid("credential removal changes another connection"));
        }
        let mut material = vec![Material {
            record: selected.clone(),
            credential: selected.credential_file.clone(),
            oauth: selected.oauth_file.clone(),
            listing: selected.discovery_file.clone(),
        }];
        // Installed slots are a closed pair. Legacy references authorize only
        // their explicit files, never a guessed sibling.
        if let Some(stem) = selected.credential_file.strip_suffix(".credential") {
            if stem.starts_with("connect-") && selected.discovery_file == format!("{stem}.json") {
                let alternate = if let Some(stem) = stem.strip_suffix("-a") {
                    Some(format!("{stem}-b"))
                } else {
                    stem.strip_suffix("-b").map(|stem| format!("{stem}-a"))
                };
                if let Some(stem) = alternate {
                    // Both historical key and OAuth material may remain in a
                    // slot after a reconnect changes the authentication kind.
                    material[0].oauth = Some(format!(
                        "{}.oauth.json",
                        selected.credential_file.trim_end_matches(".credential")
                    ));
                    material.push(Material {
                        record: selected.clone(),
                        credential: format!("{stem}.credential"),
                        oauth: Some(format!("{stem}.oauth.json")),
                        listing: format!("{stem}.json"),
                    });
                }
            }
        }
        for slot in &material {
            for name in slot.paths() {
                if name == "basic.credential"
                    || after.connections.iter().any(|record| {
                        record.credential_file == name
                            || record.discovery_file == name
                            || record.oauth_file.as_deref() == Some(name.as_str())
                    })
                {
                    return Err(invalid("removed material overlaps another profile owner"));
                }
            }
        }
        Ok((before, after, material))
    }
}

impl Store {
    /// Commit only the removal of this connection; preparation still happens
    /// before metadata publication. Recovery erases material only after it can
    /// match the complete committed result, and never recreates a credential.
    pub fn remove_with<T>(
        &mut self,
        connection: &ProviderId,
        after: &Connections,
        prepare: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<(T, InstallCommit)> {
        self.require_no_install()?;
        let before = self
            .load()?
            .ok_or_else(|| invalid("connection metadata is absent"))?;
        let intent = Intent {
            connection: connection.as_str().into(),
            before: String::from_utf8(before.to_vec()?)
                .map_err(|_| invalid("invalid removal source"))?,
            after: String::from_utf8(after.to_vec()?)
                .map_err(|_| invalid("invalid removal result"))?,
        };
        let (_, _, material) = intent.checked()?;
        let locks = material
            .iter()
            .map(|slot| slot.files(&self.root))
            .collect::<io::Result<Vec<_>>>()?;
        let locks = locks
            .iter()
            .flatten()
            .map(|files| files.try_lock(&self.root))
            .collect::<io::Result<Vec<_>>>()?;
        let prepared = prepare()?;
        let encoded = serde_json::to_vec(&intent).map_err(|_| invalid("invalid removal intent"))?;
        if encoded.len() > INTENT_CAP {
            return Err(invalid("removal intent exceeds its bound"));
        }
        discard(&self.root, &format!(".{INTENT}.new"), INTENT_CAP)?;
        profile_security::replace_private_file(&self.root, INTENT, &encoded)?;
        sync_directory(&self.root)?;
        let durability_error = self.publish(after)?;
        // Retire both generations under their renewal locks. A failed sync
        // keeps the intent and material until a later owner can finish safely.
        let cleaned = if durability_error.is_none() {
            cleanup(&self.root, &material)
        } else {
            Ok(())
        };
        drop(locks);
        let cleanup_error = if durability_error.is_none() {
            cleaned
                .and_then(|()| retire_locks(&self.root, &material))
                .and_then(|()| {
                    discard(&self.root, INTENT, INTENT_CAP)?;
                    sync_directory(&self.root)
                })
                .err()
                .map(|error| error.kind())
        } else {
            None
        };
        Ok((
            prepared,
            InstallCommit {
                durability_error,
                cleanup_error,
            },
        ))
    }

    pub(super) fn require_no_removal(&self) -> io::Result<()> {
        match profile_security::read_owner_only_file(&self.root.join(INTENT), INTENT_CAP) {
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "unfinished credential removal must be recovered",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn recover_removal(&mut self) -> io::Result<()> {
        discard(&self.root, &format!(".{INTENT}.new"), INTENT_CAP)?;
        let bytes =
            match profile_security::read_owner_only_file(&self.root.join(INTENT), INTENT_CAP) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
        let intent: Intent =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid removal intent"))?;
        let (before, after, material) = intent.checked()?;
        let current = self
            .load()?
            .ok_or_else(|| invalid("removal metadata is absent"))?;
        if current != before && current != after {
            return Err(invalid("removal intent conflicts with published metadata"));
        }
        if current == after {
            let files = material
                .iter()
                .map(|slot| slot.files(&self.root))
                .collect::<io::Result<Vec<_>>>()?;
            let _locks = files
                .iter()
                .flatten()
                .map(|files| files.try_lock(&self.root))
                .collect::<io::Result<Vec<_>>>()?;
            // Establish durability before irreversible cleanup after a prior
            // publication reported an uncertain directory sync.
            sync_directory(&self.root)?;
            cleanup(&self.root, &material)?;
            drop(_locks);
            retire_locks(&self.root, &material)?;
        }
        discard(&self.root, INTENT, INTENT_CAP)?;
        sync_directory(&self.root)
    }
}

fn discard(root: &Path, name: &str, bound: usize) -> io::Result<()> {
    match profile_security::read_owner_only_file(&root.join(name), bound) {
        Ok(bytes) => {
            let _bytes = zeroize::Zeroizing::new(bytes);
            std::fs::remove_file(root.join(name))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup(root: &Path, material: &[Material]) -> io::Result<()> {
    let mut vault = if material
        .iter()
        .any(|slot| slot.record.credential_format.binding().is_some())
    {
        Some(SealedFileVault::open(root).map_err(io::Error::other)?)
    } else {
        None
    };
    for slot in material {
        let mut names = if matches!(slot.record.method, AuthMethod::Env { .. })
            && !slot.credential.starts_with("connect-")
        {
            Vec::new()
        } else {
            vec![slot.credential.as_str()]
        };
        if let Some(state) = &slot.oauth {
            names.push(state);
        }
        for name in names {
            if slot.record.credential_format.binding().is_some() {
                let key = VaultKey::new(slot.record.id.clone(), name).map_err(io::Error::other)?;
                vault
                    .as_mut()
                    .ok_or_else(|| io::Error::other("credential vault owner is absent"))?
                    .remove(&key)
                    .map_err(io::Error::other)?;
            } else {
                discard(root, name, profile_security::stored_bound(MATERIAL_CAP))?;
                discard(
                    root,
                    &format!(".{name}.new"),
                    profile_security::stored_bound(MATERIAL_CAP),
                )?;
            }
        }
        if let Some(state) = &slot.oauth {
            let owner = format!("{}.owner", state.trim_end_matches(".oauth.json"));
            discard(root, &owner, 256)?;
            discard(root, &format!(".{owner}.new"), 256)?;
        }
        discard(root, &slot.listing, LISTING_CAP)?;
        discard(root, &format!(".{}.new", slot.listing), LISTING_CAP)?;
    }
    Ok(())
}

fn retire_locks(root: &Path, material: &[Material]) -> io::Result<()> {
    for slot in material {
        if let Some(state) = &slot.oauth {
            let files = SignInFiles::new(state, &slot.credential).map_err(io::Error::other)?;
            // State and its generation fence are already gone. A queued old
            // renewal can acquire its old inode, but cannot load or save state;
            // new leases and refresh admission refuse the removed incarnation.
            if files != SignInFiles::legacy() {
                discard(root, files.lock_file(), 0)?;
            }
        }
    }
    Ok(())
}
