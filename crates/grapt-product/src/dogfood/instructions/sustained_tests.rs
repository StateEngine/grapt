//! SUSTAINED-WORK-REPAIR-01 J01: instruction refresh visibility as accepted
//! base `20909c94` actually behaves. A reproduction receipt, replaced by J05.

use super::*;

#[test]
fn swr_t4_an_unreadable_ambient_instruction_source_keeps_a_safe_reason() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-swr-instructions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("project")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(root.join("config")).map_err(|e| e.to_string())?;
    std::fs::write(
        root.join("project/AGENTS.md"),
        vec![b'a'; MAX_FILE_BYTES + 1],
    )
    .map_err(|e| e.to_string())?;
    let roots = InstructionRoots {
        agents: String::new(),
        directory: root.join("project"),
        project: root.join("project"),
        config: root.join("config"),
        skills: Ok(Vec::new()),
        skill_warnings: Vec::new(),
    };
    let observation = roots.ambient()?;
    let SourceObservation::Unavailable { key } = observation.source else {
        return Err("an oversized source was not unavailable".to_owned());
    };
    assert_eq!(key.as_str(), "core/instructions");
    assert!(observation
        .failure
        .is_some_and(|reason| reason.contains("bounded regular file") && reason.len() <= 512));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
