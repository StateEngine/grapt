//! Capture an explicitly selected profile without recovering or publishing it.
//! Destination writes use the connection install owner inside profile staging.
use super::*;
use grapt_login::{
    chatgpt::{SignInFileLock, SignInFiles},
    vault::DestinationBinding,
};
use grapt_types::Timestamp;

mod material;
use material::Material;

/// Retains the source connection writer and OAuth renewal exclusions until the
/// new profile is published. No credential value has a diagnostic formatter.
pub struct ProfileImport {
    source: Store,
    records: Connections,
    material: Vec<(Connection, Vec<u8>, Option<Material>)>,
    renewals: Vec<(SignInFiles, SignInFileLock)>,
    snapshots: Vec<(String, zeroize::Zeroizing<Vec<u8>>)>,
}

impl std::fmt::Debug for ProfileImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProfileImport(<private-account>)")
    }
}

impl ProfileImport {
    /// Retain the legacy choice only while managed publication stays absent.
    pub fn require_absent(profile: &Path) -> io::Result<()> {
        pending(profile)?;
        if Store::read_snapshot(profile)?.is_some() {
            return Err(invalid("account connections appeared during legacy import"));
        }
        Ok(())
    }

    /// Only genuinely absent metadata permits the caller's legacy import path.
    /// Pending publication refuses instead of recovering the source account.
    pub fn capture(profile: &Path) -> io::Result<Option<Self>> {
        pending(profile)?;
        let Some(before) = Store::read_snapshot(profile)? else {
            return Ok(None);
        };
        let source = Store::lock(profile)?;
        source.require_no_install()?;
        pending(profile)?;
        if source.load()?.as_ref() != Some(&before) {
            return Err(invalid("account connections changed during import"));
        }
        let mut material = Vec::with_capacity(before.connections.len());
        let mut renewals = Vec::new();
        let mut snapshots = Vec::new();
        for record in &before.connections {
            snapshots.extend(material::snapshots(profile, record)?);
            let (secret, renewal) = Material::capture(profile, record)?;
            if let Some(renewal) = renewal {
                renewals.push(renewal);
            }
            let listing = profile_security::read_owner_only_file(
                &profile.join(&record.discovery_file),
                4 * 1024 * 1024,
            )?;
            material.push((record.clone(), listing, Some(secret)));
        }
        let import = Self {
            source,
            records: before,
            material,
            renewals,
            snapshots,
        };
        import.check()?;
        Ok(Some(import))
    }

    /// Recheck immediately before the outer staged profile publication.
    pub fn check(&self) -> io::Result<()> {
        self.source.require_no_install()?;
        pending(&self.source.root)?;
        if grapt_login::chatgpt::has_pending_switch(&self.source.root)?
            || self.source.load()?.as_ref() != Some(&self.records)
        {
            return Err(invalid("account connections changed during import"));
        }
        for (record, listing, _) in &self.material {
            if profile_security::read_owner_only_file(
                &self.source.root.join(&record.discovery_file),
                4 * 1024 * 1024,
            )? != *listing
            {
                return Err(invalid("account model listing changed during import"));
            }
        }
        for (files, _) in &self.renewals {
            files
                .check_generation(&self.source.root)
                .map_err(io::Error::other)?;
        }
        for (name, bytes) in &self.snapshots {
            let current = zeroize::Zeroizing::new(profile_security::read_owner_only_file(
                &self.source.root.join(name),
                profile_security::stored_bound(32 * 1024),
            )?);
            if current.as_slice() != bytes.as_slice() {
                return Err(invalid("account credential changed during import"));
            }
        }
        Ok(())
    }

    /// The caller owns a fresh, unpublished private destination. Install only
    /// captured inputs; do not renew tokens or discover models over the network.
    pub fn install(&mut self, root: &Path, token: &str) -> io::Result<()> {
        self.check()?;
        let mut store = Store::open(root)?;
        if store.load()?.is_some() {
            return Err(invalid("account import requires empty connection storage"));
        }
        let mut records = Connections {
            connections: Vec::new(),
            default_model: None,
        };
        for (index, (source_record, listing, material)) in self.material.iter_mut().enumerate() {
            let mut record = source_record.clone();
            let (credential, discovery) =
                next_key_files(&record.id, None, &format!("{token}-{index}"))?;
            record.credential_file = credential;
            record.discovery_file = discovery;
            let material = material
                .take()
                .ok_or_else(|| invalid("account import material was already consumed"))?;
            let commit = match material {
                Material::Legacy(bytes) => {
                    let stored = zeroize::Zeroizing::new(profile_security::seal(&bytes)?);
                    profile_security::replace_private_file(root, &record.credential_file, &stored)?;
                    profile_security::replace_private_file(root, &record.discovery_file, listing)?;
                    records.connections.push(record);
                    store.save(&records)?;
                    None
                }
                Material::Environment => {
                    profile_security::replace_private_file(root, &record.discovery_file, listing)?;
                    records.connections.push(record);
                    store.save(&records)?;
                    None
                }
                Material::Vault {
                    secret,
                    binding,
                    oauth,
                    updated,
                } => {
                    if oauth {
                        record.oauth_file = Some(format!(
                            "{}.oauth.json",
                            record.credential_file.trim_end_matches(".credential")
                        ));
                        record.credential_format = CredentialFormat::VaultedOAuth {
                            binding: binding.clone(),
                        };
                    }
                    let id = record.id.clone();
                    records.connections.push(record);
                    Some(if oauth {
                        store
                            .install_oauth_with(
                                &records,
                                &id,
                                secret,
                                &binding,
                                grapt_login::chatgpt::ISSUER,
                                listing,
                                updated,
                                || Ok(()),
                            )?
                            .1
                    } else {
                        store.install_key(&records, &id, secret, &binding, listing, updated)?
                    })
                }
            };
            if commit.is_some_and(|commit| {
                commit.durability_error.is_some() || commit.cleanup_error.is_some()
            }) {
                return Err(io::Error::other(
                    "imported connection publication did not finish",
                ));
            }
        }
        records.default_model = self.records.default_model.clone();
        store.save(&records)?;
        self.check()
    }
}

fn pending(root: &Path) -> io::Result<()> {
    for name in [
        STAGED,
        "connection-install.json",
        ".connection-install.json.new",
        "connection-removal.json",
        ".connection-removal.json.new",
        ".chatgpt-switch",
    ] {
        match std::fs::symlink_metadata(root.join(name)) {
            Ok(_) => return Err(invalid(
                "account publication is unfinished; finish the source operation before importing",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
