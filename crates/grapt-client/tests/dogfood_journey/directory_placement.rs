//! Requested working directories survive project discovery and real creation.
use super::*;

#[test]
fn directory_launch_keeps_non_git_and_nested_git_session_placement(
) -> Result<(), Box<dyn std::error::Error>> {
    let (product, address) = serve("directory-placement");
    fs::remove_dir_all(product.launch.join(".git"))?;
    for repository in [false, true] {
        let directory = if repository {
            let status = Command::new("git")
                .args(["init", "--quiet"])
                .arg(&product.launch)
                .status()?;
            assert!(status.success());
            let nested = product.launch.join("nested");
            fs::create_dir(&nested)?;
            nested
        } else {
            product.launch.clone()
        };
        let directory = fs::canonicalize(directory)?;
        let directory = directory.to_str().ok_or("fixture directory is not UTF-8")?;
        let mut client = connect(address, Bounds::PRODUCTION);
        client.offer_directory(directory)?;
        client.begin_directory_draft()?;
        let agent = client.agents().first().ok_or("agent absent")?.id.clone();
        client.select_agent(&agent)?;
        let model = client
            .models()
            .iter()
            .find(|model| model.enabled)
            .ok_or("model absent")?
            .clone();
        client.select_model(&model)?;
        let session = client.create_session()?;
        let mut http = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
        let request = WireRequest::new(Operation::SessionGet, Some(&session), None)?;
        let saved = grapt_client::wire::decode_session_attachment(&http.call(&request)?.body)?;
        assert_eq!(
            fs::canonicalize(&saved.project.worktree)?,
            fs::canonicalize(directory)?,
            "project discovery replaced the requested cwd"
        );
        if !repository {
            assert_ne!(saved.project.id.spelled(), "global");
            let mut reopened = connect(address, Bounds::PRODUCTION);
            reopened.offer_directory(directory)?;
            reopened.attach_launch_session(&session)?;
        }
    }
    Ok(())
}
