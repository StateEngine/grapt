use super::*;
#[test]
fn typed_updates_preserve_the_prior_input_prefix_and_refuse_duplicate_positions(
) -> Result<(), String> {
    let fixture = Fixture::new("instruction-epoch")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let baseline = continuation.request_input("inspect", false)?;
    let update = grapt_instruction::InstructionUpdate {
        seq: grapt_types::EventSeq::new(2),
        changes: vec![grapt_instruction::InstructionChange::Value(
            grapt_instruction::ResolvedInstruction {
                key: grapt_types::InstructionKey::new("project/agents").ok_or("instruction key")?,
                value: "updated project rule".into(),
            },
        )],
    };
    let (changed, _, _) =
        continuation.request_with_instructions("inspect", false, std::slice::from_ref(&update))?;
    assert_eq!(changed.len(), baseline.len() + 1);
    for (old, new) in baseline.iter().zip(&changed) {
        match (old, new) {
            (CodexInputItem::Message(old), CodexInputItem::Message(new)) => {
                assert_eq!(old.role, new.role);
                assert_eq!(old.content, new.content);
            }
            _ => return Err("unexpected fixture input".into()),
        }
    }
    assert!(
        matches!(changed.last(), Some(CodexInputItem::Message(message)) if message.role == MessageRole::System && message.content.contains("updated project rule"))
    );
    assert!(
        continuation
            .request_with_instructions("inspect", false, &[update.clone(), update])
            .is_err(),
        "duplicate instruction coordinates must be refused"
    );
    let next: MessageId = "msg_epoch_next"
        .parse()
        .map_err(|error| format!("{error}"))?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "later prompt")?;
    let future = grapt_instruction::InstructionUpdate {
        seq: grapt_types::EventSeq::new(65_535),
        changes: vec![grapt_instruction::InstructionChange::Removed {
            key: grapt_types::InstructionKey::new("future/source").ok_or("future key")?,
        }],
    };
    let (earlier, _, _) = continuation.request_with_instructions("inspect", false, &[future])?;
    assert_eq!(
        earlier.len(),
        baseline.len(),
        "replaying an earlier input cannot acquire future instruction updates"
    );
    Ok(())
}
