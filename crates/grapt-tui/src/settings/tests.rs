use super::*;
use crate::keys::{handle_key, paste};
use std::{path::PathBuf, sync::mpsc};
struct Profile(PathBuf);
impl Profile {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "grapt-display-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn backend() -> crate::backend::Backend {
    let (tx, _commands) = mpsc::channel();
    let rx = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    crate::backend::Backend {
        commands: tx,
        updates: rx,
        wake,
    }
}
#[test]
fn settings_search_persists_actual_values_and_preserves_the_composer_on_failed_save(
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::new()?;
    let backend = backend();
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    ui.preferences = Preferences::load(&profile.0)?;
    ui.composer.insert_str("keep this draft");
    crate::actions::run(crate::actions::Action::Settings, &mut ui, &backend)?;
    paste("color", &mut ui);
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    handle_key(plain(KeyCode::Enter), &mut ui, &backend)?;
    assert!(ui.palette.light && Preferences::load(&profile.0)?.values.light);
    assert_eq!(
        ui.status, "Connected",
        "settings feedback is not service failure"
    );
    assert_eq!(
        crate::quit::key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut ui,
            &backend,
            std::time::Duration::ZERO,
        ),
        Some(false)
    );
    assert!(ui.settings.is_none());
    assert_eq!(crate::keys::draft(&ui), "keep this draft");
    let args = || {
        vec![
            "--service".into(),
            profile.0.to_string_lossy().into_owned(),
            "--directory".into(),
            ".".into(),
        ]
    };
    assert!(crate::config::Config::parse_args(args())?.light);
    let mut dark = args();
    dark.push("--dark".into());
    assert!(!crate::config::Config::parse_args(dark)?.light);
    for query in ["notifications", "copy", "wheel"] {
        crate::actions::run(crate::actions::Action::Settings, &mut ui, &backend)?;
        paste(query, &mut ui);
        handle_key(plain(KeyCode::Enter), &mut ui, &backend)?;
        handle_key(plain(KeyCode::Esc), &mut ui, &backend)?;
    }
    let saved = Preferences::load(&profile.0)?.values;
    assert!(!saved.notifications && !saved.copy_on_select && saved.wheel_rows == 4);
    assert_eq!(crate::keys::draft(&ui), "keep this draft");
    std::fs::rename(profile.0.join("display.json"), profile.0.join("saved.json"))?;
    std::fs::create_dir(profile.0.join("display.json"))?;
    assert!(Preferences::load(&profile.0).is_err());
    crate::actions::run(crate::actions::Action::Settings, &mut ui, &backend)?;
    paste("color", &mut ui);
    handle_key(plain(KeyCode::Enter), &mut ui, &backend)?;
    assert!(
        ui.palette.light && ui.preferences.values.light,
        "failed persistence must retain the active value"
    );
    assert_eq!(crate::keys::draft(&ui), "keep this draft");
    std::fs::remove_dir(profile.0.join("display.json"))?;
    std::fs::write(profile.0.join("display.json"), r#"{"wheel_rows":0}"#)?;
    assert!(Preferences::load(&profile.0).is_err());
    Ok(())
}

#[test]
fn unrelated_settings_edits_preserve_temporary_and_launch_theme_overrides(
) -> Result<(), Box<dyn std::error::Error>> {
    for launch_override in [false, true] {
        let profile = Profile::new()?;
        let backend = backend();
        let mut args = vec![
            "--service".into(),
            profile.0.to_string_lossy().into_owned(),
            "--directory".into(),
            ".".into(),
        ];
        if launch_override {
            args.extend(["--light".into(), "--no-notifications".into()]);
        }
        let config = crate::config::Config::parse_args(args)?;
        let mut ui = Ui::new(config.light);
        ui.preferences = Preferences::load(&profile.0)?;
        if !launch_override {
            handle_key(
                KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
                &mut ui,
                &backend,
            )?;
        }
        assert!(ui.palette.light);
        ui.composer.insert_str("keep override draft");
        crate::actions::run(crate::actions::Action::Settings, &mut ui, &backend)?;
        paste("wheel", &mut ui);
        handle_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        let saved = Preferences::load(&profile.0)?.values;
        assert!(!saved.light && saved.notifications && saved.wheel_rows == 4);
        assert!(
            ui.palette.light,
            "an unrelated edit must retain the active theme"
        );
        assert_eq!(crate::keys::draft(&ui), "keep override draft");
        let ordinary = crate::config::Config::parse_args(vec![
            "--service".into(),
            profile.0.to_string_lossy().into_owned(),
            "--directory".into(),
            ".".into(),
        ])?;
        assert!(!ordinary.light);
        if launch_override {
            assert!(!config.notifications && ordinary.notifications);
        }
        let mut notifier = crate::notify::Notifier::new(config.notifications);
        notifier.set_enabled(saved.notifications);
        notifier.set_enabled(false);
        notifier.set_enabled(saved.notifications);
        let mut output = Vec::new();
        notifier.emit_to(crate::notify::TURN_COMPLETE, &mut output)?;
        assert_eq!(output.is_empty(), launch_override);
    }
    Ok(())
}
