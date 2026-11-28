use super::*;
use grapt_store::{AggregateId, EventStore};
use grapt_types::{IdToken, Timestamp};
use std::sync::atomic::{AtomicU64, Ordering};

mod override_tests;
mod rules_tests;

#[derive(Debug, Default)]
struct Ids(AtomicU64);
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new(format!("scc{}", self.0.fetch_add(1, Ordering::Relaxed)))
            .unwrap_or_else(|| panic!("fixture ID invalid"))
    }
}
#[derive(Debug)]
struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_000)
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    effective: EffectiveInstructions,
}
impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "grapt-scc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        std::fs::create_dir(root.join("project"))?;
        std::fs::create_dir(root.join("project/sub"))?;
        std::fs::create_dir(root.join("config"))?;
        let session: SessionId = "ses_scc_instructions".parse()?;
        let mut store = EventStore::open(&root.join("events.sqlite3"), "scc-instructions")?;
        store.elect_writer(&AggregateId::Session(session.clone()))?;
        InstructionService::ensure_tables(&mut store)?;
        let effective = EffectiveInstructions {
            roots: InstructionRoots {
                agents: String::new(),
                directory: root.join("project/sub"),
                project: root.join("project"),
                config: root.join("config"),
                skills: Ok(Vec::new()),
                skill_warnings: Vec::new(),
            },
            inbox: Arc::new(Mutex::new(Inbox::new(store)?)),
            session,
            ids: Arc::new(Ids::default()),
            clock: Arc::new(Clock),
        };
        Ok(Self { root, effective })
    }
    fn history(&self) -> Result<grapt_instruction::InstructionHistory, String> {
        let inbox = self.effective.inbox.lock().map_err(|_| "poisoned")?;
        InstructionService::history(inbox.store(), &self.effective.session)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "missing baseline".to_owned())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn text(prepared: &PreparedInstructions) -> String {
    let mut items = prepared.current.clone();
    items.extend(prepared.notices.clone());
    items
        .iter()
        .map(|item| match item {
            CodexInputItem::Message(message) => {
                assert_eq!(message.role, MessageRole::System);
                message.content.as_str()
            }
            _ => panic!("instruction source produced a tool item"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn scc_instructions_refresh_preserves_order_epoch_and_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new()?;
    std::fs::write(fixture.root.join("config/AGENTS.md"), "global rule")?;
    std::fs::write(fixture.root.join("project/AGENTS.md"), "project rule")?;
    std::fs::write(fixture.root.join("project/sub/AGENTS.md"), "near rule")?;
    std::fs::write(fixture.root.join("AGENTS.md"), "foreign rule")?;
    let first = text(&fixture.effective.refresh()?);
    assert!(
        first.contains("global rule")
            && first.contains("near rule")
            && first.contains("project rule")
    );
    assert!(first.find("global rule") < first.find("near rule"));
    assert!(first.find("near rule") < first.find("project rule"));
    assert!(!first.contains("foreign rule"));
    assert!(first.contains("Session ID: ses_scc_instructions"));
    assert!(first.contains("You are Grapt"));
    for guidance in [
        "Before grouped tool calls, always write a brief 1–2 sentence user-facing preamble",
        "as ordinary assistant text, then call the tools",
        "On long tasks, share short progress updates",
        "do not narrate every call",
        "Use a tool's optional description field",
    ] {
        assert!(
            first.contains(guidance),
            "prepared instructions omit {guidance}"
        );
    }
    let before = fixture.history()?;
    assert_eq!(first, text(&fixture.effective.refresh()?));
    assert_eq!(before, fixture.history()?);
    std::fs::write(fixture.root.join("project/sub/AGENTS.md"), "changed rule")?;
    let changed = text(&fixture.effective.refresh()?);
    assert!(changed.contains("changed rule"));
    assert!(!changed.contains("near rule"));
    assert_eq!(before.epoch, fixture.history()?.epoch);
    assert!(fixture.history()?.through > before.through);
    // Reopen the durable store with a fresh consumer. The old values cannot be
    // supplied by the previous service object or its in-memory projection.
    let mut store = EventStore::open(&fixture.root.join("events.sqlite3"), "scc-restart")?;
    store.elect_writer(&AggregateId::Session(fixture.effective.session.clone()))?;
    fixture.effective.inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    assert_eq!(changed, text(&fixture.effective.refresh()?));
    Ok(())
}

#[test]
fn scc_unavailable_is_not_empty_or_removed() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("project/AGENTS.md");
    std::fs::create_dir(&file)?;
    assert!(fixture.effective.refresh().is_err());
    std::fs::remove_dir(&file)?;
    std::fs::write(&file, "retained while unavailable")?;
    let initial = text(&fixture.effective.refresh()?);
    std::fs::remove_file(&file)?;
    std::fs::create_dir(&file)?;
    let unavailable = text(&fixture.effective.refresh()?);
    assert!(unavailable.contains("retained while unavailable"));
    assert!(unavailable.contains("Instruction source core/instructions is temporarily unavailable"));
    assert!(unavailable.contains("bounded regular file"));
    assert!(unavailable.contains("repair the source and retry refresh"));
    assert!(unavailable.len() <= initial.len() + 1024);
    std::fs::remove_dir(&file)?;
    std::fs::write(&file, "")?;
    let empty = text(&fixture.effective.refresh()?);
    assert!(!empty.contains("retained while unavailable"));
    assert!(empty.contains("Instructions from:"));
    std::fs::remove_file(&file)?;
    let removed = text(&fixture.effective.refresh()?);
    assert!(!removed.contains("Instructions from:"));
    assert!(removed.contains("Current UTC date: 1970-01-01"));
    Ok(())
}

#[test]
fn scc_session_context_renders_strings_and_excludes_the_superseded_identity_packet(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    {
        let mut inbox = fixture.effective.inbox.lock().map_err(|_| "lock")?;
        InstructionService::put_entry(
            inbox.store_mut(),
            &fixture.effective.session,
            &InstructionKey::new("writing").ok_or("key")?,
            r#""Use concise reports.""#,
            1,
        )?;
        InstructionService::put_entry(
            inbox.store_mut(),
            &fixture.effective.session,
            &InstructionKey::new("stateengine-identity").ok_or("key")?,
            r#"{"scope":"project","project_id":"fixture","agent_id":"agt_fixture","instruction":"Keep the coding task moving","assembled_model_input":"SCC_OLD_PACKET: call resume yourself"}"#,
            1,
        )?;
    }
    let rendered = text(&fixture.effective.refresh()?);
    assert!(rendered.contains("Session context writing:\nUse concise reports."));
    assert!(rendered.contains("Keep the coding task moving"));
    assert!(!rendered.contains("SCC_OLD_PACKET"));
    assert!(rendered.contains(super::super::stateengine_resume::BOUND_RESUME_GUIDANCE));
    Ok(())
}

// SKILLS-DELEGATION-01 J01: scope, precedence and distinct failure classes.

#[test]
fn skd_precedence_is_stated_once_and_discovery_order_is_preserved(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    std::fs::write(fixture.root.join("config/AGENTS.md"), "global rule")?;
    std::fs::write(fixture.root.join("project/AGENTS.md"), "project rule")?;
    std::fs::write(fixture.root.join("project/sub/AGENTS.md"), "near rule")?;
    let rendered = text(&fixture.effective.refresh()?);
    assert_eq!(rendered.matches("AGENTS.md precedence:").count(), 1);
    let precedence = rendered.find("AGENTS.md precedence:");
    assert!(precedence < rendered.find("global rule"));
    assert!(rendered.find("global rule") < rendered.find("near rule"));
    assert!(rendered.find("near rule") < rendered.find("project rule"));
    Ok(())
}

#[test]
fn skd_absent_instructions_render_no_precedence_statement() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::new()?;
    let rendered = text(&fixture.effective.refresh()?);
    assert!(!rendered.contains("AGENTS.md precedence:"));
    assert!(!rendered.contains("Instructions from:"));
    Ok(())
}

#[test]
fn skd_non_utf8_instructions_are_unavailable_not_empty() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("project/AGENTS.md");
    std::fs::write(&file, "last good rule")?;
    let good = text(&fixture.effective.refresh()?);
    assert!(good.contains("last good rule"));
    std::fs::write(&file, [0xff, 0xfe, 0x00, 0x41])?;
    let after = text(&fixture.effective.refresh()?);
    assert!(
        after.contains("last good rule"),
        "last known good was dropped"
    );
    assert!(
        after.contains("not UTF-8"),
        "the failure lost its reason: {after}"
    );
    Ok(())
}

