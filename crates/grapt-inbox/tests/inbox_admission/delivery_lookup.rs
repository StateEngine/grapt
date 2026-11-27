//! Omitted-mode retries retain the original admission without adding writes.
use super::*;

#[test]
fn stored_modes_survive_promotion_and_lookup_is_read_only() -> TestResult {
    for (suffix, mode) in [
        ("queue", DeliveryMode::Queue),
        ("steer", DeliveryMode::Steer),
    ] {
        let (mut inbox, session) = fresh()?;
        let input = format!("msg_lookup_{suffix}").parse()?;
        assert_eq!(inbox.admitted_delivery(&input)?, None);
        admit(
            &mut inbox,
            &format!("evt_lookup_{suffix}").parse()?,
            &session,
            &input,
            "hello",
            mode,
        )?;
        let count = |inbox: &Inbox| -> rusqlite::Result<i64> {
            inbox
                .store()
                .connection()
                .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))
        };
        let before = count(&inbox)?;
        assert_eq!(inbox.admitted_delivery(&input)?, Some(mode));
        assert_eq!(count(&inbox)?, before);
        inbox.promote_next(
            &session,
            PromoteScope::InputAtIdle,
            &mut minter("lookup"),
            1786060800001,
        )?;
        let before = count(&inbox)?;
        assert_eq!(inbox.admitted_delivery(&input)?, Some(mode));
        assert_eq!(count(&inbox)?, before);
        inbox.store().connection().execute(
            "DELETE FROM session_message WHERE id = ?1",
            [input.as_str()],
        )?;
        assert!(matches!(
            inbox.admitted_delivery(&input),
            Err(InboxError::Corruption { .. })
        ));
    }
    Ok(())
}

#[test]
fn damaged_delivery_is_an_error_rather_than_an_absent_admission() -> TestResult {
    let (mut inbox, session) = fresh()?;
    let input = "msg_lookup_corrupt".parse()?;
    admit(
        &mut inbox,
        &"evt_lookup_corrupt".parse()?,
        &session,
        &input,
        "hello",
        DeliveryMode::Queue,
    )?;
    inbox.store().connection().execute(
        "UPDATE session_pending SET delivery = 'damaged' WHERE id = ?1",
        [input.as_str()],
    )?;
    assert!(matches!(
        inbox.admitted_delivery(&input),
        Err(InboxError::Corruption { .. })
    ));
    Ok(())
}
