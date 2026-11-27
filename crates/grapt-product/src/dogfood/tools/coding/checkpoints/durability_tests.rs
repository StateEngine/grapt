//! Preview must recover interrupted journal writes without replaying uncertain edits.
use super::*;
use crate::dogfood::tools::coding::checkpoints::manifest::{capture_files, save, Manifest};
use grapt_store::{OsRandomIds, SystemClock};

type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Fixture {
    project: PathBuf,
    journal: PathBuf,
    workspace: Workspace,
    selected: UndoSelection,
    _root: Root,
}
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let ids = Arc::new(OsRandomIds::seed()?);
    let root = std::env::temp_dir().join(format!("undo-durability-{}", ids.mint().as_str()));
    let service = root.join("profile");
    let project = root.join("project");
    std::fs::create_dir_all(&service)?;
    std::fs::create_dir(&project)?;
    let project = std::fs::canonicalize(project)?;
    let artifacts = Arc::new(FilesystemArtifactStore::open(
        service.join("artifacts"),
        vec![7; 32],
        Box::new(SystemClock),
    )?);
    let input: MessageId = "msg_undo_durability".parse()?;
    let paths = [PathBuf::from("a.txt")];
    std::fs::write(project.join(&paths[0]), b"before")?;
    let before = capture_files(&artifacts, project.clone(), input.clone(), &paths)?;
    std::fs::write(project.join(&paths[0]), b"after")?;
    let after = capture_files(&artifacts, project.clone(), input.clone(), &paths)?;
    let observation = grapt_checkpoint::VcsObserver::observe(
        &mut Observer {
            root: project.clone(),
        },
        &PathIdentity::canonical(project.to_string_lossy()),
    );
    let target = save(
        &artifacts,
        &Manifest {
            root: project.clone(),
            input: input.clone(),
            origin: Some(super::super::origin::Origin::observation(&observation)),
            files: before.files,
            expected: Some(after.files),
        },
    )?;
    let selected = UndoSelection {
        input,
        target,
        paths: paths.to_vec(),
        incomplete_patches: 0,
    };
    let workspace = Workspace::open(&service, &project, artifacts, Arc::new(SystemClock), ids)?;
    assert!(
        workspace.preview(&selected).is_ok(),
        "fixture has no initial undo preview"
    );
    let operation = format!("undo_{}", selected.target.as_str());
    let journal = service
        .join("restore-journal")
        .join(manifest::hash(
            project.to_str().ok_or("UTF-8 project")?.as_bytes(),
        ))
        .join(format!("{}.json", manifest::hash(operation.as_bytes())));
    Ok(Fixture {
        project,
        journal,
        workspace,
        selected,
        _root: Root(root),
    })
}

#[test]
fn torn_initial_undo_journal_preserves_evidence_and_allows_safe_preview() -> TestResult {
    let fixture = fixture()?;
    let damaged = br#"{"kind":"applying","schema":1,"operation":"#;
    std::fs::write(&fixture.journal, damaged)?;
    let result = fixture.workspace.preview(&fixture.selected);
    assert!(
        result.is_ok(),
        "torn initial record permanently disabled undo: {result:?}"
    );
    let mut preserved = false;
    for entry in std::fs::read_dir(fixture.journal.parent().ok_or("journal directory")?)? {
        let path = entry?.path();
        if path.is_file() && std::fs::read(&path)? == damaged {
            preserved = true;
        }
    }
    assert!(preserved, "repair discarded the interrupted write evidence");
    assert_eq!(std::fs::read(fixture.project.join("a.txt"))?, b"after");
    Ok(())
}

#[test]
fn torn_initial_undo_journal_cannot_authorize_preview_over_later_edits() -> TestResult {
    let fixture = fixture()?;
    std::fs::write(
        &fixture.journal,
        br#"{"kind":"applying","schema":1,"operation":"#,
    )?;
    std::fs::write(fixture.project.join("a.txt"), b"human edit")?;
    assert!(
        fixture.workspace.preview(&fixture.selected).is_err(),
        "damaged journal bypassed file preconditions"
    );
    assert_eq!(std::fs::read(fixture.project.join("a.txt"))?, b"human edit");
    Ok(())
}

fn applying(fixture: &Fixture) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::json!({
        "kind":"applying", "schema":1,
        "operation":format!("undo_{}", fixture.selected.target.as_str()),
        "plan":"restore-plan-fixture", "provider":"native-patch-artifacts", "version":"1",
        "checkpoint":fixture.selected.target.as_str(), "root_kind":"plain",
        "root":fixture.project.to_str().ok_or("UTF-8 project")?, "common_dir":null, "git_dir":null,
        "lanes":["patch-files"], "permission":"per_undo_fixture", "admitted_at_ms":1,
        "writes":1, "deletes":0, "bytes":6
    }))
}

#[test]
fn torn_terminal_undo_journal_keeps_the_admitted_operation_quarantined() -> TestResult {
    let fixture = fixture()?;
    let mut damaged = serde_json::to_vec(&applying(&fixture)?)?;
    damaged.extend_from_slice(b"\n{\"kind\":\"terminal\",\"schema\":1,\"state\":");
    std::fs::write(&fixture.journal, damaged)?;
    std::fs::write(fixture.project.join("a.txt"), b"uncertain partial edit")?;
    for _ in 0..2 {
        let error = fixture
            .workspace
            .preview(&fixture.selected)
            .err()
            .ok_or("admitted undo became a fresh preview")?;
        assert!(
            error.contains("uncertain partial effects"),
            "valid Applying prefix did not reconcile to quarantine: {error}"
        );
    }
    assert_eq!(
        std::fs::read(fixture.project.join("a.txt"))?,
        b"uncertain partial edit"
    );
    Ok(())
}

#[test]
fn complete_invalid_undo_records_and_operation_mismatches_remain_corruption() -> TestResult {
    let fixture = fixture()?;
    let mut mismatched = applying(&fixture)?;
    mismatched["operation"] = serde_json::json!("different-operation");
    for damaged in [
        br#"{"kind":"applying","schema":2}"#.to_vec(),
        serde_json::to_vec(&mismatched)?,
    ] {
        std::fs::write(&fixture.journal, &damaged)?;
        let error = fixture
            .workspace
            .preview(&fixture.selected)
            .err()
            .ok_or("complete corrupt journal became a fresh preview")?;
        assert!(
            error.contains("corrupt"),
            "complete invalid journal lost its explicit refusal: {error}"
        );
        assert_eq!(std::fs::read(&fixture.journal)?, damaged);
    }
    assert_eq!(std::fs::read(fixture.project.join("a.txt"))?, b"after");
    Ok(())
}
