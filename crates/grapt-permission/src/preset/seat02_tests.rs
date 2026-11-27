use super::*;

fn effect(preset: Preset, action: &str, resource: &str) -> Effect {
    decide(
        action,
        &[resource],
        &[Layer::owner(), Layer::preset_in(preset, Some("/repo"))],
    )
    .effect
}

#[test]
fn seat02_maintainer_allows_ordinary_project_and_external_tool_work() {
    assert_eq!(DEFAULT_PRESET, Preset::Maintainer);
    for action in [
        "fs.create",
        "fs.modify",
        "fs.overwrite",
        "fs.move",
        "fs.delete",
        "mcp_server_tool",
        "plugin_tool",
    ] {
        assert_eq!(
            effect(Preset::Maintainer, action, "/repo/file.txt"),
            Effect::Allow,
            "{action}"
        );
    }
    assert_eq!(
        effect(Preset::Author, "fs.delete", "/repo/file.txt"),
        Effect::Ask
    );
    assert_eq!(
        effect(Preset::Inspect, "fs.delete", "/repo/file.txt"),
        Effect::Deny
    );
}

#[test]
fn seat02_maintainer_destructive_shell_exception_includes_recursive_deletes() {
    for command in [
        "rm -rf build",
        "rm --recursive build",
        "git reset --hard",
        "echo x > /outside/file",
    ] {
        assert_eq!(
            effect(Preset::Maintainer, "shell", command),
            Effect::Ask,
            "{command}"
        );
    }
    for command in ["cargo test", "echo x > /repo/file", "mv /repo/a /repo/b"] {
        assert_eq!(
            effect(Preset::Maintainer, "shell", command),
            Effect::Allow,
            "{command}"
        );
    }
}

#[test]
fn seat02_maintainer_external_directory_exception_is_explicit() {
    assert_eq!(
        effect(Preset::Maintainer, "external_directory", "/outside"),
        Effect::Ask
    );
    assert_eq!(
        effect(Preset::Maintainer, "read", "/outside/file"),
        Effect::Allow
    );
    assert_eq!(
        effect(Preset::Author, "external_directory", "/outside"),
        Effect::Deny
    );
}

#[test]
fn seat02_maintainer_owner_floor_keeps_authority_edits_attended() {
    for path in [
        "/repo/grapt.json",
        "/repo/grapt.jsonc",
        "/profile/service.conf",
    ] {
        for action in [
            "fs.create",
            "fs.modify",
            "fs.overwrite",
            "fs.move",
            "fs.delete",
        ] {
            assert_eq!(
                effect(Preset::Maintainer, action, path),
                Effect::Ask,
                "{action}: {path}"
            );
        }
    }
    assert_eq!(
        effect(Preset::Maintainer, "fs.overwrite", "/repo/ordinary.json"),
        Effect::Allow
    );
}

#[test]
fn seat02_bypass_skips_owner_config_and_ancestor_ask_and_deny_layers() {
    for protection in [Effect::Ask, Effect::Deny] {
        let mut layers = vec![Layer::owner(), Layer::preset(Preset::Bypass)];
        for name in [LayerName::Config, LayerName::Ancestor] {
            layers.push(Layer {
                name,
                rules: vec![Rule::new("*", "*", protection)],
                floor: true,
                compose: Compose::Narrow,
            });
        }
        for (action, resource) in [
            ("fs.delete", "/repo/grapt.json"),
            ("shell", "rm -rf build"),
            ("external_directory", "/outside"),
            ("mcp_server_tool", "server/tool"),
        ] {
            assert_eq!(
                decide(action, &[resource], &layers).effect,
                Effect::Allow,
                "{action}: {protection:?}"
            );
            assert!(!withheld(action, &layers), "bypass hid {action}");
        }
        layers.retain(|layer| layer.name != LayerName::Bypass);
        assert_ne!(
            decide("fs.delete", &["/repo/grapt.json"], &layers).effect,
            Effect::Allow
        );
    }
}

#[test]
fn seat02_bypass_is_named_and_stricter_selections_still_win() {
    assert_eq!(Preset::parse("bypass"), Some(Preset::Bypass));
    assert_ne!(DEFAULT_PRESET, Preset::Bypass);
    for preset in [Preset::Inspect, Preset::Author, Preset::Maintainer] {
        assert_eq!(preset.strictest(Preset::Bypass), preset);
        assert_eq!(Preset::Bypass.strictest(preset), preset);
    }
}