#[test]
fn skd_oversized_instructions_are_unavailable_not_truncated(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("project/AGENTS.md");
    std::fs::write(&file, "last good rule")?;
    fixture.effective.refresh()?;
    std::fs::write(&file, "x".repeat(MAX_FILE_BYTES + 1))?;
    let after = text(&fixture.effective.refresh()?);
    assert!(after.contains("last good rule"));
    assert!(after.contains("bounded regular file"));
    assert!(
        !after.contains(&"x".repeat(1024)),
        "an oversized file leaked into the request"
    );
    Ok(())
}

#[test]
fn skd_deleted_instructions_are_withdrawn() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("project/sub/AGENTS.md");
    std::fs::write(&file, "temporary rule")?;
    assert!(text(&fixture.effective.refresh()?).contains("temporary rule"));
    std::fs::remove_file(&file)?;
    let after = text(&fixture.effective.refresh()?);
    assert!(!after.contains("temporary rule"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn skd_project_instructions_linking_outside_the_project_are_refused(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    std::fs::write(fixture.root.join("project/AGENTS.md"), "project rule")?;
    let good = text(&fixture.effective.refresh()?);
    assert!(good.contains("project rule"));
    std::fs::write(fixture.root.join("outside.md"), "foreign linked rule")?;
    std::os::unix::fs::symlink(
        fixture.root.join("outside.md"),
        fixture.root.join("project/sub/AGENTS.md"),
    )?;
    let after = text(&fixture.effective.refresh()?);
    assert!(
        !after.contains("foreign linked rule"),
        "a foreign file contributed"
    );
    assert!(
        after.contains("project rule"),
        "last known good was dropped"
    );
    assert!(after.contains("resolves outside the directory it governs"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn skd_global_instructions_may_link_into_a_dotfiles_tree() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::new()?;
    std::fs::write(fixture.root.join("dotfiles.md"), "dotfiles rule")?;
    std::os::unix::fs::symlink(
        fixture.root.join("dotfiles.md"),
        fixture.root.join("config/AGENTS.md"),
    )?;
    let rendered = text(&fixture.effective.refresh()?);
    assert!(rendered.contains("dotfiles rule"));
    Ok(())
}

#[test]
fn skd_scoped_instructions_cover_only_directories_below_the_session(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let session = fixture.root.join("project/sub");
    let deep = session.join("a/b");
    std::fs::create_dir_all(&deep)?;
    std::fs::write(session.join("AGENTS.md"), "session-level rule")?;
    std::fs::write(session.join("a/AGENTS.md"), "a rule")?;
    std::fs::write(deep.join("AGENTS.md"), "b rule")?;
    std::fs::write(deep.join("file.rs"), "fn main() {}\n")?;
    let session = std::fs::canonicalize(&session)?;
    let scoped = scoped_for_read(&session, &session.join("a/b/file.rs")).instructions;
    let rendered: Vec<(String, String)> = scoped
        .into_iter()
        .map(|scoped| scoped.content.map(|content| (scoped.path, content)))
        .collect::<Result<_, _>>()?;
    assert_eq!(
        rendered,
        vec![
            ("a/b/AGENTS.md".to_owned(), "b rule".to_owned()),
            ("a/AGENTS.md".to_owned(), "a rule".to_owned()),
        ]
    );
    assert!(scoped_for_read(&session, &session.join("top.rs")).is_empty());
    Ok(())
}

#[test]
fn skd_unusable_scoped_instructions_are_reported_not_dropped(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let session = fixture.root.join("project/sub");
    std::fs::create_dir_all(session.join("a"))?;
    std::fs::write(session.join("a/AGENTS.md"), [0xff, 0xfe])?;
    let session = std::fs::canonicalize(&session)?;
    let scoped = scoped_for_read(&session, &session.join("a/file.rs")).instructions;
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].path, "a/AGENTS.md");
    match &scoped[0].content {
        Err(reason) => assert!(reason.contains("not UTF-8"), "{reason}"),
        Ok(content) => panic!("an unreadable scoped file was delivered: {content}"),
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn skd_scoped_instructions_linking_outside_the_session_are_refused(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let session = fixture.root.join("project/sub");
    std::fs::create_dir_all(session.join("a"))?;
    std::fs::write(fixture.root.join("outside.md"), "foreign scoped rule")?;
    std::os::unix::fs::symlink(fixture.root.join("outside.md"), session.join("a/AGENTS.md"))?;
    let session = std::fs::canonicalize(&session)?;
    let scoped = scoped_for_read(&session, &session.join("a/file.rs")).instructions;
    assert_eq!(scoped.len(), 1);
    match &scoped[0].content {
        Err(reason) => assert!(reason.contains("resolves outside"), "{reason}"),
        Ok(content) => panic!("a foreign scoped file was delivered: {content}"),
    }
    Ok(())
}

#[test]
fn skd_scoped_instructions_stop_at_their_aggregate_bound() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::new()?;
    let session = fixture.root.join("project/sub");
    let deep = session.join("a/b");
    std::fs::create_dir_all(&deep)?;
    std::fs::write(deep.join("AGENTS.md"), "b".repeat(MAX_FILE_BYTES - 10))?;
    std::fs::write(session.join("a/AGENTS.md"), "a".repeat(64))?;
    let session = std::fs::canonicalize(&session)?;
    let scoped = scoped_for_read(&session, &session.join("a/b/file.rs")).instructions;
    assert_eq!(scoped.len(), 2);
    assert!(scoped[0].content.is_ok());
    match &scoped[1].content {
        Err(reason) => assert!(reason.contains("exceed 64 KiB"), "{reason}"),
        Ok(_) => panic!("the aggregate bound was not enforced"),
    }
    Ok(())
}

#[test]
fn skd_scoped_only_reads_carry_the_precedence_contract() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let session = fixture.root.join("project/sub");
    std::fs::create_dir_all(session.join("pkg/deep"))?;
    std::fs::write(session.join("pkg/AGENTS.md"), "parent: four spaces")?;
    std::fs::write(session.join("pkg/deep/AGENTS.md"), "child: two spaces")?;
    let session = std::fs::canonicalize(&session)?;
    let rendered = scoped_for_read(&session, &session.join("pkg/deep/file.rs")).render_text();
    let contract = rendered
        .find(SCOPED_PRECEDENCE)
        .ok_or("no precedence contract")?;
    let child = rendered
        .find("child: two spaces")
        .ok_or("child rule missing")?;
    let parent = rendered
        .find("parent: four spaces")
        .ok_or("parent rule missing")?;
    assert!(contract < child && child < parent, "{rendered}");
    assert!(SCOPED_PRECEDENCE.contains("user gives in this conversation override"));
    assert!(scoped_for_read(&session, &session.join("top.rs"))
        .render_text()
        .is_empty());
    Ok(())
}

#[test]
fn skd_the_scoped_walk_reports_its_bound_exactly() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let session = fixture.root.join("project/sub");
    let at = |depth: usize| vec!["a"; depth].join("/");
    std::fs::create_dir_all(session.join(at(MAX_FILES + 1)))?;
    std::fs::write(session.join("a/AGENTS.md"), "depth-one rule")?;
    let session = std::fs::canonicalize(&session)?;
    // Exactly at the bound: every directory is checked, so nothing is unknown.
    let bounded = scoped_for_read(&session, &session.join(at(MAX_FILES)).join("f.rs"));
    assert!(bounded.unchecked.is_none());
    assert_eq!(bounded.instructions.len(), 1);
    assert!(bounded.render_text().contains("depth-one rule"));
    // One directory beyond: the unchecked directory is named, not inferred absent.
    let beyond = scoped_for_read(&session, &session.join(at(MAX_FILES + 1)).join("f.rs"));
    assert_eq!(beyond.unchecked.as_deref(), Some("a"));
    assert!(beyond.instructions.is_empty());
    let rendered = beyond.render_text();
    assert!(rendered.contains(SCOPED_PRECEDENCE), "{rendered}");
    assert!(
        rendered.contains("Scoped instructions incomplete") && rendered.contains("a and the"),
        "{rendered}"
    );
    Ok(())
}

// SKILLS-DELEGATION-01 J02: skill guidance is metadata, optional and honest.

fn skill(id: &str, description: &str) -> crate::dogfood::catalogs::SkillSummary {
    crate::dogfood::catalogs::SkillSummary {
        id: id.to_owned(),
        name: id.to_owned(),
        description: description.to_owned(),
    }
}

#[test]
fn skd_skill_guidance_defers_metadata_until_discovery() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new()?;
    fixture.effective.roots.skills = Ok(vec![skill("release-notes", "Draft release notes.")]);
    let rendered = text(&fixture.effective.refresh()?);
    assert!(rendered.contains("1 available skills"));
    assert!(rendered.contains("task keywords"));
    assert!(!rendered.contains("release-notes"));
    assert!(!rendered.contains("Draft release notes."));
    assert!(rendered.contains("Use the skill tool to load a skill"));
    Ok(())
}

