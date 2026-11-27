//! A first partial wake contains only settled rows and preserves the rest.
use super::*;

pub(super) fn assert_settled_wake_coverage(inbox: &Inbox, input: &PendingInput) -> TestResult {
    let scope = RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("secode")?,
            TargetProjectId::try_new("secode")?,
        ),
        principal: SourcePrincipalId::try_new("coder")?,
        recipient: SeatRecipientId::try_new("coder")?,
    };
    let page = inbox.store().read_remote_mailbox_for_binding(
        &scope,
        grapt_types::MailboxCursor::START,
        64,
        1_048_576,
    )?;
    assert!(page.next_after.is_none());
    let mut selected = 0;
    for name in ["mail-a", "mail-b"] {
        let row = page
            .records
            .iter()
            .find(|record| record.ack().source_message().as_str() == name)
            .ok_or("original addressed row absent")?;
        if input.text.contains(name) {
            selected += 1;
            assert!(
                row.consumed(),
                "selected row lacks committed wake provenance"
            );
            assert!(
                !row.delivery_pending() && !row.repair_pending(),
                "unsettled row entered the current wake"
            );
        } else {
            assert!(
                !row.consumed() && row.actionable(),
                "unselected original work was lost"
            );
        }
    }
    assert!(selected > 0, "wake contains no settled addressed work");
    assert!(!input.text.contains("quiet") && !input.text.contains("fixture-secret"));
    Ok(())
}
