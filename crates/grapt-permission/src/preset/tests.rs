use super::*;

#[test]
fn sec03_materialized_command_grants_preserve_literal_case_slashes_and_globs() {
    let command = r"printf 'A\B*?'";
    let layers = [
        Layer::preset(Preset::Maintainer),
        Layer {
            name: LayerName::Config,
            rules: vec![Rule::new("shell", "*", Effect::Ask)],
            floor: false,
            compose: Compose::Narrow,
        },
        Layer {
            name: LayerName::Config,
            rules: vec![Rule::literal("shell", command, Effect::Allow)],
            floor: false,
            compose: Compose::Override,
        },
    ];
    assert_eq!(decide("shell", &[command], &layers).effect, Effect::Allow);
    for unrelated in [r"printf 'A/B*?'", r"printf 'a\b*?'", r"printf 'A\Bxx'"] {
        assert_eq!(
            decide("shell", &[unrelated], &layers).effect,
            Effect::Ask,
            "a command grant was broadened by resource normalization"
        );
    }
    let literal_star = [Layer {
        name: LayerName::Config,
        rules: vec![Rule::literal("shell", "*", Effect::Deny)],
        floor: false,
        compose: Compose::Override,
    }];
    assert!(!withheld("shell", &literal_star));
    assert_eq!(decide("shell", &["*"], &literal_star).effect, Effect::Deny);
    assert_eq!(
        decide("shell", &["echo ordinary"], &literal_star).effect,
        Effect::Ask
    );
}

#[test]
fn checklist_updates_still_obey_an_explicit_deny() {
    for preset in [Preset::Inspect, Preset::Author, Preset::Maintainer] {
        let denied = Layer {
            name: LayerName::Config,
            rules: vec![Rule::new("update_plan", "*", Effect::Deny)],
            floor: false,
            compose: Compose::Override,
        };
        assert_eq!(
            decide(
                "update_plan",
                &["task checklist"],
                &[Layer::owner(), Layer::preset(preset), denied]
            )
            .effect,
            Effect::Deny
        );
    }
}

#[test]
fn checklist_updates_are_prompt_free_only_for_the_fixed_local_resource() {
    for preset in Preset::ALL {
        assert_eq!(
            under(preset, "update_plan", "task checklist"),
            Effect::Allow
        );
        for resource in [
            "/repo/a",
            "/repo/grapt.json",
            "server/execute",
            "task checklist/../shell",
        ] {
            assert_eq!(
                under(preset, "update_plan", resource),
                if preset == Preset::Inspect {
                    Effect::Deny
                } else if matches!(preset, Preset::Author | Preset::Maintainer) {
                    Effect::Ask
                } else {
                    Effect::Allow
                }
            );
        }
        assert_eq!(
            under(preset, "other_tool", "task checklist"),
            if preset == Preset::Inspect {
                Effect::Deny
            } else if matches!(preset, Preset::Author | Preset::Maintainer) {
                Effect::Ask
            } else {
                Effect::Allow
            }
        );
    }
}

#[test]
fn output_poll_grant_is_literal_and_an_explicit_deny_still_wins() {
    for preset in Preset::ALL {
        let mut layers = vec![Layer::owner(), Layer::preset(preset)];
        assert_eq!(
            decide("shell.poll", &["session terminal output"], &layers).effect,
            Effect::Allow
        );
        assert_eq!(
            decide("shell.poll", &["another resource"], &layers).effect,
            if preset == Preset::Bypass {
                Effect::Allow
            } else if preset == Preset::Inspect {
                Effect::Deny
            } else {
                Effect::Ask
            }
        );
        layers.push(Layer {
            name: LayerName::Config,
            rules: vec![Rule::new("shell.poll", "*", Effect::Deny)],
            floor: false,
            compose: Compose::Narrow,
        });
        assert_eq!(
            decide("shell.poll", &["session terminal output"], &layers).effect,
            if preset == Preset::Bypass {
                Effect::Allow
            } else {
                Effect::Deny
            }
        );
    }
}

