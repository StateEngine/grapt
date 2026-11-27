use super::{display, files, graph, roots, ImportSource, Path};
use grapt_types::profile_names::{choose, Spelling};

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn discover(
    directory: &Path,
    home: &Path,
    source: ImportSource,
    plan: &mut files::Plan,
) -> Result<(), String> {
    if source == ImportSource::Codex {
        for name in ["AGENTS.md", "AGENTS.override.md"] {
            let path = directory.join(name);
            if files::exists(&path)? {
                plan.row(format!(
                    "already used: {} (native instruction discovery)",
                    display(&path)
                ))?;
            }
        }
        let source_root = home.join(".codex");
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let path = source_root.join(name);
            if !files::exists(&path)? {
                continue;
            }
            let config = config_root(home)?;
            let mirror = project_dir(&config).join("imports/codex");
            let imported = mirror_global(
                &path,
                &source_root,
                &mirror,
                plan,
                &mut Vec::new(),
                0,
                &mut (0, 0),
            )?;
            let target = if files::exists(&config.join("AGENTS.md"))?
                || files::exists(&config.join("AGENTS.override.md"))?
            {
                plan.row("existing global instructions win; AGENTS.import-codex.md is side by side and requires an explicit include".into())?;
                config.join("AGENTS.import-codex.md")
            } else {
                config.join("AGENTS.md")
            };
            plan.add(
                target.clone(),
                format!("{}\n", include(&config, &imported)?).into_bytes(),
            )?;
            plan.instruction_root(target, config, false)?;
            break;
        }
        return Ok(());
    }
    for (vendor, scope) in roots(directory, home, source) {
        // Global instructions belong in Grapt's configuration directory.
        let target_scope = if scope == home {
            config_root(home)?
        } else {
            scope.clone()
        };
        let mut ambient = String::new();
        let candidates = if scope == home {
            vec![vendor.join("CLAUDE.md")]
        } else {
            vec![
                scope.join("CLAUDE.md"),
                scope.join("CLAUDE.local.md"),
                vendor.join("CLAUDE.md"),
            ]
        };
        for path in candidates {
            if let Some(bytes) = files::read(&path)? {
                if bytes.len() > 65_536 {
                    return Err("instruction source exceeds the native 64 KiB bound".into());
                }
                std::str::from_utf8(&bytes).map_err(|_| "instruction source is not UTF-8")?;
                if scope != home {
                    let relative = path
                        .strip_prefix(&scope)
                        .map_err(|_| "instruction escaped its scope")?;
                    ambient.push_str(&format!(
                        "\n@{}\n",
                        relative.to_string_lossy().replace('\\', "/")
                    ));
                } else {
                    let mirror = project_dir(&target_scope).join("imports/claude");
                    let imported = mirror_global(
                        &path,
                        &vendor,
                        &mirror,
                        plan,
                        &mut Vec::new(),
                        0,
                        &mut (0, 0),
                    )?;
                    ambient.push_str(&format!("\n{}\n", include(&target_scope, &imported)?));
                }
            }
        }
        for path in files::scan(&vendor.join("rules"))? {
            if !super::rules::markdown(&path) {
                continue;
            }
            let bytes = files::read(&path)?.ok_or("rule disappeared")?;
            if bytes.len() > 65_536 {
                return Err("instruction source exceeds the native 64 KiB bound".into());
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| "rule is not UTF-8")?;
            let has_paths = super::metadata::scoped(text)?;
            if has_paths && scope != home {
                let rule_root = vendor.join("rules");
                let relative = path
                    .strip_prefix(&rule_root)
                    .map_err(|_| "rule escaped its source")?;
                let name = path.file_name().ok_or("rule filename missing")?;
                let name = if relative.components().count() == 1 {
                    name.to_string_lossy().into_owned()
                } else {
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(relative.to_string_lossy().as_bytes());
                    format!("claude-{:x}-{}", digest, name.to_string_lossy())
                };
                let base = relative
                    .parent()
                    .ok_or("rule has no parent")?
                    .to_string_lossy()
                    .replace('\\', "/");
                let rebased = rebase_rule_imports(text, &base)?;
                if rebased.len() > 65_536 {
                    return Err("rebased rule exceeds the native 64 KiB bound".into());
                }
                let target = project_dir(&target_scope).join("rules").join(name);
                let target = if files::exists(&target)? {
                    plan.row(format!(
                        "existing rule wins; {}.import-claude is side by side and is not loaded",
                        display(&target)
                    ))?;
                    target.with_file_name(format!(
                        "{}.import-claude",
                        target
                            .file_name()
                            .ok_or("rule filename missing")?
                            .to_string_lossy()
                    ))
                } else {
                    target
                };
                plan.add(target.clone(), rebased.into_bytes())?;
                plan.instruction_root(target, target_scope.clone(), true)?;
            } else if scope != home {
                let relative = path
                    .strip_prefix(&scope)
                    .map_err(|_| "rule escaped scope")?;
                ambient.push_str(&format!(
                    "\n@{}\n",
                    relative.to_string_lossy().replace('\\', "/")
                ));
            } else if !has_paths {
                let mirror = project_dir(&target_scope).join("imports/claude");
                let imported = mirror_global(
                    &path,
                    &vendor,
                    &mirror,
                    plan,
                    &mut Vec::new(),
                    0,
                    &mut (0, 0),
                )?;
                ambient.push_str(&format!("\n{}\n", include(&target_scope, &imported)?));
            } else {
                plan.row(format!(
                    "not imported: {} (global path-scoped rules have no native home-directory scope)",
                    display(&path)
                ))?;
            }
        }
        if !ambient.is_empty() {
            let agents = target_scope.join("AGENTS.md");
            let target = if files::exists(&agents)?
                || files::exists(&target_scope.join("AGENTS.override.md"))?
            {
                plan.row("existing instructions win; AGENTS.import-claude.md is side by side and needs an explicit @AGENTS.import-claude.md include".into())?;
                target_scope.join("AGENTS.import-claude.md")
            } else {
                agents
            };
            plan.add(target.clone(), ambient.into_bytes())?;
            plan.instruction_root(target, target_scope, false)?;
        }
    }
    Ok(())
}

