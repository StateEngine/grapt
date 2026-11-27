use super::{display, files, roots, ImportSource, Path};

pub(super) fn discover(
    directory: &Path,
    home: &Path,
    source: ImportSource,
    plan: &mut files::Plan,
) -> Result<(), String> {
    for (vendor, scope) in roots(directory, home, source) {
        let root = vendor.join("skills");
        if !files::exists(&root)? {
            continue;
        }
        if source == ImportSource::Claude {
            plan.row(format!(
                "already used: {} (native skill discovery)",
                display(&root)
            ))?;
            continue;
        }
        // An outer skill owns its full resource tree, including nested examples
        // named SKILL.md. Choose units before copying, independent of scan order.
        let skills = files::scan(&root)?
            .into_iter()
            .filter(|path| path.file_name().is_some_and(|name| name == "SKILL.md"))
            .map(|path| {
                path.parent()
                    .map(Path::to_path_buf)
                    .ok_or("skill has no directory")
            })
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        let destination_root = scope.join(".agents/skills");
        for parent in &skills {
            if parent
                .ancestors()
                .skip(1)
                .any(|ancestor| skills.contains(ancestor))
            {
                continue;
            }
            let relative = parent
                .strip_prefix(&root)
                .map_err(|_| "skill escaped its source root")?;
            if relative.as_os_str().is_empty() {
                return Err("Codex SKILL.md must live in a named skill directory".into());
            }
            let target = destination_root.join(relative);
            let mut enclosing = None;
            for ancestor in target
                .ancestors()
                .skip(1)
                .take_while(|path| path.starts_with(&destination_root))
            {
                if files::exists(&ancestor.join("SKILL.md"))? {
                    enclosing = Some(ancestor);
                    break;
                }
            }
            if let Some(enclosing) = enclosing {
                plan.row(format!(
                    "kept existing skill {} (encloses {})",
                    display(enclosing),
                    display(&target)
                ))?;
                continue;
            }
            plan.copy_skill(parent, target)?;
        }
    }
    for scope in [directory, home] {
        let shared = scope.join(".agents/skills");
        if files::exists(&shared)? {
            plan.row(format!(
                "already used: {} (native shared skill discovery)",
                display(&shared)
            ))?;
        }
    }
    Ok(())
}
