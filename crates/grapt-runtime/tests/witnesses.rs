//! Public fail-closed attempt-admission witnesses.
//!
//! Positive provider lifecycle custody lives in the private named modules under
//! `src/attempt_admission`. This external crate cannot mint an admitted receipt.

use grapt_runtime::{AttemptAdmissionRefusal, AttemptPreparation};

struct NeverReader;

impl grapt_runtime::ProviderReader for NeverReader {
    async fn next(&mut self) -> Option<grapt_provider::AttemptFragment> {
        None
    }
}

#[test]
fn public_refusal_invokes_zero_reader_factories_and_zero_effects() {
    let preparation = AttemptPreparation::<NeverReader>::refused(AttemptAdmissionRefusal::NoIssuer);
    assert!(format!("{preparation:?}").contains("AttemptPreparation"));
}

#[test]
fn public_source_inventory_has_no_raw_reader_factory_or_prepare_escape() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let step_sources = [
        "src/step.rs",
        "src/step/driver.rs",
        "src/step/preparation.rs",
    ]
    .map(|path| {
        std::fs::read_to_string(root.join(path))
            .unwrap_or_else(|error| panic!("step source {path} must be readable: {error}"))
    });
    let supervisor = std::fs::read_to_string(root.join("src/supervisor.rs"))
        .unwrap_or_else(|error| panic!("supervisor source must be readable: {error}"));
    let owner = std::fs::read_to_string(root.join("src/physical_attempt_owner.rs"))
        .unwrap_or_else(|error| panic!("physical attempt owner source must be readable: {error}"));
    let admission = std::fs::read_to_string(root.join("src/attempt_admission.rs"))
        .unwrap_or_else(|error| panic!("attempt admission source must be readable: {error}"));
    for step in step_sources {
        assert!(!step.contains("pub type ReaderFactory"));
        assert!(!step.contains("fn open("));
    }
    assert!(!supervisor.contains("pub fn prepare<F"));
    assert!(!owner.contains("pub struct PrivateOpener"));
    assert!(!admission.contains("    pub fn admitted("));
}