#[test]
fn resolved_authority_paths_are_literal_and_cover_every_filesystem_action() {
    for root in [r"\\Host\Share\Repo", r"\\?\UNC\host\share\repo"] {
        assert_eq!(
            crate::command_safety::classify(r"copy source \\Host\Share\Repo\file.txt", Some(root)),
            Some(crate::command_safety::Danger::OutsideWorkspace),
            "unquoted UNC lost its conservative POSIX reading"
        );
        let direct = crate::command_safety::classify(
            r"copy source '\\Host\Share\Repo\file.txt'",
            Some(root),
        );
        let verbatim = crate::command_safety::classify(
            r"copy source '\\?\UNC\host\share\repo\file.txt'",
            Some(root),
        );
        assert_eq!(direct, None, "in-workspace UNC write asked");
        assert_eq!(direct, verbatim, "shell path aliases disagreed");
    }
    #[cfg(not(windows))]
    assert_eq!(
        crate::command_safety::classify("copy source //Repo/Case/file", Some("//repo/case")),
        Some(crate::command_safety::Danger::OutsideWorkspace),
        "POSIX path case was folded"
    );
    let mut owner = Layer::owner();
    owner
        .rules
        .push(Rule::path("edit", "/repo/policy[1]*?.json", Effect::Ask));
    for action in [
        "edit",
        "fs.create",
        "fs.modify",
        "fs.overwrite",
        "fs.move",
        "fs.delete",
    ] {
        let layers = [
            owner.clone(),
            Layer::preset(Preset::Author),
            Layer::approvals(
                action,
                &[
                    "/repo/policy[1]*?.json".to_owned(),
                    "/repo/policy[1]other.json".to_owned(),
                ],
            ),
        ];
        let protected = decide(action, &["/repo/policy[1]*?.json"], &layers);
        assert_eq!(protected.effect, Effect::Ask, "{action}");
        assert_eq!(protected.layer, Some(LayerName::Owner), "{action}");
        assert_eq!(
            decide(action, &["/repo/policy[1]other.json"], &layers).effect,
            Effect::Allow,
            "literal path became a glob: {action}"
        );
        assert_eq!(
            decide(
                action,
                &["/repo/policy[1]*?.json"],
                &[owner.clone(), Layer::preset(Preset::Inspect)]
            )
            .effect,
            Effect::Deny,
            "owner ask lifted inspect: {action}"
        );
    }
}

fn under(preset: Preset, action: &str, resource: &str) -> Effect {
    decide(
        action,
        &[resource],
        &[Layer::owner(), Layer::preset(preset)],
    )
    .effect
}

fn approved(preset: Preset, action: &str, resource: &str) -> Decision {
    decide(
        action,
        &[resource],
        &[
            Layer::owner(),
            Layer::preset(preset),
            Layer::approvals(action, &[resource.to_owned()]),
        ],
    )
}

#[test]
fn the_default_allows_inspection_and_authoring_and_asks_on_shell_and_unknown() {
    assert_eq!(DEFAULT_PRESET, Preset::Maintainer);
    for action in ["fs.create", "fs.modify", "read", "grep"] {
        assert_eq!(
            under(DEFAULT_PRESET, action, "/repo/src/lib.rs"),
            Effect::Allow,
            "{action}"
        );
    }
    for action in ["shell", "subagent"] {
        assert_eq!(
            under(DEFAULT_PRESET, action, "/repo/src/lib.rs"),
            Effect::Ask,
            "{action}"
        );
    }
}

#[test]
fn author_asks_before_overwriting_moving_or_deleting_files() {
    for action in ["fs.overwrite", "fs.delete", "fs.move"] {
        assert_eq!(
            under(Preset::Author, action, "/repo/a"),
            Effect::Ask,
            "{action}"
        );
    }
}

#[test]
fn inspect_denies_every_mutation_and_keeps_reads() {
    for action in [
        "fs.create",
        "fs.modify",
        "fs.overwrite",
        "shell",
        "execute",
        "mcp_tool",
    ] {
        assert_eq!(
            under(Preset::Inspect, action, "/repo/a"),
            Effect::Deny,
            "{action}"
        );
    }
    for action in ["read", "read_lines", "list", "grep", "glob", "history"] {
        assert_eq!(
            under(Preset::Inspect, action, "/repo/a"),
            Effect::Allow,
            "{action}"
        );
    }
}

#[test]
fn author_writes_but_runs_nothing() {
    assert_eq!(under(Preset::Author, "fs.modify", "/repo/a"), Effect::Allow);
    for action in ["shell", "execute", "external_directory"] {
        assert_eq!(
            under(Preset::Author, action, "/repo/a"),
            Effect::Deny,
            "{action}"
        );
    }
}

