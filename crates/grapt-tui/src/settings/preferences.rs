// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// . Modified for bounded native JSON
// preferences and commit-after-persist state, separate from engine settings.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Values {
    pub light: bool,
    pub notifications: bool,
    pub copy_on_select: bool,
    pub wheel_rows: u8,
}
impl Default for Values {
    fn default() -> Self {
        Self {
            light: false,
            notifications: true,
            copy_on_select: true,
            wheel_rows: 3,
        }
    }
}
#[derive(Default)]
pub struct Preferences {
    root: Option<PathBuf>,
    pub values: Values,
}
impl Preferences {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("display.json");
        let values = match crate::config::read(&path, 4096) {
            Ok(text) => serde_json::from_str::<Values>(&text)
                .map_err(|e| format!("Invalid display settings: {e}"))?,
            Err(error) => match fs::symlink_metadata(&path) {
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Values::default(),
                _ => return Err(format!("Cannot read display settings: {error}")),
            },
        };
        if !(1..=12).contains(&values.wheel_rows) {
            return Err("Mouse wheel speed must be between 1 and 12 rows".into());
        }
        Ok(Self {
            root: Some(root.to_owned()),
            values,
        })
    }
    pub fn save(&mut self, values: Values) -> Result<(), String> {
        let root = self
            .root
            .as_ref()
            .ok_or("Display preferences are unavailable")?;
        let bytes = serde_json::to_vec_pretty(&values).map_err(|e| e.to_string())?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = root.join(format!(
            ".display-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, root.join("display.json"))
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot save display settings: {error}"));
        }
        self.values = values;
        Ok(())
    }
}