#[test]
fn skill_count_and_metadata_size_do_not_expand_the_initial_prompt() {
    let skills = vec![skill("release-notes", &"long metadata ".repeat(1_000)); 1_000];
    let rendered = render_skills(&skills);
    assert!(rendered.len() < 512);
    assert!(rendered.contains("1000 available skills"));
    assert!(rendered.contains("next_cursor"));
    assert!(!rendered.contains("long metadata"));
}

#[test]
fn skd_an_unreadable_skill_catalog_does_not_stop_a_new_session(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new()?;
    fixture.effective.roots.skills = Err("skill frontmatter is malformed".to_owned());
    let rendered = text(&fixture.effective.refresh()?);
    assert!(!rendered.contains("available skills"));
    assert!(rendered.contains("core/skill-guidance is temporarily unavailable"));
    assert!(rendered.contains("skill frontmatter is malformed"));
    Ok(())
}

#[test]
fn skd_a_broken_skill_catalog_keeps_the_last_guidance() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new()?;
    fixture.effective.roots.skills = Ok(vec![skill("release-notes", "Draft release notes.")]);
    fixture.effective.refresh()?;
    // Restart: the last guidance comes from the durable store, not memory.
    let mut store = EventStore::open(&fixture.root.join("events.sqlite3"), "skd-restart")?;
    store.elect_writer(&AggregateId::Session(fixture.effective.session.clone()))?;
    fixture.effective.inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    fixture.effective.roots.skills = Err("skill directory is unreadable".to_owned());
    let rendered = text(&fixture.effective.refresh()?);
    assert!(
        rendered.contains("1 available skills"),
        "last guidance dropped"
    );
    assert!(rendered.contains("skill directory is unreadable"));
    Ok(())
}

