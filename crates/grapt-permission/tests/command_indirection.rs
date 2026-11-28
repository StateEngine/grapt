//! Approval decisions for shell resources containing command indirection.
use grapt_permission::{
    command_safety::simple_commands,
    prefix::PrefixPattern,
    preset::{decide, Compose, Effect, Layer, LayerName, Preset, Rule},
};

fn decision(command: &str) -> Effect {
    let resource = format!("{command}\nworkdir: /repo\nshell: cmd\nlogin: false\nterminal: false");
    decide(
        "shell",
        &[&resource],
        &[
            Layer::preset_in(Preset::Maintainer, Some("/repo")),
            Layer {
                name: LayerName::Config,
                rules: vec![
                    Rule::new("shell", "*", Effect::Allow),
                    Rule::destructive("shell", Effect::Ask, Some("/repo")),
                ],
                floor: false,
                compose: Compose::Override,
            },
        ],
    )
    .effect
}

#[test]
fn windows_call_and_launch_indirection_require_approval() {
    for command in [
        "cmd /c call rd /s /q build",
        "cmd /c call call rd /s /q build",
        "cmd /c @rd /s /q build",
        "cmd /c @ rd /s /q build",
        "cmd /c start /b rd /s /q build",
        "cmd /c forfiles /c \"cmd /c rd /s /q @path\"",
        "cmd /c if exist build rd /s /q build",
        "cmd /c if 1 equ 1 rd /s /q build",
        "cmd /c rd/s/q build",
        "cmd /c rd= /s /q build",
        "cmd /c rd,/s,/q build",
        "cmd /c rd;/s;/q build",
        "cmd /c if exist\"build\" rd /s /q build",
        "cmd /c call:cleanup",
        "%COMSPEC% /c rd /s /q build",
        "$SHELL -c 'rm -rf build'",
        "cmd /c for %x in (build) do rd /s /q %x",
    ] {
        assert_eq!(decision(command), Effect::Ask, "{command}");
    }
}

#[test]
fn nested_shells_and_attached_cmd_switches_require_approval() {
    for command in [
        "cmd /c cmd /c call rd /s /q build",
        "cmd /d/crd /s /q build",
        "cmd /c\"rd /s /q build\"",
        "cmd /krd /s /q build",
        "bash -c 'cmd /c call rd /s /q build'",
        "pwsh -Command 'cmd /c call rd /s /q build'",
    ] {
        assert_eq!(decision(command), Effect::Ask, "{command}");
    }
}

#[test]
fn git_configuration_and_unresolved_commands_require_approval() {
    for command in [
        "git -c alias.x=\"reset --hard\" x",
        "git -calias.x=\"!rm -rf build\" x",
        "git --config-env=alias.x=COMMAND x",
        "git --config-env alias.x=COMMAND x",
        "git -C sub custom-command",
        "git submodule foreach 'git reset --hard'",
        "git bisect run rm -rf build",
        "git rebase --exec='rm -rf build' main",
        "git rebase -x'rm -rf build' main",
        "git rebase --exe='rm -rf build' main",
        "git rebase -ix'rm -rf build' main",
        "git --exec-path=tools custom-command",
        "env git -c alias.x=\"reset --hard\" x",
    ] {
        assert_eq!(decision(command), Effect::Ask, "{command}");
        assert!(!simple_commands(command).is_complete(), "{command}");
    }
}

#[test]
fn modeled_shell_and_git_commands_stay_prompt_free() {
    for command in [
        "cmd /d /c dir",
        "bash -c 'git status'",
        "git -C sub status",
        "[ -d build ]",
        "! git status",
    ] {
        assert_eq!(decision(command), Effect::Allow, "{command}");
    }
}

#[test]
fn standalone_echo_suppression_cannot_supply_a_different_prefix_grant() {
    let reading = simple_commands("@ git status");
    assert!(!reading.is_complete());
    assert!(reading
        .commands()
        .iter()
        .any(|words| words.first().is_some_and(|word| word == "@")));
}

#[test]
fn opaque_executables_preserve_literal_prefix_restrictions() -> Result<(), &'static str> {
    let pattern = PrefixPattern::new(vec![vec!["tool[1]".to_owned()]])?;
    let reading = simple_commands("tool[1] status");
    assert!(!reading.is_complete());
    assert!(reading
        .commands()
        .iter()
        .any(|command| pattern.matches(command)));
    Ok(())
}