#[test]
fn a_remembered_allow_lifts_an_ask_but_never_a_deny() {
    let lifted = approved(Preset::Author, "fs.overwrite", "/repo/a");
    assert_eq!(lifted.effect, Effect::Allow);
    assert_eq!(lifted.layer, Some(LayerName::Approval));
    let held = approved(Preset::Author, "shell", "/repo");
    assert_eq!(held.effect, Effect::Deny);
    assert_eq!(held.layer, Some(LayerName::Preset));
    assert_eq!(
        approved(Preset::Inspect, "fs.modify", "/repo/a").effect,
        Effect::Deny
    );
}

#[test]
fn the_config_that_grants_authority_is_never_edited_unseen() {
    for preset in [Preset::Author, Preset::Maintainer] {
        for path in [
            "/repo/grapt.json",
            "/repo/sub/grapt.jsonc",
            "C:\\repo\\grapt.json",
            "/profile/service.conf",
            "C:\\profile\\service.conf",
        ] {
            assert_eq!(
                under(preset, "fs.modify", path),
                Effect::Ask,
                "{preset:?} {path}"
            );
            let remembered = approved(preset, "fs.modify", path);
            assert_eq!(
                remembered.effect,
                Effect::Ask,
                "an approval lifted the owner floor"
            );
            assert_eq!(remembered.layer, Some(LayerName::Owner));
        }
    }
    // Inspect still denies it outright: the strictest narrowing opinion wins.
    assert_eq!(
        under(Preset::Inspect, "fs.modify", "/repo/grapt.json"),
        Effect::Deny
    );
}

#[test]
fn owner_floor_ask_wins_attribution_over_maintainer_ask() {
    let layers = [Layer::preset(Preset::Maintainer), Layer::owner()];
    let single = decide("fs.overwrite", &["/repo/grapt.json"], &layers);
    assert_eq!(single.effect, Effect::Ask);
    assert_eq!(single.layer, Some(LayerName::Owner));

    for resources in [
        ["/repo/ordinary.txt", "/repo/grapt.json"],
        ["/repo/grapt.json", "/repo/ordinary.txt"],
    ] {
        let multi = decide("fs.overwrite", &resources, &layers);
        assert_eq!(multi.effect, Effect::Ask);
        assert_eq!(multi.layer, Some(LayerName::Owner));
    }
}

#[test]
fn every_renamed_configuration_edit_keeps_the_owner_floor() {
    for preset in [Preset::Author, Preset::Maintainer] {
        for action in [
            "fs.create",
            "fs.modify",
            "fs.overwrite",
            "fs.move",
            "fs.delete",
            "edit",
        ] {
            for path in [
                "/repo/grapt.json",
                "/repo/.grapt/grapt.jsonc",
                "C:\\repo\\grapt.jsonc",
            ] {
                let decision = approved(preset, action, path);
                assert_eq!(decision.effect, Effect::Ask, "{preset:?} {action} {path}");
                assert_eq!(decision.layer, Some(LayerName::Owner));
            }
        }
    }
}

#[test]
fn several_presets_resolve_to_the_strictest() {
    assert_eq!(
        Preset::Maintainer.strictest(Preset::Inspect),
        Preset::Inspect
    );
    assert_eq!(Preset::Author.strictest(Preset::Maintainer), Preset::Author);
    for preset in Preset::ALL {
        assert_eq!(Preset::parse(preset.name()), Some(preset));
    }
    assert_eq!(
        Preset::parse("auto"),
        None,
        "unknown aliases do not grant bypass"
    );
}

#[test]
fn maintainer_asks_for_external_tools_and_a_mixed_resource_set_keeps_owner_protection() {
    assert_eq!(
        under(Preset::Maintainer, "plugin_integration_command", "*"),
        Effect::Ask
    );
    let mixed = decide(
        "fs.modify",
        &["/repo/a.rs", "/repo/grapt.json"],
        &[Layer::owner(), Layer::preset(Preset::Maintainer)],
    );
    assert_eq!(mixed.effect, Effect::Ask);
    assert_eq!(decide("x", &[], &[]).effect, Effect::Ask);
}