#[test]
fn skd_no_offered_skills_withdraws_the_guidance() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new()?;
    fixture.effective.roots.skills = Ok(vec![skill("release-notes", "Draft release notes.")]);
    fixture.effective.refresh()?;
    fixture.effective.roots.skills = Ok(Vec::new());
    let rendered = text(&fixture.effective.refresh()?);
    assert!(!rendered.contains("available skills"));
    Ok(())
}

#[test]
fn a_session_keeps_its_epochs_base_bytes_and_tools_across_a_rewind(
) -> Result<(), Box<dyn std::error::Error>> {
    let shown = |request: &grapt_provider::ChatGptCodexRequest| {
        let names: Vec<_> = request.tools.iter().map(|tool| tool.name.clone()).collect();
        let input = format!("{:?}", request.input);
        let base = input.contains("read_lines reads source with line numbers.");
        (
            names.contains(&"read_lines".to_owned()),
            names.contains(&"subagent".to_owned()),
            base,
        )
    };
    let current = Fixture::new()?;
    assert_eq!(
        shown(&planned_catalog_request(&current, Vec::new(), false)?),
        (false, false, false)
    );
    // A Session that began before the record holds the base it began with.
    let legacy = Fixture::new()?;
    let observations = vec![SourceObservation::Available {
        key: key("core/base")?,
        value: InstructionValue::Text(builtin_epoch::legacy_base()),
    }];
    {
        let mut inbox = legacy.effective.inbox.lock().map_err(|_| "poisoned")?;
        let input = RefreshInput {
            event: "evt_legacy_baseline".parse()?,
            session: legacy.effective.session.clone(),
            created_ms: 1,
            required: Vec::new(),
            observations,
        };
        InstructionService::refresh(inbox.store_mut(), input)?;
    }
    rewind(&current)?;
    rewind(&legacy)?;
    // A rewind clears the instructions but not the epoch either began in.
    assert_eq!(
        shown(&planned_catalog_request(&current, Vec::new(), false)?),
        (false, false, false)
    );
    for _ in 0..2 {
        let request = planned_catalog_request(&legacy, Vec::new(), false)?;
        assert_eq!(shown(&request), (true, true, true));
    }
    Ok(())
}

