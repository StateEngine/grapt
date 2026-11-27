//! Exercise native aliases after tool preparation, using synthetic targets only.
use super::*;
use grapt_permission::preset::{Compose, Effect, Layer, LayerName, Preset, Rule};

fn aliases(target: &Path) -> Result<Vec<(&'static str, String)>, String> {
    let canonical = target.to_str().ok_or("synthetic target is not UTF-8")?;
    let ordinary = canonical
        .strip_prefix(r"\\?\")
        .ok_or("verbatim scratch root")?;
    let mut paths = vec![
        ("ordinary", ordinary.to_owned()),
        ("verbatim", canonical.to_owned()),
        ("forward-slashes", ordinary.replace('\\', "/")),
        ("mixed-case", ordinary.to_ascii_lowercase()),
        ("trailing-dot", format!("{ordinary}.")),
        ("trailing-space", format!("{ordinary} ")),
        ("default-stream", format!("{ordinary}::$DATA")),
    ];
    if let Some(short) = crate::dogfood::authority_path::short_alias(target)? {
        paths.push(("short-file", short.to_string_lossy().into_owned()));
    }
    let base = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
    let suffix = target
        .strip_prefix(&base)
        .map_err(|_| "synthetic target outside TEMP")?;
    for (variable, label) in [
        ("GRAPT_SEC_WIN_PREFIX_SHORT_BASE", "short-name"),
        ("GRAPT_SEC_WIN_PREFIX_UNC_BASE", "UNC"),
    ] {
        match std::env::var(variable) {
            Ok(base) => {
                let path = Path::new(&base).join(suffix).to_string_lossy().into_owned();
                if label == "UNC" {
                    let unc = path.strip_prefix(r"\\").ok_or("UNC fixture spelling")?;
                    paths.push(("verbatim-UNC", format!(r"\\?\UNC\{unc}")));
                }
                paths.push((label, path));
            }
            Err(_) => {
                eprintln!("SEC-WIN-PREFIX {label}: not measured; native alias fixture unavailable")
            }
        }
    }
    Ok(paths)
}

#[test]
#[cfg_attr(windows, expect(clippy::too_many_lines, reason = "legacy"))]
fn configured_denials_govern_native_windows_prepared_file_effects() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let outside = Scratch::new()?;
    let mut violations = Vec::new();
    for (filename, floor) in [
        ("SyntheticProtectedFile.txt", Effect::Allow),
        ("SeCode.JsOn", Effect::Ask),
        ("SeCode.JsOnC", Effect::Ask),
        ("SeRvIcE.CoNf", Effect::Ask),
        (".grapt/agents/synthetic.md", Effect::Deny),
        (".grapt/credentials/synthetic.json", Effect::Deny),
        (".stateengine/synthetic.json", Effect::Deny),
    ] {
        let target = scratch.0.join(filename);
        std::fs::create_dir_all(target.parent().ok_or("synthetic parent")?)
            .map_err(|e| e.to_string())?;
        std::fs::write(&target, "synthetic-before").map_err(|e| e.to_string())?;
        let canonical = target.to_string_lossy();
        let ordinary = canonical.strip_prefix(r"\\?\").ok_or("ordinary target")?;
        for (label, path) in aliases(&target)? {
            for tool in [EDIT_TOOL, WRITE_TOOL] {
                std::fs::write(&target, "synthetic-before").map_err(|e| e.to_string())?;
                let raw = if tool == EDIT_TOOL {
                    serde_json::json!({"path":path,"old_text":"synthetic-before","new_text":"synthetic-after"})
                } else {
                    serde_json::json!({"path":path,"content":"synthetic-after"})
                };
                let input = input(&raw.to_string())?;
                let prepared = prepare_effect(&scratch.0, tool, &input, &no_routes());
                let malformed =
                    matches!(label, "trailing-dot" | "trailing-space" | "default-stream");
                if malformed || floor == Effect::Deny {
                    let reason = prepared.err().ok_or_else(|| {
                        format!(
                            "protected or ambiguous target was prepared: {filename}/{label}/{tool}"
                        )
                    })?;
                    assert!(
                        reason.contains(if malformed {
                            "ambiguous Windows filename"
                        } else {
                            "protected control metadata"
                        }),
                        "unexpected preparation refusal: {filename}/{label}/{tool}: {reason}"
                    );
                    continue;
                }
                let prepared = prepared.map_err(|reason| {
                    format!(
                        "supported target was not prepared: {filename}/{label}/{tool}: {reason}"
                    )
                })?;
                let mut layers = vec![Layer::owner(), Layer::preset(Preset::Maintainer)];
                let before = grapt_permission::preset::decide(
                    &prepared.action,
                    &[&prepared.resource],
                    &layers,
                );
                assert_eq!(
                    before.effect, floor,
                    "owner floor: {filename}/{label}/{tool}"
                );
                layers.push(Layer {
                    name: LayerName::Config,
                    rules: vec![Rule::new("edit", ordinary, Effect::Deny)],
                    floor: false,
                    compose: Compose::Override,
                });
                let decision = grapt_permission::preset::decide(
                    &prepared.action,
                    &[&prepared.resource],
                    &layers,
                );
                eprintln!(
                    "SEC-WIN-PREFIX {filename} {label} {tool}: {:?}",
                    decision.effect
                );
                let repository = Layer {
                    name: LayerName::Config,
                    rules: vec![Rule::new("edit", ordinary, Effect::Deny)],
                    floor: true,
                    compose: Compose::Narrow,
                };
                let approval =
                    Layer::approvals(&prepared.action, std::slice::from_ref(&prepared.resource));
                assert_eq!(
                    grapt_permission::preset::decide(
                        &prepared.action,
                        &[&prepared.resource],
                        &[
                            Layer::owner(),
                            Layer::preset(Preset::Maintainer),
                            repository,
                            approval
                        ]
                    )
                    .effect,
                    Effect::Deny,
                    "repository narrowing: {filename}/{label}/{tool}"
                );
                let mut owner = Layer::owner();
                owner.rules.push(Rule::path("edit", ordinary, Effect::Ask));
                assert_eq!(
                    grapt_permission::preset::decide(
                        &prepared.action,
                        &[&prepared.resource],
                        &[owner, Layer::preset(Preset::Maintainer)]
                    )
                    .effect,
                    Effect::Ask,
                    "resolved owner path: {filename}/{label}/{tool}"
                );
                if decision.effect == Effect::Allow {
                    match prepared.target {
                        EffectTarget::Edit(path) => {
                            run_edit(&scratch.0, &path, &input)?;
                        }
                        EffectTarget::Coding(target) => {
                            coding::run(&scratch.0, &target, &input)?;
                        }
                        _ => return Err("unexpected prepared filesystem effect".into()),
                    }
                    assert_eq!(
                        std::fs::read(&target).map_err(|e| e.to_string())?,
                        b"synthetic-after"
                    );
                    violations.push(format!("{filename}/{label}/{tool}"));
                } else {
                    assert_eq!(decision.effect, Effect::Deny, "{filename}/{label}/{tool}");
                    assert_eq!(
                        std::fs::read(&target).map_err(|e| e.to_string())?,
                        b"synthetic-before"
                    );
                }
            }
        }
    }
    let escaped = outside.0.join("SyntheticOutside.txt");
    std::fs::write(&escaped, "synthetic-outside").map_err(|e| e.to_string())?;
    for (label, path) in aliases(&escaped)? {
        for tool in [EDIT_TOOL, WRITE_TOOL] {
            let raw = if tool == EDIT_TOOL {
                serde_json::json!({"path":path,"old_text":"synthetic-outside","new_text":"escaped"})
            } else {
                serde_json::json!({"path":path,"content":"escaped"})
            };
            assert!(
                prepare_effect(&scratch.0, tool, &input(&raw.to_string())?, &no_routes()).is_err(),
                "outside target was prepared: {label}/{tool}"
            );
        }
    }
    assert_eq!(
        std::fs::read(escaped).map_err(|e| e.to_string())?,
        b"synthetic-outside"
    );
    shell_denials()?;
    assert!(
        violations.is_empty(),
        "configured deny bypassed through prepared effects: {violations:?}"
    );
    Ok(())
}

#[cfg_attr(windows, expect(clippy::too_many_lines, reason = "legacy"))]
fn shell_denials() -> Result<(), String> {
    use crate::dogfood::catalog_config::{CatalogEnvironment, CatalogSources};
    use grapt_sandbox::{ContainedShellSpawner, Mxc, ShellRequest};
    let fixture = Scratch::new()?;
    let project = fixture.0.join("project");
    let service = fixture.0.join("service");
    for directory in [&project, &service] {
        std::fs::create_dir(directory).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        service.join("grapt.jsonc"),
        br#"{"sandbox":{"enabled":true,"network":true}}"#,
    )
    .map_err(|e| e.to_string())?;
    let targets = [
        project.join("grapt.jsonc"),
        project.join(".grapt/agents/synthetic.md"),
        project.join(".grapt/credentials/synthetic.json"),
        service.join("service.conf"),
    ];
    for target in &targets {
        std::fs::create_dir_all(target.parent().ok_or("shell target parent")?)
            .map_err(|e| e.to_string())?;
        std::fs::write(target, b"{}").map_err(|e| e.to_string())?;
    }
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&fixture.0))?;
    let plan = sandbox::Plan::read(&project, &sources)?
        .ok_or("synthetic sandbox policy was not prepared")?;
    let allowed = project.join("Allowed.txt");
    for target in targets {
        for (label, path) in aliases(&target)? {
            if allowed.exists() {
                std::fs::remove_file(&allowed).map_err(|e| e.to_string())?;
            }
            let command = format!(
                "echo permitted>Allowed.txt & echo synthetic-after>\"{path}\" & exit /b 23"
            );
            let raw = serde_json::json!({"cmd": command,"shell":"cmd","timeout_ms":10000});
            let prepared = terminal::prepare_exec(
                &project,
                &input(&raw.to_string())?,
                shell_environment::Environment::parent()?,
            )?;
            let EffectTarget::TerminalExec(spec) = prepared.target else {
                return Err("native shell was not prepared".into());
            };
            assert!(
                spec.cmd_shell,
                "native shell diagnostic must use cmd's actual framing"
            );
            let request = ShellRequest {
                command_line: format!(
                    "{} /D /S /C \"{}\"",
                    sandbox::windows::quote(terminal::launch_path(&spec.program)?.as_os_str())?,
                    spec.command
                ),
                working_directory: terminal::shell_cwd(&spec.cwd, spec.cmd_shell)?,
                environment: spec
                    .environment
                    .iter()
                    .map(|(name, value)| {
                        Ok(format!(
                            "{}={}",
                            name.to_str().ok_or("environment name")?,
                            value.to_str().ok_or("environment value")?
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
                timeout_ms: 10000,
                policy: plan.policy(),
            };
            let before = std::fs::read(&target).map_err(|e| e.to_string())?;
            let mut child = Mxc
                .spawn(&request)
                .map_err(|e| format!("native shell alias was not measured: {}", e.reason))?;
            assert!(
                matches!(child.enforcement(), RealizedEnforcement::PlatformSandbox {backend} if backend == "mxc")
            );
            assert_eq!(
                child.wait().map_err(|e| e.to_string())?,
                23,
                "native shell did not complete: {label}"
            );
            assert!(
                std::fs::read_to_string(&allowed)
                    .map_err(|e| e.to_string())?
                    .contains("permitted"),
                "native shell positive control did not run: {label}"
            );
            assert_eq!(
                std::fs::read(&target).map_err(|e| e.to_string())?,
                before,
                "native shell protected-write bypass: {label}/{}",
                target.display()
            );
            eprintln!(
                "SEC-WIN-PREFIX shell {label}: protected target unchanged: {}",
                target.display()
            );
        }
    }
    Ok(())
}
