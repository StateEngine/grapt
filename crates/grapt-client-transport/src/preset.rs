//! Preset vocabulary and wire data. Effective authority always comes from the server.

use crate::wire::PresetSnapshot;
pub use grapt_api::u3a::PermissionPresetDto as Preset;

pub const BYPASS_INDICATOR: &str = "Bypass permissions on";

pub fn from_flag(flag: &str) -> Option<Preset> {
    match flag {
        "--inspect" => Some(Preset::Inspect),
        "--author" => Some(Preset::Author),
        "--maintainer" => Some(Preset::Maintainer),
        "--dangerously-bypass-permissions" => Some(Preset::Bypass),
        _ => None,
    }
}

pub fn strictest(current: Option<Preset>, next: Preset) -> Preset {
    match (current, next) {
        (None, next) => next,
        (Some(Preset::Inspect), _) | (_, Preset::Inspect) => Preset::Inspect,
        (Some(Preset::Author), _) | (_, Preset::Author) => Preset::Author,
        (Some(Preset::Maintainer), _) | (_, Preset::Maintainer) => Preset::Maintainer,
        _ => Preset::Bypass,
    }
}

pub fn name(preset: Preset) -> &'static str {
    match preset {
        Preset::Inspect => "inspect",
        Preset::Author => "author",
        Preset::Maintainer => "maintainer",
        Preset::Bypass => "bypass",
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Phase {
    Apply(Preset),
    Read,
}

#[derive(Debug)]
pub struct Job {
    pub session: crate::wire::SessionWireId,
    pub generation: u64,
    pub phase: Phase,
    pub pending: bool,
}

#[derive(Debug, Default)]
pub struct State {
    pub launch: Option<Preset>,
    pub generation: u64,
    pub job: Option<Job>,
    pub snapshot: Option<PresetSnapshot>,
    pub error: Option<String>,
    pub applied: bool,
}