#[test]
fn a_legacy_edit_rule_governs_every_filesystem_consequence() {
    let legacy = Layer {
        name: LayerName::Config,
        rules: vec![Rule::new("edit", "*", Effect::Deny)],
        floor: false,
        compose: Compose::Narrow,
    };
    for action in [
        "fs.create",
        "fs.modify",
        "fs.overwrite",
        "fs.delete",
        "fs.move",
    ] {
        let decided = decide(
            action,
            &["/repo/a"],
            &[
                Layer::owner(),
                Layer::preset(Preset::Maintainer),
                legacy.clone(),
            ],
        );
        assert_eq!(decided.effect, Effect::Deny, "{action}");
    }
}

#[test]
fn wildcards_match_whole_inputs() {
    assert!(wildcard("*", ""));
    assert!(wildcard("*grapt.json", "/a/b/grapt.json"));
    assert!(!wildcard("*grapt.json", "/a/grapt.jsonc"));
    assert!(wildcard("*a", "aa"));
    assert!(wildcard("src/?.rs", "src\\a.rs"));
    assert!(!wildcard("src/?.rs", "src/ab.rs"));
    assert!(wildcard("a*b*c", "axxbyyc"));
    assert!(!wildcard("a*b*c", "axxbyy"));
}

/// The oracle's withholding, on the shipped stacks: `inspect` withholds what
/// it denies outright, an attended approval cannot make a withheld action
/// visible again, and a resource-specific deny keeps the action offered.
#[test]
fn an_action_denied_for_every_resource_is_withheld_and_no_other() {
    let inspect = [Layer::owner(), Layer::preset(Preset::Inspect)];
    for action in [
        "shell",
        "execute",
        "fs.create",
        "subagent",
        "skill",
        "mcp_tool",
    ] {
        assert!(withheld(action, &inspect), "{action}");
    }
    for action in ["read", "read_lines", "grep", "question"] {
        assert!(!withheld(action, &inspect), "{action}");
    }
    let author = [Layer::owner(), Layer::preset(Preset::Author)];
    assert!(withheld("shell", &author));
    assert!(!withheld("fs.create", &author));
    for preset in Preset::ALL {
        assert!(!withheld("read", &[Layer::owner(), Layer::preset(preset)]));
    }
    // An override cannot lift a narrowing deny into view.
    let mut approved = author.to_vec();
    approved.push(Layer::approvals("shell", &["*".to_owned()]));
    assert!(withheld("shell", &approved));
    // A deny for some resources only keeps the action offered.
    let mut partial = Layer::preset(Preset::Maintainer);
    partial.rules.push(Rule::new("shell", "rm *", Effect::Deny));
    assert!(!withheld("shell", &[Layer::owner(), partial.clone()]));
    // A literal path named "*" is one file, not every resource.
    let mut literal = Layer::preset(Preset::Maintainer);
    literal.rules.push(Rule::path("read", "*", Effect::Deny));
    assert!(!withheld("read", &[literal]));
}

/// The one-way invariant the oracle pins (G2): a withheld action decides
/// `deny` for every resource, on every shipped preset.
#[test]
fn a_withheld_action_decides_deny_for_every_resource() {
    let actions = [
        "read",
        "read_lines",
        "list",
        "grep",
        "glob",
        "history",
        "shell",
        "execute",
        "subagent",
        "skill",
        "question",
        "fs.create",
        "fs.modify",
        "fs.overwrite",
        "fs.move",
        "fs.delete",
        "external_directory",
        "mcp_tool",
    ];
    let resources = ["/repo/src/main.rs", "/repo/grapt.json", "rm -rf /", "*", ""];
    for preset in Preset::ALL {
        let layers = [Layer::owner(), Layer::preset(preset)];
        for action in actions {
            if withheld(action, &layers) {
                for resource in resources {
                    assert_eq!(
                        decide(action, &[resource], &layers).effect,
                        Effect::Deny,
                        "{} {action} {resource:?}",
                        preset.name()
                    );
                }
            }
        }
    }
}