// Mirror only referenced global instruction files, never the surrounding
// settings, credentials or unrelated home files. References use acquired source
// names, so mirrors remain valid even across filesystems with different casing.
fn mirror_global(
    path: &Path,
    root: &Path,
    target: &Path,
    plan: &mut files::Plan,
    active: &mut Vec<std::path::PathBuf>,
    depth: usize,
    budget: &mut (usize, usize),
) -> Result<std::path::PathBuf, String> {
    if depth >= 16 {
        return Err("instruction imports exceed 16 active files".into());
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| "global instruction import is missing")?;
    let root = std::fs::canonicalize(root).map_err(|_| "global instruction root is unavailable")?;
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| "global instruction import escapes its source configuration directory")?;
    if active.contains(&canonical) {
        return Err("global instruction imports contain a cycle".into());
    }
    let bytes = files::read(&canonical)?.ok_or("global instruction import disappeared")?;
    budget.0 = budget.0.saturating_add(1);
    budget.1 = budget.1.saturating_add(bytes.len());
    if budget.0 > 64 || budget.1 > 1024 * 1024 {
        return Err("instruction imports exceed 64 source occurrences or 1 MiB".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "global instruction import is not UTF-8")?;
    active.push(canonical.clone());
    if bytes.len() > 65_536 {
        return Err("instruction source exceeds the native 64 KiB bound".into());
    }
    let destination = target.join(relative);
    let parent = destination
        .parent()
        .ok_or("mirrored instruction has no parent")?;
    let mut output = String::new();
    let mut fence = graph::Fence::default();
    for line in text.split_inclusive('\n') {
        if let Some(reference) = fence.reference(line)? {
            let next = canonical
                .parent()
                .ok_or("instruction import has no parent")?
                .join(reference);
            let imported = mirror_global(&next, &root, target, plan, active, depth + 1, budget)?;
            output.push_str(&include(parent, &imported)?);
            if line.ends_with("\r\n") {
                output.push_str("\r\n");
            } else if line.ends_with('\n') {
                output.push('\n');
            }
        } else {
            output.push_str(line);
        }
    }
    active.pop();
    plan.add(destination.clone(), output.into_bytes())?;
    Ok(destination)
}

fn include(base: &Path, target: &Path) -> Result<String, String> {
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = target.components().collect();
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return Err("instruction reference has no shared root".into());
    }
    let mut relative = std::path::PathBuf::new();
    for _ in &base[common..] {
        relative.push("..");
    }
    for component in &target[common..] {
        relative.push(component.as_os_str());
    }
    let name = relative
        .to_str()
        .ok_or("instruction reference path is not UTF-8")?
        .replace('\\', "/");
    if name.is_empty() || name.chars().any(char::is_whitespace) || name.contains([':', '\0']) {
        return Err("instruction reference cannot be represented by native @path grammar".into());
    }
    Ok(format!("@{name}"))
}

fn rebase_rule_imports(text: &str, base: &str) -> Result<String, String> {
    let mut output = String::new();
    let mut fence = graph::Fence::default();
    for line in text.split_inclusive('\n') {
        if let Some(reference) = fence.reference(line)? {
            let separator = if base.is_empty() { "" } else { "/" };
            output.push_str(&format!(
                "@../../.claude/rules/{base}{separator}{reference}"
            ));
            if line.ends_with('\n') {
                output.push('\n');
            }
        } else {
            output.push_str(line);
        }
    }
    Ok(output)
}

/// The pre-rename name of the config directory override, still honoured.
const LEGACY_CONFIG_DIR_ENV: &str = "SECODE_CONFIG_DIR"; // grapt-rename: keep

fn present(path: &Path) -> bool {
    !matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// A scope's project directory: `.grapt`, or its pre-rename name when only
/// that one exists, so imports land where the product reads them.
fn project_dir(scope: &Path) -> std::path::PathBuf {
    let current = scope.join(Spelling::Current.project_dir());
    let legacy = scope.join(Spelling::Legacy.project_dir());
    match choose(present(&current), present(&legacy)).spelling {
        Spelling::Current => current,
        Spelling::Legacy => legacy,
    }
}

fn config_root(home: &Path) -> Result<std::path::PathBuf, String> {
    let path = std::env::var_os("GRAPT_CONFIG_DIR")
        .or_else(|| std::env::var_os(LEGACY_CONFIG_DIR_ENV))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            let current = base.join(Spelling::Current.profiles_dir());
            let legacy = base.join(Spelling::Legacy.profiles_dir());
            match choose(present(&current), present(&legacy)).spelling {
                Spelling::Current => current,
                Spelling::Legacy => legacy,
            }
        });
    if !path.is_absolute() {
        return Err("import config directory must be absolute".into());
    }
    graph::normalized(&path)
}
