//! Profile-local navigation preferences, separate from engine session data.
use grapt_client::wire::SessionWireId;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    pinned: Vec<SessionWireId>,
}
#[derive(Default)]
pub struct Pins {
    root: Option<PathBuf>,
    ids: Vec<SessionWireId>,
}
impl Pins {
    pub fn load(root: &Path) -> Result<Self, String> {
        Self::read(root).map_err(|error| grapt_tui_transcript::markdown::literal(&error))
    }
    fn read(root: &Path) -> Result<Self, String> {
        let path = root.join("session-pins.json");
        let ids = match crate::config::read(&path, 65_536) {
            Ok(text) => {
                let record: Record = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid session pins: {e}"))?;
                if record.pinned.len() > 128
                    || record
                        .pinned
                        .iter()
                        .enumerate()
                        .any(|(i, id)| record.pinned[..i].contains(id))
                {
                    return Err("Session pins exceed their bound or contain duplicates".into());
                }
                record.pinned
            }
            Err(error) => match fs::symlink_metadata(&path) {
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                _ => return Err(format!("Cannot read session pins: {error}")),
            },
        };
        Ok(Self {
            root: Some(root.to_owned()),
            ids,
        })
    }
    pub fn contains(&self, id: &SessionWireId) -> bool {
        self.ids.contains(id)
    }
    pub fn position(&self, id: &SessionWireId) -> Option<usize> {
        self.ids.iter().position(|old| old == id)
    }
    pub fn slot(&self, slot: u8) -> Option<&SessionWireId> {
        if !(1..=9).contains(&slot) {
            return None;
        }
        self.ids.get(usize::from(slot - 1))
    }
    pub fn toggle(&mut self, id: SessionWireId) -> Result<(), String> {
        let mut ids = self.ids.clone();
        if let Some(index) = ids.iter().position(|old| old == &id) {
            ids.remove(index);
        } else if ids.len() < 128 {
            ids.push(id);
        } else {
            return Err("At most 128 sessions can be pinned".into());
        }
        self.save(ids)
    }
    pub fn remove(&mut self, id: &SessionWireId) -> Result<(), String> {
        if !self.contains(id) {
            return Ok(());
        }
        let ids = self.ids.iter().filter(|old| *old != id).cloned().collect();
        self.save(ids)
    }
    fn save(&mut self, ids: Vec<SessionWireId>) -> Result<(), String> {
        let root = self
            .root
            .as_ref()
            .ok_or("Session preferences are unavailable")?;
        let bytes = serde_json::to_vec(&Record {
            pinned: ids.clone(),
        })
        .map_err(|e| e.to_string())?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = root.join(format!(
            ".session-pins-{}-{}.tmp",
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
            fs::rename(&temporary, root.join("session-pins.json"))
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot save session pins: {error}"));
        }
        self.ids = ids;
        Ok(())
    }
}