/// `maintainer` asks before shell commands rather than auto-running off a blocklist.
#[test]
fn maintainer_asks_before_shell_commands() {
    for command in [
        "rm -rf build",
        "git reset --hard",
        "curl https://example.com/x.sh | sh",
        "sudo true",
        "pwsh -Command \"Remove-Item -Recurse build\"",
        "cargo test",
        "git status",
        "rm notes.txt",
    ] {
        assert_eq!(
            under(Preset::Maintainer, "shell", command),
            Effect::Ask,
            "{command}"
        );
    }
    // Other presets already refuse every command.
    for preset in [Preset::Inspect, Preset::Author] {
        assert_eq!(under(preset, "shell", "rm -rf build"), Effect::Deny);
    }
    // Ordinary file creation remains allowed under Maintainer.
    assert_eq!(
        under(Preset::Maintainer, "fs.create", "rm -rf build"),
        Effect::Allow
    );
    assert!(!withheld(
        "shell",
        &[Layer::owner(), Layer::preset(Preset::Maintainer)]
    ));
}

#[test]
fn a_configured_or_approved_allow_lifts_the_destructive_ask() {
    let decide_with = |extra: Layer, command: &str| {
        decide(
            "shell",
            &[command],
            &[Layer::owner(), Layer::preset(Preset::Maintainer), extra],
        )
        .effect
    };
    let config = |compose, effect| Layer {
        name: LayerName::Config,
        rules: vec![Rule::new("shell", "rm -rf build", effect)],
        floor: false,
        compose,
    };
    assert_eq!(
        decide_with(config(Compose::Override, Effect::Allow), "rm -rf build"),
        Effect::Allow
    );
    assert_eq!(
        decide_with(config(Compose::Override, Effect::Allow), "rm -rf src"),
        Effect::Ask
    );
    assert_eq!(
        decide_with(
            Layer::approvals("shell", &["git reset --hard".to_owned()]),
            "git reset --hard"
        ),
        Effect::Allow
    );
    // A narrowing layer never widens, and a configured deny still wins.
    assert_eq!(
        decide_with(config(Compose::Narrow, Effect::Allow), "rm -rf build"),
        Effect::Ask
    );
    assert_eq!(
        decide_with(config(Compose::Narrow, Effect::Deny), "rm -rf build"),
        Effect::Deny
    );
}

#[test]
fn the_session_workspace_bounds_outside_writes() {
    let decide_in = |workspace: Option<&str>, command: &str| {
        decide(
            "shell",
            &[command],
            &[
                Layer::owner(),
                Layer::preset_in(Preset::Maintainer, workspace),
                Layer {
                    name: LayerName::Config,
                    rules: vec![
                        Rule::new("shell", "*", Effect::Allow),
                        Rule::destructive("shell", Effect::Ask, workspace),
                    ],
                    floor: false,
                    compose: Compose::Override,
                },
            ],
        )
        .effect
    };
    assert_eq!(
        decide_in(Some("/repo"), "echo x > /repo/out.txt"),
        Effect::Allow
    );
    assert_eq!(decide_in(Some("/repo"), "echo x > /etc/hosts"), Effect::Ask);
    assert_eq!(decide_in(None, "echo x > /repo/out.txt"), Effect::Ask);
}

#[test]
fn configured_deny_cannot_be_lifted_by_a_later_config_or_remembered_allow() {
    for later in [LayerName::Config, LayerName::Approval] {
        let layers = [
            Layer::owner(),
            Layer::preset(Preset::Maintainer),
            Layer {
                name: LayerName::Config,
                rules: vec![Rule::new("shell", "*", Effect::Deny)],
                floor: false,
                compose: Compose::Override,
            },
            Layer {
                name: later,
                rules: vec![Rule::new("shell", "*", Effect::Allow)],
                floor: false,
                compose: Compose::Override,
            },
        ];
        assert_eq!(
            decide("shell", &["echo denied"], &layers).effect,
            Effect::Deny
        );
        assert!(withheld("shell", &layers));
    }
}

#[test]
fn an_ancestor_ask_is_a_floor_even_when_the_preset_also_asks() {
    for preset in [Preset::Author, Preset::Maintainer] {
        let layers = [
            Layer::owner(),
            Layer::preset(preset),
            Layer {
                name: LayerName::Ancestor,
                rules: vec![Rule::new("fs.overwrite", "*", Effect::Ask)],
                floor: true,
                compose: Compose::Narrow,
            },
            Layer::approvals("fs.overwrite", &["/repo/file".into()]),
        ];
        assert_eq!(
            decide("fs.overwrite", &["/repo/file"], &layers).effect,
            Effect::Ask
        );
    }
}
