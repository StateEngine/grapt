//! One tool effect is admitted only after every semantic consequence is allowed.
use super::*;

#[derive(Debug, Clone)]
pub struct PermissionConsequence {
    pub action: String,
    pub resource: String,
}

impl PermissionHttpState {
    /// Read-only prediction for scheduling approval hooks. This grants no tool
    /// authority: `begin_tool` must re-evaluate the final layers before dispatch.
    #[allow(clippy::too_many_arguments)]
    pub fn preview_tool(
        &self,
        store: &EventStore,
        session: &SessionId,
        layers: &[crate::preset::Layer],
        project: &ProjectId,
        action: &str,
        resources: &[String],
        consequences: Option<&[PermissionConsequence]>,
    ) -> Result<PermissionCreateEffect, PermissionHttpError> {
        self.evaluate_tool_consequences(
            store,
            session,
            layers,
            project,
            action,
            resources,
            Some(resources),
            None,
            consequences,
        )
        .map(|(effect, _)| effect)
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn evaluate_tool_consequences(
        &self,
        store: &EventStore,
        session: &SessionId,
        layers: &[crate::preset::Layer],
        project: &ProjectId,
        action: &str,
        resources: &[String],
        save: Option<&[String]>,
        metadata: Option<&Value>,
        consequences: Option<&[PermissionConsequence]>,
    ) -> Result<(PermissionCreateEffect, Value), PermissionHttpError> {
        let mut owner_floor = false;
        let mut removes_files = matches!(action, "fs.delete" | "fs.move");
        let effect = if let Some(consequences) = consequences {
            if resources.len() > MAX_HTTP_PERMISSION_RESOURCES
                || consequences.len() > MAX_HTTP_PERMISSION_RESOURCES
                || save.is_some_and(|patterns| patterns.len() > MAX_HTTP_PERMISSION_RESOURCES)
            {
                return Err(PermissionHttpError::ResourceBound {
                    max: MAX_HTTP_PERMISSION_RESOURCES,
                });
            }
            if action.is_empty() {
                return Err(PermissionError::InvalidAction {
                    action: action.to_owned(),
                }
                .into());
            }
            let mut expected: Vec<&str> = resources.iter().map(String::as_str).collect();
            let mut actual: Vec<&str> = consequences
                .iter()
                .map(|item| item.resource.as_str())
                .collect();
            expected.sort_unstable();
            actual.sort_unstable();
            if consequences.is_empty() || expected != actual {
                return Err(PermissionError::InvalidAction {
                    action: "tool consequences do not cover its resources".to_owned(),
                }
                .into());
            }
            // Deny before consulting remembered approvals, on every path.
            for consequence in consequences {
                let decision =
                    crate::preset::decide(&consequence.action, &[&consequence.resource], layers);
                if decision.effect == crate::preset::Effect::Deny {
                    return Err(PermissionHttpError::Denied {
                        action: consequence.action.clone(),
                        layer: decision
                            .layer
                            .map_or("permission", crate::preset::LayerName::label),
                    });
                }
                owner_floor |= decision.layer == Some(crate::preset::LayerName::Owner);
                removes_files |= matches!(consequence.action.as_str(), "fs.delete" | "fs.move");
            }
            // A prior whole-tool approval may cover ordinary files, but the
            // per-file deny and owner floor remain locked by the same decider.
            let mut remembered: Vec<String> = self
                .session_approvals
                .iter()
                .filter(|(held, held_action, _)| held == session && held_action == action)
                .flat_map(|(_, _, patterns)| patterns.iter().cloned())
                .collect();
            remembered.extend(saved_rule_patterns(store, project, action)?);
            let mut result = PermissionCreateEffect::Allow;
            for consequence in consequences {
                let mut stack = layers.to_vec();
                // Filesystem amendments authorize only that native resource.
                // The retried command still needs its ordinary authorization.
                if consequence.action == action
                    || (!crate::sandbox_amendment(action)
                        && !crate::sandbox_amendment(&consequence.action))
                {
                    stack.push(crate::preset::Layer::approvals(
                        &consequence.action,
                        &remembered,
                    ));
                }
                let effect = self.evaluate_create(
                    store,
                    session,
                    &stack,
                    project,
                    &consequence.action,
                    std::slice::from_ref(&consequence.resource),
                    save,
                )?;
                match effect {
                    PermissionCreateEffect::Deny => {
                        return Err(PermissionHttpError::Denied {
                            action: consequence.action.clone(),
                            layer: "permission",
                        })
                    }
                    PermissionCreateEffect::Ask => result = PermissionCreateEffect::Ask,
                    PermissionCreateEffect::Allow => {}
                }
            }
            result
        } else {
            let refs: Vec<&str> = resources.iter().map(String::as_str).collect();
            owner_floor = crate::preset::decide(action, &refs, layers).layer
                == Some(crate::preset::LayerName::Owner);
            self.evaluate_create(store, session, layers, project, action, resources, save)?
        };
        let consequence_metadata = consequences.map(|items| {
            items
                .iter()
                .map(|item| json!({"action": item.action, "resource": item.resource}))
                .collect::<Vec<_>>()
        });
        Ok((
            effect,
            json!({
                "tool": action,
                "policy": {"ownerFloor": owner_floor, "removesFiles": removes_files},
                "consequences": consequence_metadata,
                "request": metadata,
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{Effect, Layer, LayerName, Preset, Rule};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn item(action: &str, resource: &str) -> PermissionConsequence {
        PermissionConsequence {
            action: action.to_owned(),
            resource: resource.to_owned(),
        }
    }

    #[test]
    fn remembered_sandbox_paths_are_literal_and_do_not_authorize_commands() -> TestResult {
        let mut store = EventStore::open_in_memory()?;
        PermissionHttpState::initialize(&mut store)?;
        let session: SessionId = "ses_sandbox_scope".parse()?;
        let project = ProjectId::new("prj_sandbox_scope").ok_or("project")?;
        let action = crate::SANDBOX_WRITE_ACTION;
        let path = "/outside/literal*name";
        let state = PermissionHttpState {
            session_approvals: vec![(
                session.clone(),
                action.into(),
                vec![path.into(), "/outside/a\\b".into()],
            )],
        };
        let mut layers = vec![Layer::preset(Preset::Maintainer)];
        let evaluate = |checks: &[PermissionConsequence], layers: &[Layer]| {
            let resources = checks
                .iter()
                .map(|check| check.resource.clone())
                .collect::<Vec<_>>();
            state.preview_tool(
                &store,
                &session,
                layers,
                &project,
                action,
                &resources,
                Some(checks),
            )
        };
        assert_eq!(
            evaluate(&[item(action, path)], &layers)?,
            PermissionCreateEffect::Allow
        );
        assert_eq!(
            evaluate(&[item(action, "/outside/literal-other-name")], &layers)?,
            PermissionCreateEffect::Ask
        );
        assert_eq!(
            evaluate(&[item(action, "/outside/a/b")], &layers)?,
            PermissionCreateEffect::Ask
        );
        layers.push(Layer {
            name: LayerName::Config,
            rules: vec![Rule::new("shell", "*", Effect::Ask)],
            floor: false,
            compose: crate::preset::Compose::Narrow,
        });
        // The command deliberately equals the remembered path, so even an
        // exact resource match cannot turn file consent into command consent.
        let compound = [item(action, path), item("shell", path)];
        assert_eq!(evaluate(&compound, &layers)?, PermissionCreateEffect::Ask);
        layers.push(Layer::approvals("shell", &[path.into()]));
        assert_eq!(evaluate(&compound, &layers)?, PermissionCreateEffect::Allow);
        layers.push(Layer {
            name: LayerName::Config,
            rules: vec![Rule::new(action, "*", Effect::Deny)],
            floor: true,
            compose: crate::preset::Compose::Narrow,
        });
        assert!(evaluate(&compound, &layers).is_err());
        let outer_shell = PermissionHttpState {
            session_approvals: vec![(session.clone(), "shell".into(), vec![path.into()])],
        };
        assert_eq!(
            outer_shell.preview_tool(
                &store,
                &session,
                &[Layer::preset(Preset::Maintainer)],
                &project,
                "shell",
                &[path.into()],
                Some(&[item(action, path)])
            )?,
            PermissionCreateEffect::Ask
        );
        Ok(())
    }

    #[test]
    fn a_tool_ask_row_names_the_tool_its_fs_verbs_only_name_the_effect() -> TestResult {
        let mut store = EventStore::open_in_memory()?;
        PermissionHttpState::initialize(&mut store)?;
        let session: SessionId = "ses_tool_name".parse()?;
        let project = ProjectId::new("prj_tool_name").ok_or("project")?;
        let state = PermissionHttpState::default();
        let layers = [Layer::preset(Preset::Author)];
        let resources = ["/repo/ordinary.txt".to_owned()];
        let consequences = [item("fs.modify", "/repo/ordinary.txt")];
        let (effect, metadata) = state.evaluate_tool_consequences(
            &store,
            &session,
            &layers,
            &project,
            "patch",
            &resources,
            None,
            Some(&serde_json::json!({"note": "the caller's metadata"})),
            Some(&consequences),
        )?;
        assert_eq!(effect, PermissionCreateEffect::Allow);
        assert_eq!(
            metadata["tool"], "patch",
            "the row must name the tool that asked, not only its fs verbs"
        );
        assert_eq!(metadata["consequences"][0]["action"], "fs.modify");
        assert_eq!(
            metadata["consequences"][0]["resource"],
            "/repo/ordinary.txt"
        );
        assert_eq!(metadata["request"]["note"], "the caller's metadata");
        Ok(())
    }

    #[test]
    fn compound_policy_checks_each_path_and_preserves_owner_and_removal_classification(
    ) -> TestResult {
        let mut store = EventStore::open_in_memory()?;
        PermissionHttpState::initialize(&mut store)?;
        let session: SessionId = "ses_consequences".parse()?;
        let project = ProjectId::new("prj_consequences").ok_or("project")?;
        let state = PermissionHttpState::default();
        let mut owner = Layer::owner();
        owner
            .rules
            .push(Rule::path("edit", "/repo/policy.json", Effect::Ask));
        let layers = [
            owner,
            Layer::preset(Preset::Author),
            Layer::approvals(
                "fs.modify",
                &["/repo/ordinary.txt".into(), "/repo/policy.json".into()],
            ),
        ];
        let resources = [
            "/repo/ordinary.txt".to_owned(),
            "/repo/policy.json".to_owned(),
        ];
        let consequences = [
            item("fs.modify", &resources[0]),
            item("fs.modify", &resources[1]),
        ];
        let (effect, metadata) = state.evaluate_tool_consequences(
            &store,
            &session,
            &layers,
            &project,
            "patch",
            &resources,
            None,
            None,
            Some(&consequences),
        )?;
        assert_eq!(
            effect,
            PermissionCreateEffect::Ask,
            "one authority file did not hold the whole patch"
        );
        assert_eq!(metadata["policy"]["ownerFloor"], true);
        assert_eq!(metadata["policy"]["removesFiles"], false);
        let ordinary = [item("fs.modify", "/repo/ordinary.txt")];
        let (effect, _) = state.evaluate_tool_consequences(
            &store,
            &session,
            &layers,
            &project,
            "patch",
            &resources[..1],
            None,
            None,
            Some(&ordinary),
        )?;
        assert_eq!(
            effect,
            PermissionCreateEffect::Allow,
            "ordinary edit acquired an owner floor"
        );
        let removal = [item("fs.delete", "/repo/ordinary.txt")];
        let (effect, metadata) = state.evaluate_tool_consequences(
            &store,
            &session,
            &layers,
            &project,
            "patch",
            &resources[..1],
            None,
            None,
            Some(&removal),
        )?;
        assert_eq!(effect, PermissionCreateEffect::Ask);
        assert_eq!(metadata["policy"]["removesFiles"], true);
        Ok(())
    }

    #[test]
    fn one_denied_consequence_refuses_the_whole_tool_before_approval_reads() -> TestResult {
        // Deliberately remove approval storage: a deny must not query it.
        let store = EventStore::open_in_memory()?;
        let state = PermissionHttpState::default();
        store
            .connection()
            .execute("DROP TABLE permission_http_rule", [])?;
        let session: SessionId = "ses_consequence_deny".parse()?;
        let project = ProjectId::new("prj_consequence_deny").ok_or("project")?;
        let mut policy = Layer::preset(Preset::Maintainer);
        policy
            .rules
            .push(Rule::path("fs.modify", "/repo/blocked.txt", Effect::Deny));
        let resources = [
            "/repo/ordinary.txt".to_owned(),
            "/repo/blocked.txt".to_owned(),
        ];
        let consequences = [
            item("fs.modify", &resources[0]),
            item("fs.modify", &resources[1]),
        ];
        let result = state.evaluate_tool_consequences(
            &store,
            &session,
            &[policy],
            &project,
            "patch",
            &resources,
            None,
            None,
            Some(&consequences),
        );
        assert!(
            matches!(result, Err(PermissionHttpError::Denied { layer, .. }) if layer == LayerName::Preset.label()),
            "denied member did not refuse before approval lookup: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn missing_extra_empty_or_over_bound_consequences_cannot_authorize_a_tool() -> TestResult {
        let store = EventStore::open_in_memory()?;
        let state = PermissionHttpState::default();
        let session: SessionId = "ses_consequence_coverage".parse()?;
        let project = ProjectId::new("prj_consequence_coverage").ok_or("project")?;
        let resources = ["/repo/a".to_owned(), "/repo/b".to_owned()];
        for consequences in [
            vec![],
            vec![item("fs.modify", "/repo/a")],
            vec![item("fs.modify", "/repo/a"), item("fs.modify", "/repo/c")],
            vec![item("fs.modify", "/repo/a"); MAX_HTTP_PERMISSION_RESOURCES + 1],
        ] {
            let result = state.evaluate_tool_consequences(
                &store,
                &session,
                &[],
                &project,
                "patch",
                &resources,
                None,
                None,
                Some(&consequences),
            );
            assert!(
                matches!(
                    result,
                    Err(PermissionHttpError::ResourceBound { .. })
                        | Err(PermissionHttpError::Domain(
                            PermissionError::InvalidAction { .. }
                        ))
                ),
                "incomplete or excessive consequences reached approval lookup: {result:?}"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod msys_tests {
    use super::*;
    use crate::preset::{Effect, Layer, Preset, Rule};

    #[test]
    fn removing_containment_needs_its_own_literal_recorded_grant(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = EventStore::open_in_memory()?;
        PermissionHttpState::initialize(&mut store)?;
        let session: SessionId = "ses_msys_scope".parse()?;
        let project = ProjectId::new("prj_msys_scope").ok_or("project")?;
        let action = crate::SANDBOX_MSYS_ACTION;
        let executable = "C:/Git/usr/bin/literal*ssh.exe";
        let mut state = PermissionHttpState::default();
        let check = |state: &PermissionHttpState, name: &str, layers: &[Layer]| {
            state.preview_tool(
                &store,
                &session,
                layers,
                &project,
                action,
                &[name.into()],
                None,
            )
        };
        let bypass = [Layer::preset(Preset::Bypass)];
        assert_eq!(
            check(&state, executable, &bypass)?,
            PermissionCreateEffect::Ask
        );
        let mut layers = vec![
            Layer::preset(Preset::Maintainer),
            Layer::approvals("shell", &[executable.into()]),
        ];
        assert_eq!(
            check(&state, executable, &layers)?,
            PermissionCreateEffect::Ask
        );
        state
            .session_approvals
            .push((session.clone(), action.into(), vec![executable.into()]));
        assert_eq!(
            check(&state, executable, &layers)?,
            PermissionCreateEffect::Allow
        );
        assert_eq!(
            check(&state, "C:/Git/usr/bin/literal-other-ssh.exe", &layers)?,
            PermissionCreateEffect::Ask
        );
        // The earlier command approval proved it could not grant MSYS access.
        // Remove it before checking the converse: MSYS consent cannot grant a command.
        layers.truncate(1);
        layers.push(Layer {
            name: crate::preset::LayerName::Config,
            rules: vec![Rule::new("shell", "*", Effect::Ask)],
            floor: false,
            compose: crate::preset::Compose::Narrow,
        });
        let consequences = [
            PermissionConsequence {
                action: action.into(),
                resource: executable.into(),
            },
            PermissionConsequence {
                action: "shell".into(),
                resource: executable.into(),
            },
        ];
        assert_eq!(
            state.preview_tool(
                &store,
                &session,
                &layers,
                &project,
                action,
                &[executable.into(), executable.into()],
                Some(&consequences)
            )?,
            PermissionCreateEffect::Ask
        );
        layers.push(Layer::approvals("shell", &[executable.into()]));
        assert_eq!(
            state.preview_tool(
                &store,
                &session,
                &layers,
                &project,
                action,
                &[executable.into(), executable.into()],
                Some(&consequences)
            )?,
            PermissionCreateEffect::Allow
        );
        layers.push(Layer {
            name: crate::preset::LayerName::Config,
            rules: vec![Rule::new(action, "*", Effect::Deny)],
            floor: false,
            compose: crate::preset::Compose::Narrow,
        });
        assert_eq!(
            check(&state, executable, &layers)?,
            PermissionCreateEffect::Deny
        );
        Ok(())
    }
}
