//! A fresh profile enters the native TUI before asking for provider credentials.
//! Initialization and credential storage remain owned by the engine.
use std::{io::IsTerminal, path::Path};

pub(super) fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub(super) async fn create_profile(product: &Path, profile: &Path) -> Result<(), String> {
    super::create_profile_parent(profile)?;
    super::initialize_profile(product, profile, None).await
}