#[test]
fn an_epoch_a_newer_grapt_recorded_is_kept_across_a_rewind(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let text = |value: &str| InstructionValue::Text(value.to_owned());
    {
        let mut inbox = fixture.effective.inbox.lock().map_err(|_| "poisoned")?;
        let input = RefreshInput {
            event: "evt_newer_baseline".parse()?,
            session: fixture.effective.session.clone(),
            created_ms: 1,
            required: Vec::new(),
            observations: vec![
                SourceObservation::Available {
                    key: key("core/base")?,
                    value: text("newer base"),
                },
                SourceObservation::Available {
                    key: key(builtin_epoch::KEY)?,
                    value: text("3"),
                },
            ],
        };
        InstructionService::refresh(inbox.store_mut(), input)?;
    }
    // A rewind clears both; the refresh restores the ones the Session held.
    for rewound in [false, true] {
        if rewound {
            rewind(&fixture)?;
        }
        let request = planned_catalog_request(&fixture, Vec::new(), false)?;
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert!(!names.contains(&"read_lines") && !names.contains(&"subagent"));
        assert!(format!("{:?}", request.input).contains("newer base"));
        let current = {
            let inbox = fixture.effective.inbox.lock().map_err(|_| "poisoned")?;
            InstructionService::current(inbox.store(), &fixture.effective.session)?
                .values
                .ok_or("no current values")?
        };
        let value = |name: &str| {
            let found = current.iter().find(|value| value.key.as_str() == name);
            found.map(|value| value.value.clone())
        };
        assert_eq!(value("core/base").as_deref(), Some("newer base"));
        assert_eq!(value(builtin_epoch::KEY).as_deref(), Some("3"));
    }
    Ok(())
}

/// Commit a rewind, which clears the Session's instruction history.
fn rewind(fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    let mut inbox = fixture.effective.inbox.lock().map_err(|_| "poisoned")?;
    let session = &fixture.effective.session;
    let data = serde_json::json!({"sessionID": session.as_str(), "to": "msg_boundary"});
    let aggregate = AggregateId::Session(session.clone());
    let writer = inbox.store_mut().fencing_token(&aggregate)?;
    let input = grapt_store::AppendInput {
        id: &"evt_rewind".parse()?,
        aggregate,
        writer,
        kind_name: "session.revert.committed",
        version: grapt_types::EventVersion::new(1).ok_or("version")?,
        created_ms: 2,
        data: &data,
    };
    let projector = &mut grapt_instruction::InstructionProjector;
    inbox
        .store_mut()
        .append(input, &mut [projector])?
        .1
        .discard();
    Ok(())
}

