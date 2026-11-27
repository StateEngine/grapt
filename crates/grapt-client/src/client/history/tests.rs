//! A newer input cannot inherit an older execution terminal.
use super::tail_terminal;
use crate::wire::{DurableFinish, DurableMessage, MessagePage, MessageWireId};

#[test]
fn every_input_origin_blocks_an_older_terminal() -> Result<(), Box<dyn std::error::Error>> {
    for input in [
        DurableMessage::User {
            text: "human".into(),
        },
        DurableMessage::SeatStartup {
            text: "startup".into(),
        },
        DurableMessage::Automated {
            text: "wake".into(),
        },
    ] {
        let terminal = DurableMessage::Assistant {
            items: Vec::new(),
            finish: Some(DurableFinish::Stop),
            error: None,
            usage: None,
            cache: Box::default(),
            footer: None,
        };
        let mut page = MessagePage {
            previous: None,
            next: None,
            unsupported: 0,
            rows: 2,
            messages: vec![input, terminal],
            ids: vec![
                MessageWireId::parse("msg_new_input".into()).ok_or("input id")?,
                MessageWireId::parse("msg_old_terminal".into()).ok_or("terminal id")?,
            ],
        };
        assert!(
            tail_terminal(&page).is_none(),
            "new input is still in flight"
        );
        page.messages.remove(0);
        page.ids.remove(0);
        page.rows = 1;
        assert!(
            tail_terminal(&page).is_some(),
            "terminal control must settle"
        );
    }
    Ok(())
}

#[test]
fn an_early_failure_row_is_the_turn_terminal() -> Result<(), Box<dyn std::error::Error>> {
    // The service records a turn that failed before any assistant message as
    // a System row with `metadata.executionFailure: true`; newest first, it
    // follows the input it ended.
    let page = MessagePage {
        previous: None,
        next: None,
        unsupported: 0,
        rows: 2,
        messages: vec![
            DurableMessage::ExecutionFailure {
                text: "context could not be prepared".into(),
            },
            DurableMessage::User {
                text: "human".into(),
            },
        ],
        ids: vec![
            MessageWireId::parse("msg_failure".into()).ok_or("failure id")?,
            MessageWireId::parse("msg_input".into()).ok_or("input id")?,
        ],
    };
    let terminal = tail_terminal(&page).ok_or("the failure must settle the turn")?;
    assert_eq!(terminal.0.as_inner().as_str(), "msg_failure");
    assert_eq!(terminal.1, crate::projection::Outcome::Failed);
    Ok(())
}

fn page(newest_first: Vec<DurableMessage>) -> Result<MessagePage, Box<dyn std::error::Error>> {
    let ids = (0..newest_first.len())
        .map(|at| MessageWireId::parse(format!("msg_row_{at}")).ok_or("row id"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MessagePage {
        previous: None,
        next: None,
        unsupported: 0,
        rows: newest_first.len(),
        messages: newest_first,
        ids,
    })
}

fn failure() -> DurableMessage {
    DurableMessage::ExecutionFailure {
        text: "context could not be prepared".into(),
    }
}

#[test]
fn an_input_after_an_early_failure_is_a_turn_in_flight() -> Result<(), Box<dyn std::error::Error>> {
    // Oldest first: failure, then a newer input.
    let in_flight = page(vec![
        DurableMessage::User {
            text: "again".into(),
        },
        failure(),
    ])?;
    assert!(
        tail_terminal(&in_flight).is_none(),
        "the newer input is running"
    );
    Ok(())
}

#[test]
fn a_failure_after_a_tool_call_step_settles_the_turn() -> Result<(), Box<dyn std::error::Error>> {
    // Oldest first: a step that called a tool, which ends nothing, then the
    // execution's failure.
    let tool_step = DurableMessage::Assistant {
        items: Vec::new(),
        finish: Some(DurableFinish::ToolCalls),
        error: None,
        usage: None,
        cache: Box::default(),
        footer: None,
    };
    let settled = page(vec![failure(), tool_step])?;
    let terminal = tail_terminal(&settled).ok_or("the failure settles the turn")?;
    assert_eq!(terminal.1, crate::projection::Outcome::Failed);
    Ok(())
}
