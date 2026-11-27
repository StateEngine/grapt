use super::*;

#[cfg(unix)]
#[tokio::test]
async fn yielded_terminal_keeps_its_origin_and_expired_handles_never_replay() -> Result<(), String>
{
    let scratch = super::super::super::tests::Scratch::new()?;
    let session = session("ses_recovery_origin")?;
    let other = super::session("ses_recovery_other")?;
    let operation = operation("op_recovery_origin")?;
    let registry = TerminalRegistry::default();
    let mut spec = spec(
        &scratch.0,
        "printf once >> started; printf READY; sleep 30",
        false,
    )?;
    spec.origin = Some(super::super::recovery::Origin::local(123456));
    let (bytes, delivery) = registry
        .exec(&session, operation.clone(), spec)
        .await
        .map_err(failure)?;
    delivery.captured();
    let value = json(&bytes)?;
    assert_eq!(value["recovery"]["started_ms"], 123456);
    assert!(value["recovery"]["host"]
        .as_str()
        .is_some_and(|host| host.starts_with("local/")));
    assert!(registry.contains(&session, &operation));
    assert!(!registry.contains(&other, &operation));
    let entry = registry.lookup(&session, &operation)?;
    await_output(&entry, "READY").await?;
    drop(registry);
    let _settled = settled_report(&entry).await?;
    let restarted = TerminalRegistry::default();
    let error = match restarted.lookup(&session, &operation) {
        Ok(_) => return Err("restart resurrected a terminal handle".into()),
        Err(error) => error,
    };
    assert!(error.contains("status unknown"));
    assert!(error.contains("history({})"));
    assert!(error.contains("do not rerun"));
    assert_eq!(
        std::fs::read_to_string(scratch.0.join("started")).map_err(|error| error.to_string())?,
        "once"
    );
    Ok(())
}
