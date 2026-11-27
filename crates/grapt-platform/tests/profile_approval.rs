//! The production source-reader API must consume the approved view, not a
//! pathname checked earlier. This binary installs one process-lifetime view.
use grapt_config::{ConfigSourcePath, MaxDocumentBytes, MaxPathBytes, SourceIoError, SourceReader};
use grapt_platform::profile_configuration::{
    frozen, install, FrozenRead, ProfileApproval, ProfileConfiguration, ProfileSourceReader,
};

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn approved_sources_survive_replacement_but_changed_bootstrap_refuses(
) -> Result<(), Box<dyn std::error::Error>> {
    let temporary =
        std::env::temp_dir().join(format!("grapt-approved-sources-{}", std::process::id()));
    std::fs::create_dir(&temporary)?;
    let fixture = Fixture(std::fs::canonicalize(temporary)?);
    let json = fixture.0.join("grapt.jsonc");
    std::fs::write(&json, b"{\"permission\":\"ask\"}")?;
    std::fs::write(
        fixture.0.join("service.conf"),
        b"permission_preset=maintainer\n",
    )?;
    let approval = ProfileConfiguration::capture(&fixture.0)?
        .approval()
        .clone();
    // Use the exact JSON coordinates carried by serve-terminal, with strict decoding.
    let encoded = approval.encoded()?;
    let approval = ProfileApproval::decode(&encoded)?;
    std::fs::write(&json, b"{\"permission\":\"allow\"}")?;
    assert!(
        install(&fixture.0, &approval).is_err(),
        "a prior decision cannot authorize changed bytes"
    );
    std::fs::write(&json, b"{\"permission\":\"ask\"}")?;
    install(&fixture.0, &approval)?;
    std::fs::write(&json, b"{\"permission\":\"allow\"}")?;
    std::fs::write(fixture.0.join("grapt.json"), b"{\"plugin\":[\"foreign\"]}")?;
    std::fs::write(
        fixture.0.join("service.conf"),
        b"permission_preset=unrestricted\n",
    )?;
    let bound = MaxDocumentBytes::try_new(1024)?;
    let path_bound = MaxPathBytes::try_new(MaxPathBytes::CEILING)?;
    let source = ConfigSourcePath::new(&json, path_bound)?;
    assert_eq!(
        ProfileSourceReader.read_bounded(&source, bound)?,
        b"{\"permission\":\"ask\"}"
    );
    let absent = ConfigSourcePath::new(&fixture.0.join("grapt.json"), path_bound)?;
    assert!(matches!(
        ProfileSourceReader.probe(&absent),
        Err(SourceIoError::NotFound)
    ));
    assert!(matches!(
        ProfileSourceReader.read_bounded(&absent, bound),
        Err(SourceIoError::NotFound)
    ));
    match frozen(&fixture.0.join("service.conf"), 1024)? {
        FrozenRead::Bytes(actual) => assert_eq!(actual, b"permission_preset=maintainer\n"),
        other => panic!("approved source was not frozen: {other:?}"),
    }
    assert!(ProfileSourceReader
        .read_bounded(&source, MaxDocumentBytes::try_new(1)?)
        .is_err());
    // Ordinary unbound configuration retains the established reader contract.
    let other = fixture.0.join("instructions.txt");
    std::fs::write(&other, b"ordinary")?;
    assert_eq!(
        ProfileSourceReader.read_bounded(&ConfigSourcePath::new(&other, path_bound)?, bound)?,
        b"ordinary"
    );
    Ok(())
}
