//! The gateway offer, split from the carrier to keep it under the module
//! ceiling: the repository a journey is launched in, and the walk through
//! the real product's project, agent and model selection.

use super::*;

/// The repository `root`'s journeys are launched in: beside the service root,
/// never inside it, as `grapt code` opens a user's repository. The product
/// lists no project for its own service root, so a journey that offered none
/// would find an empty gateway, exactly as a real first start would without
/// its repository.
pub(super) fn launch_repository(root: &Path) -> PathBuf {
    let launch = PathBuf::from(format!("{}-launch", root.display()));
    if launch.exists() {
        fs::remove_dir_all(&launch)
            .unwrap_or_else(|error| panic!("{} reset failed: {error}", launch.display()));
    }
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(&launch)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("fixture launch repository failed: {error}"));
    assert!(
        initialized.success(),
        "fixture launch repository was refused"
    );
    launch
}

/// Walk the real product's project gateway, agent roster and model picker,
/// after offering `product`'s launch repository as `grapt code` does.
///
/// Returns what was chosen, so a witness can assert on the REAL server's rows
/// rather than on rows this file invented. Every refusal panics naming the
/// step: a selection that silently fell back to a default would let the
/// journey below create a session against the server's own choice and then
/// report it as the user's.
pub(super) fn select_offer<C: Connection>(
    client: &mut Client<C>,
    product: &Product,
) -> (String, String, String) {
    let launch = product
        .launch
        .to_str()
        .unwrap_or_else(|| panic!("the launch repository must be a UTF-8 path"));
    client
        .offer_directory(launch)
        .unwrap_or_else(|error| panic!("the launch repository must resolve: {error}"));
    client
        .refresh_projects()
        .unwrap_or_else(|error| panic!("the real project gateway must answer: {error}"));
    let project = client
        .projects()
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("the real product must offer at least one project"));
    client
        .select_project(&project)
        .unwrap_or_else(|error| panic!("selecting a real project must succeed: {error}"));

    let agent = client
        .agents()
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("the real product must offer at least one visible agent"));
    client
        .select_agent(&agent.id)
        .unwrap_or_else(|error| panic!("selecting a real agent must succeed: {error}"));

    let model = client
        .models()
        .iter()
        .find(|row| row.enabled)
        .cloned()
        .unwrap_or_else(|| panic!("the real product must offer at least one enabled model"));
    client
        .select_model(&model)
        .unwrap_or_else(|error| panic!("selecting a real model must succeed: {error}"));

    (
        project.worktree.clone(),
        agent.id.spelled().to_owned(),
        model.id.spelled().to_owned(),
    )
}