#[test]
fn base_guidance_names_only_tools_the_catalog_offers() {
    for family in [
        grapt_provider::ProtocolFamily::OpenAiResponses,
        grapt_provider::ProtocolFamily::ChatGptCodex,
        grapt_provider::ProtocolFamily::AnthropicMessages,
        grapt_provider::ProtocolFamily::OpenAiCompatible,
    ] {
        let offered = crate::dogfood::tools::offered_definitions(
            family,
            &crate::dogfood::instructions::BuiltinEpoch::Current,
        );
        for name in ["glob", "grep", "list", "read", "shell", "edit", "write"] {
            assert!(BASE.contains(name), "base guidance no longer names {name}");
            assert!(
                offered.iter().any(|tool| tool.name == name),
                "base guidance names {name}, which the tool catalog does not offer"
            );
        }
    }
    assert!(BASE.contains("use the offered patch tool"));
}

#[test]
fn epoch_prefix_survives_replacement_removal_and_reopen() -> Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = Fixture::new()?;
    let path = fixture.root.join("project/AGENTS.md");
    std::fs::write(&path, "original project rule")?;
    let first = fixture.effective.refresh()?;
    let source_text = |items: &[CodexInputItem]| {
        items
            .iter()
            .map(|item| match item {
                CodexInputItem::Message(message) => message.content.clone(),
                _ => panic!("instruction context contains a non-message"),
            })
            .collect::<Vec<_>>()
    };
    std::fs::write(&path, "replacement project rule")?;
    let changed = fixture.effective.refresh()?;
    assert_eq!(source_text(&first.initial), source_text(&changed.initial));
    assert!(text(&changed).contains("replacement project rule"));
    assert!(!text(&changed).contains("original project rule"));
    let update = changed.updates.last().ok_or("replacement update missing")?;
    let tail = source_text(&[epoch_update(update)]).join("\n");
    assert!(tail.contains("core/instructions replaces its previously supplied value"));
    assert!(tail.contains("replacement project rule"));
    std::fs::remove_file(&path)?;
    let removed = fixture.effective.refresh()?;
    let update = removed.updates.last().ok_or("removal update missing")?;
    assert!(source_text(&[epoch_update(update)])
        .join("\n")
        .contains("no longer applies"));
    let mut store = EventStore::open(&fixture.root.join("events.sqlite3"), "epoch-reopen")?;
    store.elect_writer(&AggregateId::Session(fixture.effective.session.clone()))?;
    fixture.effective.inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    let reopened = fixture.effective.refresh()?;
    assert_eq!(source_text(&first.initial), source_text(&reopened.initial));
    assert_eq!(removed.updates, reopened.updates);
    Ok(())
}

#[test]
fn cache04_session_partition_uses_typed_identity_without_demoting_system_authority(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    std::fs::write(
        fixture.root.join("project/AGENTS.md"),
        "Session ID: quoted repository text stays in the stable policy",
    )?;
    let tools = catalog("{}");
    let prepared = fixture
        .effective
        .refresh_with_tools(Some((&tools, false)))?;
    for epochs in [false, true] {
        assert_eq!(
            prepared.session_context(),
            Some("Session ID: ses_scc_instructions")
        );
        let prefix = prepared.prefix(epochs, false);
        let mut text = String::new();
        for item in prefix {
            let CodexInputItem::Message(message) = item else {
                panic!("instruction was not System text")
            };
            assert_eq!(message.role, MessageRole::System);
            text.push_str(&message.content);
        }
        assert!(text.contains("Session ID: quoted repository text"));
        assert!(!text.contains("ses_scc_instructions"));
        assert_eq!(
            prepared.prefix(epochs, true),
            if epochs {
                prepared.initial.clone()
            } else {
                prepared.current.clone()
            }
        );
    }
    Ok(())
}

fn catalog(schema: &str) -> Vec<grapt_provider::ToolDefinition> {
    vec![grapt_provider::ToolDefinition {
        input_format: grapt_provider::ToolInputFormat::Json,
        name: "fixture_lookup".into(),
        description: Some("Look up a fixture".into()),
        input_schema: schema.into(),
    }]
}

