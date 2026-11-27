//! Skill discovery has a project boundary; explicit roots retain their scope.
use super::*;

#[test]
fn repository_skills_exclude_ancestors_and_keep_pinned_and_explicit_sources(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-skill-boundary-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let project = root.join("project");
    let nested = project.join("nested");
    std::fs::create_dir_all(&nested)?;
    // Both a regular Git directory and a linked-worktree marker are supported
    // by the same resolver that owns the selected repository location.
    for linked in [false, true] {
        let marker = project.join(".git");
        if linked {
            std::fs::remove_dir(&marker)?;
            std::fs::write(&marker, "gitdir: fixture-worktree\n")?;
        } else {
            std::fs::create_dir(&marker)?;
        }
        let environment = CatalogEnvironment::isolated(&root);
        let fixtures = [
            (
                root.join(".claude/skills/ancestor-claude"),
                "ancestor-claude",
                false,
            ),
            (
                root.join(".agents/skills/ancestor-agents"),
                "ancestor-agents",
                false,
            ),
            (
                root.join(".grapt/skill/ancestor-legacy"),
                "ancestor-legacy",
                false,
            ),
            (
                root.join(".grapt/skills/ancestor-config"),
                "ancestor-config",
                false,
            ),
            (
                environment.home.join(".claude/skills/user-claude"),
                "user-claude",
                true,
            ),
            (
                environment.home.join(".agents/skills/user-agents"),
                "user-agents",
                true,
            ),
            (
                environment.config.join("skill/config-legacy"),
                "config-legacy",
                true,
            ),
            (
                environment.config.join("skills/config-current"),
                "config-current",
                true,
            ),
            (project.join(".agents/skills/project"), "project", true),
            (nested.join(".claude/skills/nested"), "nested", true),
            (
                project.join(".grapt/skills/project-config"),
                "project-config",
                true,
            ),
            (root.join("explicit/configured"), "configured", true),
        ];
        for (directory, id, _) in &fixtures {
            std::fs::create_dir_all(directory)?;
            std::fs::write(
                directory.join("SKILL.md"),
                format!("---\nname: {id}\ndescription: {id} fixture\n---\nFixture instructions.\n"),
            )?;
        }
        // A deliberately configured source may live outside the Git root.
        std::fs::write(
            project.join("grapt.json"),
            serde_json::to_vec(&json!({"skills":[root.join("explicit")]}))?,
        )?;
        let sources = CatalogSources::read(&nested, environment)?;
        let discovered = skills(&sources, &nested)?;
        let ids: BTreeSet<_> = discovered
            .iter()
            .filter_map(|skill| skill["id"].as_str())
            .collect();
        let expected: BTreeSet<_> = fixtures
            .iter()
            .filter(|(_, _, included)| *included)
            .map(|(_, id, _)| *id)
            .collect();
        assert_eq!(ids, expected, "linked worktree: {linked}");
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}
