//! Read-only base, writable roots, then protected and denied carveouts.
//! Copyright 2025 OpenAI.
//! Apache-2.0. Synthetic targets are owned by inode and removed only if empty.
use super::targets::{self, Target};
use crate::Policy;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) struct Mounts {
    pub arguments: Vec<OsString>,
    targets: Vec<Target>,
    registry: PathBuf,
}

fn normalized(path: &Path) -> Result<PathBuf, String> {
    if path.components().count() >= 128 {
        return Err("sandbox protected target exceeds 128 ancestors".into());
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => result.push("/"),
            Component::Normal(name) => result.push(name),
            Component::CurDir => {}
            Component::ParentDir if result != Path::new("/") => {
                result.pop();
            }
            _ => return Err("sandbox path escapes its filesystem root".into()),
        }
    }
    Ok(result)
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path)
        .map_err(|e| format!("sandbox root {} is unavailable: {e}", path.display()))
}

impl Mounts {
    pub fn native_leases(&self) -> impl Iterator<Item = &Path> {
        self.targets.iter().map(Target::lease_path)
    }

    pub fn prepare(policy: &Policy, grants: &[crate::WriteTarget]) -> Result<Self, String> {
        Self::prepare_with_aliases(policy, grants, &[])
    }

    /// Platform aliases point at canonical read roots already in the policy.
    /// Create them before writable mounts and carveouts, so a library alias
    /// cannot restore access beneath a protected or denied path.
    #[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
    pub fn prepare_with_aliases(
        policy: &Policy,
        grants: &[crate::WriteTarget],
        aliases: &[(PathBuf, PathBuf)],
    ) -> Result<Self, String> {
        let mut mounts = Self {
            arguments: Vec::new(),
            targets: Vec::new(),
            registry: targets::registry()?,
        };
        let mut readable = policy
            .readable_roots
            .iter()
            .map(|p| canonical(p))
            .collect::<Result<Vec<_>, _>>()?;
        let mut writable = policy
            .writable_roots
            .iter()
            .map(|p| {
                if grants.iter().any(|grant| grant.path() == p) {
                    Ok(p.clone())
                } else {
                    canonical(p)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        readable.sort_by_key(|p| p.components().count());
        writable.sort_by_key(|p| p.components().count());
        // A read-only bind covers names, while an existing writable hard-link
        // alias can still change the same inode through another mount.
        let carveouts = policy
            .protected_paths
            .iter()
            .chain(&policy.denied_paths)
            .cloned()
            .collect::<Vec<_>>();
        crate::aliases::check(&carveouts, &writable)?;
        if readable.iter().any(|p| p == Path::new("/")) {
            mounts.bind("--ro-bind", Path::new("/"), Path::new("/"));
        } else {
            mounts.arguments.extend(["--tmpfs".into(), "/".into()]);
            for path in &readable {
                mounts.bind("--ro-bind", path, path);
            }
        }
        for (canonical, alias) in aliases {
            if !readable.iter().any(|root| alias.starts_with(root)) {
                mounts.arguments.extend([
                    "--symlink".into(),
                    canonical.as_os_str().into(),
                    alias.as_os_str().into(),
                ]);
            }
        }
        mounts.arguments.extend([
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
        ]);
        // System device nodes are mounted by the launcher after these binds.
        // A grant must never expose host /proc, /sys or device trees writable.
        for path in &writable {
            if ["/proc", "/sys", "/dev"]
                .iter()
                .any(|system| Path::new(system).starts_with(path) || path.starts_with(system))
            {
                return Err(format!(
                    "sandbox writable root overlaps a kernel filesystem: {}",
                    path.display()
                ));
            }
            match grants.iter().find(|grant| grant.path() == path) {
                Some(grant) => {
                    use std::os::fd::AsRawFd;
                    mounts.arguments.extend([
                        "--bind-fd".into(),
                        grant.native_descriptor().as_raw_fd().to_string().into(),
                        path.as_os_str().into(),
                    ]);
                }
                None => mounts.bind("--bind", path, path),
            }
        }
        let mut protected = policy
            .protected_paths
            .iter()
            .map(|p| normalized(p))
            .collect::<Result<Vec<_>, _>>()?;
        let denied = policy
            .denied_paths
            .iter()
            .map(|p| normalized(p))
            .collect::<Result<Vec<_>, _>>()?;
        mounts.pin_parents(protected.iter().chain(&denied), &writable)?;
        protected.sort_by_key(|p| p.components().count());
        let mut covered = Vec::<PathBuf>::new();
        for path in protected {
            if covered.iter().any(|ancestor| path.starts_with(ancestor)) {
                continue;
            }
            mounts.mask(&path, &writable, false)?;
            covered.push(path);
        }
        for path in &denied {
            mounts.mask(path, &writable, true)?;
        }
        // Commands may have temp write authority, but cannot forge target
        // ownership or unlink leases belonging to another active namespace.
        let registry = mounts.registry.clone();
        mounts.bind("--ro-bind", &registry, &registry);
        Ok(mounts)
    }

    fn bind(&mut self, flag: &str, source: &Path, target: &Path) {
        self.arguments.extend([
            flag.into(),
            source.as_os_str().into(),
            target.as_os_str().into(),
        ]);
    }

    fn pin_parents<'a>(
        &mut self,
        protected: impl Iterator<Item = &'a PathBuf>,
        writable: &[PathBuf],
    ) -> Result<(), String> {
        let mut parents = Vec::new();
        for path in protected {
            for parent in path.ancestors().skip(1) {
                if writable.iter().any(|root| root == parent) {
                    // This root is already a mount point. A broader grant
                    // can still make its parents mutable, so keep walking.
                    continue;
                }
                if !writable.iter().any(|root| parent.starts_with(root)) {
                    continue;
                }
                match fs::symlink_metadata(parent) {
                    Ok(metadata) if metadata.is_dir() => parents.push(parent.to_path_buf()),
                    Ok(_) => {
                        return Err("sandbox protected parent is not an ordinary directory".into())
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!("sandbox protected parent unavailable: {error}"))
                    }
                }
            }
        }
        parents.sort();
        parents.dedup();
        parents.sort_by_key(|path| path.components().count());
        for path in parents {
            // Pin the name of every mutable ancestor before applying masks.
            // Otherwise renaming .git could expose a fresh writable config at
            // its old name while the read-only mount moves with the old .git.
            self.bind("--bind", &path, &path);
        }
        Ok(())
    }

    fn mask(&mut self, path: &Path, writable: &[PathBuf], denied: bool) -> Result<(), String> {
        if ["/proc", "/sys", "/dev"]
            .iter()
            .any(|system| path.starts_with(system) || Path::new(system).starts_with(path))
        {
            return Err(format!(
                "sandbox carveout overlaps a kernel filesystem: {}",
                path.display()
            ));
        }
        if !denied
            && !writable
                .iter()
                .any(|root| path.starts_with(root) || root.starts_with(path))
        {
            return Ok(());
        }
        let mut current = PathBuf::from("/");
        for part in path.components().skip(1) {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.is_symlink() => {
                    // A mutable link can be retargeted after policy resolution.
                    if writable.iter().any(|root| current.starts_with(root)) {
                        return Err(format!(
                            "sandbox protected path crosses a writable symlink: {}",
                            current.display()
                        ));
                    }
                }
                Ok(_) => {
                    if let Some(target) = Target::acquire(&self.registry, &current, false)? {
                        self.bind("--ro-bind", &current, &current);
                        self.targets.push(target);
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !writable.iter().any(|root| current.starts_with(root)) {
                        return Ok(());
                    }
                    // A shared lease keeps the synthetic target present until
                    // the last namespace using its read-only mount has exited.
                    let target = Target::acquire(&self.registry, &current, true)?
                        .ok_or("sandbox target lease is absent")?;
                    self.targets.push(target);
                    self.bind("--ro-bind", &current, &current);
                    return Ok(());
                }
                Err(error) => {
                    return Err(format!(
                        "sandbox protected path {} is unavailable: {error}",
                        current.display()
                    ))
                }
            }
        }
        let actual = canonical(path)?;
        if denied {
            if actual.is_dir() {
                self.arguments.extend([
                    "--tmpfs".into(),
                    actual.as_os_str().into(),
                    "--remount-ro".into(),
                    actual.as_os_str().into(),
                ]);
            } else {
                self.bind("--ro-bind", Path::new("/dev/null"), &actual);
            }
        } else {
            self.bind("--ro-bind", &actual, &actual);
        }
        Ok(())
    }
}
