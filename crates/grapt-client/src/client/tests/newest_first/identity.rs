use super::*;

#[test]
fn review_backfill_keeps_the_open_blocks_identity() -> Result<(), Box<dyn std::error::Error>> {
    for kind in ["text", "reasoning"] {
        let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
        client.reconcile(&id)?;
        hold_older_page(&mut client);
        client.peer_mut().event(
            &id,
            1,
            &format!("session.{kind}.started"),
            serde_json::json!({"assistantMessageID":"msg_original_block","ordinal":0}),
        );
        client.pump()?;
        let before = client
            .view()
            .ok_or("view")?
            .slices
            .last()
            .ok_or("slice")?
            .ordinal;
        client.peer_mut().hold = false;
        pump_until_settled(&mut client);
        let after = client
            .view()
            .ok_or("view")?
            .slices
            .last()
            .ok_or("slice")?
            .ordinal;
        assert_ne!(before, after, "older history did not shift the live slice");
        client.peer_mut().event(
            &id,
            2,
            &format!("session.{kind}.delta"),
            serde_json::json!({"assistantMessageID":"msg_other_block","ordinal":1,"delta":"unrelated Ω"}),
        );
        client.pump()?;
        assert!(
            !client
                .view()
                .ok_or("view")?
                .slices
                .iter()
                .any(|slice| slice.text.contains("unrelated Ω")),
            "backfill let a foreign block append to the open {kind} block"
        );
    }
    Ok(())
}