#[test]
fn mcp_tool_epoch_keeps_the_header_and_orders_changes_across_failure_and_reopen(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new()?;
    let original = catalog(r#"{"type":"object","properties":{"old":{"type":"string"}}}"#);
    let replacement = catalog(r#"{"type":"object","properties":{"new":{"type":"string"}}}"#);
    let first = fixture
        .effective
        .refresh_with_tools(Some((&original, false)))?;
    assert_eq!(first.tools.frozen.as_ref(), Some(&original));
    assert!(first.updates.is_empty());
    assert!(
        !text(&first).contains("fixture_lookup"),
        "header metadata is not repeated in system text"
    );
    let changed = fixture
        .effective
        .refresh_with_tools(Some((&replacement, false)))?;
    assert_eq!(changed.tools.frozen.as_ref(), Some(&original));
    assert_eq!(first.initial, changed.initial);
    assert_eq!(changed.updates.len(), 1);
    let update = changed.updates.first().ok_or("missing catalog update")?;
    let CodexInputItem::Message(update_text) = epoch_update(update) else {
        return Err("update must be a message".into());
    };
    assert!(update_text.content.contains("MCP catalog update"));
    assert!(update_text.content.contains("new"));
    assert!(!update_text.content.contains(tool_epoch::FROZEN));
    let unavailable = fixture.effective.refresh_with_tools(Some((&[], true)))?;
    assert_eq!(unavailable.tools.frozen, changed.tools.frozen);
    assert_eq!(
        unavailable.updates, changed.updates,
        "failed refresh keeps the last good catalog"
    );
    assert!(text(&unavailable).contains("MCP discovery is temporarily incomplete"));
    let mut store = EventStore::open(&fixture.root.join("events.sqlite3"), "mcp-epoch-reopen")?;
    store.elect_writer(&AggregateId::Session(fixture.effective.session.clone()))?;
    InstructionService::rebuild(&mut store, &fixture.effective.session)?;
    fixture.effective.inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    let reopened = fixture
        .effective
        .refresh_with_tools(Some((&replacement, false)))?;
    assert_eq!(reopened.tools.frozen, first.tools.frozen);
    assert_eq!(reopened.updates, changed.updates);
    let removed = fixture.effective.refresh_with_tools(Some((&[], false)))?;
    assert_eq!(removed.tools.frozen, first.tools.frozen);
    assert_eq!(
        removed.updates.len(),
        2,
        "a confirmed empty catalog is a real chronological change"
    );
    assert!(text(&removed).contains("MCP catalog update"));
    {
        let mut inbox = fixture.effective.inbox.lock().map_err(|_| "poisoned")?;
        let session = fixture.effective.session.clone();
        let aggregate = AggregateId::Session(session.clone());
        let writer = inbox.store().fencing_token(&aggregate)?;
        let id: EventId = "evt_mcp_epoch_compacted".parse()?;
        let data = serde_json::json!({"sessionID":session.as_str(), "reason":"auto", "text":"compacted", "recent":"recent"});
        let (_, notices) = inbox.store_mut().append(
            grapt_store::AppendInput {
                id: &id,
                aggregate,
                writer,
                kind_name: "session.compaction.ended",
                version: grapt_types::EventVersion::new(1).ok_or("version")?,
                created_ms: 1_000,
                data: &data,
            },
            &mut [&mut grapt_instruction::InstructionProjector],
        )?;
        notices.discard();
    }
    let compacted = fixture.effective.refresh_with_tools(Some((&[], false)))?;
    assert_eq!(compacted.tools.frozen, first.tools.frozen);
    assert!(compacted.updates.is_empty());
    assert!(compacted.initial.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.content.contains("MCP catalog update"))), "compaction must carry the current catalog into the new epoch baseline");
    let other = Fixture::new()?;
    assert_eq!(
        other
            .effective
            .refresh_with_tools(Some((&replacement, false)))?
            .tools
            .frozen,
        Some(replacement)
    );
    Ok(())
}

#[test]
fn production_requests_keep_the_tools_header_after_catalog_failure_and_change(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new()?;
    let original = catalog(r#"{"type":"object","properties":{"old":{"type":"string"}}}"#);
    let replacement = catalog(r#"{"type":"object","properties":{"new":{"type":"string"}}}"#);
    let mut requests = Vec::new();
    for (definitions, refresh_failed) in [
        (original.clone(), false),
        (Vec::new(), true),
        (replacement, false),
    ] {
        requests.push(planned_catalog_request(
            &fixture,
            definitions,
            refresh_failed,
        )?);
    }
    assert_eq!(requests[0].tools, requests[1].tools);
    assert_eq!(requests[0].tools, requests[2].tools);
    assert!(requests[0].tools.contains(&original[0]));
    for request in [&requests[0], &requests[2]] {
        assert!(request.input.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.content.contains("Use fixture_lookup for exact record identifiers."))));
    }
    // The first server is absent from the live publication even while another
    // server's discovery is incomplete. Its old guidance must be withdrawn.
    assert!(!requests[1].input.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.content.contains("Use fixture_lookup for exact record identifiers."))));
    assert!(requests[2].input.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.content.contains("MCP catalog update"))));
    // The journal is the owner, so recreating it from durable facts retains the header.
    let mut store = EventStore::open(&fixture.root.join("events.sqlite3"), "mcp-plan-reopen")?;
    store.elect_writer(&AggregateId::Session(fixture.effective.session.clone()))?;
    fixture.effective.inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    assert_eq!(
        fixture
            .effective
            .refresh_with_tools(Some((&[], true)))?
            .tools
            .frozen,
        Some(original)
    );
    let removed = planned_catalog_request(&fixture, Vec::new(), false)?;
    assert!(!removed.input.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.content.contains("Use fixture_lookup for exact record identifiers."))));
    let current = fixture.effective.refresh_with_tools(Some((&[], false)))?;
    assert!(current.updates.iter().any(|update| update.changes.iter().any(
        |change| matches!(change, grapt_instruction::InstructionChange::Removed { key } if key.as_str() == "core/mcp-guidance")
    )));
    Ok(())
}

