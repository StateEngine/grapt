//! Active-version selection while shared exclusion is held.

use grapt_service::installation::{prefix_of_binary, PrefixGuard};
use std::path::PathBuf;

use super::{inventory, io, transition};

#[derive(Debug)]
pub struct Start {
    pub guard: Option<PrefixGuard>,
    pub redirect: Option<PathBuf>,
}

pub fn prepare() -> Result<Start, String> {
    let binary = std::env::current_exe()
        .map_err(|error| format!("installation binary cannot be located: {:?}", error.kind()))?;
    let Some(prefix) = prefix_of_binary(&binary).map_err(|error| error.to_string())? else {
        return Ok(Start {
            guard: None,
            redirect: None,
        });
    };
    let guard = PrefixGuard::shared(&prefix).map_err(|error| error.to_string())?;
    let current = transition::pointer(&prefix)?.ok_or("installation has no active version")?;
    // The active version's own inventory names its CLI, so a retained
    // pre-rename version stays startable after a revert.
    let bin = prefix.join("versions").join(current).join("bin");
    let cli = inventory::installed(&bin)?.cli();
    let active = bin.join(cli);
    io::regular(&active)?;
    // A compatibility alias is a verified copy of the active CLI in the same
    // version. Running it is running the active build: no second process, so
    // arguments, streams, exit status and signals stay its own.
    let alias = inventory::alias_of(cli).map(|alias| bin.join(alias));
    let redirect = (binary != active && alias.as_ref() != Some(&binary)).then_some(active);
    Ok(Start {
        guard: Some(guard),
        redirect,
    })
}