#[test]
fn first_failed_or_partial_mcp_discovery_recovers_after_reopen(
) -> Result<(), Box<dyn std::error::Error>> {
    for partial in [Vec::new(), catalog("{}")] {
        let mut fixture = Fixture::new()?;
        let first_request = planned_catalog_request(&fixture, partial.clone(), true)?;
        assert!(!first_request
            .tools
            .iter()
            .any(|tool| tool.name == "fixture_lookup"));
        let failed = fixture
            .effective
            .refresh_with_tools(Some((&partial, true)))?;
        assert!(
            failed.tools.frozen.is_none(),
            "a failed snapshot must never install the Session header"
        );
        assert!(text(&failed).contains("Before the first complete discovery"));
        let mut store =
            EventStore::open(&fixture.root.join("events.sqlite3"), "first-failure-reopen")?;
        store.elect_writer(&AggregateId::Session(fixture.effective.session.clone()))?;
        fixture.effective.inbox = Arc::new(Mutex::new(Inbox::new(store)?));
        let complete = catalog(r#"{"type":"object"}"#);
        let recovered_request = planned_catalog_request(&fixture, complete.clone(), false)?;
        assert!(
            recovered_request.tools.contains(&complete[0]),
            "recovery must offer the direct tool on the existing Session"
        );
        let recovered = fixture
            .effective
            .refresh_with_tools(Some((&complete, false)))?;
        assert_eq!(recovered.tools.frozen, Some(complete));
    }
    Ok(())
}

fn planned_catalog_request(
    fixture: &Fixture,
    definitions: Vec<grapt_provider::ToolDefinition>,
    refresh_failed: bool,
) -> Result<grapt_provider::ChatGptCodexRequest, Box<dyn std::error::Error>> {
    use super::super::production::{tests::projection_targets, ProductPlan};
    use grapt_provider::{ProtocolFamily, ProviderRequest};
    use grapt_runtime::{AttemptPlan, HistoryGeneration};
    let targets = projection_targets(ProtocolFamily::ChatGptCodex)?;
    let instructions = EffectiveInstructions {
        roots: fixture.effective.roots.clone(),
        inbox: fixture.effective.inbox.clone(),
        session: fixture.effective.session.clone(),
        ids: fixture.effective.ids.clone(),
        clock: fixture.effective.clock.clone(),
    };
    let step = grapt_types::StepNumber::new(1).ok_or("step")?;
    let mut published = super::super::mcp::PublishedTools {
        definitions,
        refresh_failed,
        ..Default::default()
    };
    for definition in &published.definitions {
        published.routes.insert(
            definition.name.clone(),
            super::super::mcp::McpRoute {
                description: definition.description.clone(),
                output_schema: None,
                server: "epoch-fixture".into(),
                tool: grapt_mcp::McpToolName::new(&definition.name).ok_or("tool")?,
                launch: super::super::mcp::LaunchIdentity {
                    declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Disabled),
                    program: "fixture".into(),
                    cwd: "/fixture".into(),
                    arguments: Vec::new(),
                    incarnation: 1,
                },
            },
        );
    }
    if !published.definitions.is_empty() {
        published.instructions.insert(
            "epoch-fixture".into(),
            "Use fixture_lookup for exact record identifiers.".into(),
        );
    }
    let published = Arc::new(published);
    let mut plan = ProductPlan::transient(
        ProtocolFamily::ChatGptCodex,
        "hello".into(),
        grapt_provider::PromptCacheKey::for_session(&fixture.effective.session)?,
        targets.issue()?,
        targets,
        published,
        Vec::new(),
        step,
    )
    .with_instructions(instructions);
    let planned = plan
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|error| error.detail)?;
    let ProviderRequest::ChatGptCodex(request) = planned.request else {
        return Err("wrong family".into());
    };
    Ok(request)
}
