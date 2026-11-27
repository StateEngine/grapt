//! Ordered command results through the protocol loop and their UI consumers.
use crate::backend::tests::boundary_fixture::{Fixture, Result};
use crate::backend::{
    commands, first_prompt, followups, view::Ledger, Command, CommandId, Handoff, Outcome, Settled,
};
use crate::ui::Ui;
use grapt_client::{
    wire::session_id, Bounds, Client, ContractDouble, Fault, Operation, Transport, WireRequest,
};
use std::sync::mpsc;

#[derive(Default)]
struct Results {
    ledger: Ledger,
    submissions: first_prompt::Submissions,
    followups: followups::Submissions,
    model: Option<(CommandId, bool)>,
    forms: Vec<(CommandId, grapt_tui_forms::Reply)>,
    messages: Vec<(CommandId, crate::sessions::Message)>,
}
impl Results {
    fn collect(&mut self) {
        self.ledger.collect(
            &mut self.submissions,
            &mut self.followups,
            &mut self.model,
            &mut self.forms,
            &mut self.messages,
        );
    }
}
fn client() -> Result<Client<ContractDouble>> {
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    client.refresh_projects()?;
    let project = client.projects().first().ok_or("project")?.clone();
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model")?.clone();
    client.select_model(&model)?;
    client.create_session()?;
    Ok(client)
}
fn done(text: &str, view: commands::View) -> commands::Completion {
    let mut done = commands::Completion::new(text);
    done.view = Some(view);
    done
}
fn settled(outcome: Outcome) -> Settled {
    Settled {
        id: CommandId::TEST,
        outcome,
    }
}

#[test]
fn review_ledger_two_command_results_survive_a_pending_view_and_apply_in_order() -> Result {
    for commands in [["/status", "/models"], ["/models", "/status"]] {
        let fixture = Fixture::ready()?;
        let first = fixture.command(Command::Submit(commands[0].into()))?;
        let second = fixture.command(Command::Submit(commands[1].into()))?;
        let outcomes = fixture.outcomes(2)?;
        assert_eq!(
            outcomes.iter().map(|s| s.id).collect::<Vec<_>>(),
            [first, second]
        );
        assert!(
            outcomes
                .iter()
                .all(|s| matches!(s.outcome, Outcome::Command(_))),
            "{outcomes:?}"
        );
        let mut ui = Ui::new(false);
        ui.session_update(Some(fixture.session.clone()), false);
        assert!(crate::deliver(&mut ui, outcomes, true));
        assert_eq!(ui.models.is_some(), commands[1] == "/models");
        assert_eq!(ui.usage_pane.is_some(), commands[1] == "/status");
        assert!(
            fixture.backend.updates.take().is_none(),
            "outcomes delivered only once"
        );
    }
    Ok(())
}

#[test]
fn review_ledger_two_submissions_and_two_model_results_keep_both_ids() -> Result {
    let fixture = Fixture::paused()?;
    let first = fixture.command(Command::Submit("first unsent".into()))?;
    let second = fixture.command(Command::Submit("second unsent".into()))?;
    let outcomes = fixture.outcomes(2)?;
    assert_eq!(
        outcomes.iter().map(|s| s.id).collect::<Vec<_>>(),
        [first, second]
    );
    let texts: Vec<_> = outcomes
        .iter()
        .filter_map(|s| match &s.outcome {
            Outcome::Submission(text, false, true) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["first unsent", "second unsent"]);
    let fixture = Fixture::ready()?;
    let first = fixture.command(Command::Model(fixture.model.clone()))?;
    let mut absent = fixture.model.clone();
    absent.id = grapt_client::wire::model_id("absent-model")?;
    let second = fixture.command(Command::Model(absent))?;
    let outcomes = fixture.outcomes(2)?;
    assert_eq!(
        outcomes.iter().map(|s| s.id).collect::<Vec<_>>(),
        [first, second]
    );
    assert!(matches!(outcomes[0].outcome, Outcome::Model(true)));
    assert!(matches!(outcomes[1].outcome, Outcome::Model(false)));
    let mut ui = Ui::new(false);
    ui.models = Some(crate::models::Picker::default());
    crate::deliver(&mut ui, outcomes, true);
    assert!(ui.models.is_none());
    Ok(())
}

#[test]
fn review_ledger_two_form_replies_reach_their_board_drafts() -> Result {
    let fixture = Fixture::ready()?;
    let rows = {
        let mut source = fixture.source.lock().map_err(|_| "source lock")?;
        source
            .raise_form(&fixture.session, "First")
            .ok_or("first form")?;
        source
            .raise_form(&fixture.session, "Second")
            .ok_or("second form")?;
        let request = WireRequest::new(Operation::SessionFormList, Some(&fixture.session), None)?;
        grapt_client::wire::decode_session_forms(&source.call(&request)?.body)?
    };
    assert_eq!(rows.len(), 2);
    let mut ids = Vec::new();
    for row in &rows {
        ids.push(fixture.command(Command::Form(grapt_tui_forms::Request {
            target: grapt_tui_forms::Target::Session(fixture.session.clone()),
            form: row.clone(),
            decision: Some(grapt_tui_forms::Decision::Cancel),
        }))?);
    }
    let outcomes = fixture.outcomes(2)?;
    assert_eq!(outcomes.iter().map(|s| s.id).collect::<Vec<_>>(), ids);
    let mut ui = Ui::new(false);
    ui.forms.update(Some(fixture.session.clone()), None, rows);
    crate::deliver(&mut ui, outcomes, true);
    for index in 0..2 {
        assert_eq!(
            ui.forms.draft_state(index),
            Some(&grapt_client::wire::FormState::Cancelled)
        );
    }
    Ok(())
}

#[test]
fn review_ledger_an_unconfirmed_followup_keeps_its_origin_when_reconciliation_accepts_it() -> Result
{
    let mut client = client()?;
    let session = client.selected().ok_or("session")?.clone();
    client
        .peer_mut()
        .set_fault(Fault::PromptReplyLostUnconfirmed);
    let request = crate::followups::Request {
        session,
        text: "followup task".into(),
        files: vec![],
    };
    let (id, _) = Handoff::default().envelope(Command::Followup(request.clone()));
    let mut results = Results::default();
    assert!(results
        .followups
        .submit(&mut client, request, true, id)
        .is_err());
    assert!(client.has_unconfirmed_prompt());
    results.collect();
    let first = results.ledger.take();
    assert!(matches!(
        first[0].outcome,
        Outcome::Followup(crate::followups::Custody::Unconfirmed)
    ));
    assert_eq!(first[0].id, id);
    assert!(!client.retry_prompt("followup task")?);
    assert!(results.followups.reconcile(&client));
    results.collect();
    let final_result = results.ledger.take();
    assert!(matches!(
        final_result[0].outcome,
        Outcome::Followup(crate::followups::Custody::Accepted)
    ));
    assert_eq!(
        final_result[0].id, id,
        "reconciliation must retain the original command identity"
    );
    Ok(())
}

#[test]
fn review_ledger_later_command_completion_retains_its_submit_identity() -> Result {
    let fixture = Fixture::ready()?;
    let id = fixture.command(Command::Submit("/mcps".into()))?;
    let outcomes = fixture.outcomes(2)?;
    assert!(outcomes
        .iter()
        .any(|settled| matches!(settled.outcome, Outcome::Submission(_, false, false))));
    let completion = outcomes
        .iter()
        .find(|settled| matches!(settled.outcome, Outcome::Command(_)))
        .ok_or("operator completion")?;
    assert_eq!(
        completion.id, id,
        "a later completion answers its initiating Submit"
    );
    Ok(())
}

#[test]
fn review_ledger_two_followup_custodies_and_later_submission_keep_order_and_origin() -> Result {
    let handoff = Handoff::default();
    let mut results = Results::default();
    let mut ids = Vec::new();
    for ticket in [1, 2] {
        let request = crate::followups::Request {
            session: session_id("ses_ledger")?,
            text: format!("task{ticket}"),
            files: vec![],
        };
        let (id, _) = handoff.envelope(Command::Followup(request));
        ids.push(id);
        results.followups.result = Some((id, crate::followups::Custody::Refused));
        results.collect();
    }
    let (submission, _) = handoff.envelope(Command::Submit("later".into()));
    results.collect();
    results.submissions.outcome = Some((submission, ("later".into(), false, true)));
    results.collect();
    let outcomes = results.ledger.take();
    assert_eq!(
        outcomes.iter().map(|s| s.id).collect::<Vec<_>>(),
        [ids[0], ids[1], submission]
    );
    assert!(matches!(outcomes[0].outcome, Outcome::Followup(_)));
    assert!(matches!(outcomes[1].outcome, Outcome::Followup(_)));
    assert!(matches!(
        outcomes[2].outcome,
        Outcome::Submission(_, false, true)
    ));
    assert!(results.ledger.take().is_empty());
    Ok(())
}

#[test]
fn review_ledger_submissions_and_completions_apply_in_both_settlement_orders() {
    for submission_first in [true, false] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("draft");
        let mut completion = done("draft", commands::View::Status);
        completion.replacement = Some("expanded".into());
        let submission = settled(Outcome::Submission("draft".into(), true, true));
        let completion = settled(Outcome::Command(completion));
        let outcomes = if submission_first {
            vec![submission, completion]
        } else {
            vec![completion, submission]
        };
        crate::deliver(&mut ui, outcomes, true);
        assert_eq!(
            ui.composer.lines(),
            &[if submission_first { "" } else { "expanded" }]
        );
        assert!(ui.usage_pane.is_some());
    }
}

#[test]
fn review_ledger_search_and_opening_completion_apply_in_both_orders() -> Result {
    let fixture = Fixture::ready()?;
    let agents = fixture.take()?.agents;
    assert!(!agents.is_empty());
    let mut found = agents[0].clone();
    found.row.id = session_id("ses_search_result")?;
    for search_first in [true, false] {
        let mut ui = Ui::new(false);
        ui.agents = agents.clone();
        let search = settled(Outcome::Session(crate::sessions::Message::Search(
            crate::sessions::Search {
                query: String::new(),
                result: Ok(vec![found.clone()]),
                reach: grapt_client::CatalogReach::Read,
            },
        )));
        let open = settled(Outcome::Command(done(
            "/sessions",
            commands::View::Sessions,
        )));
        let outcomes = if search_first {
            vec![search, open]
        } else {
            vec![open, search]
        };
        crate::deliver(&mut ui, outcomes, true);
        let expected = if search_first {
            &agents[0].row.id
        } else {
            &found.row.id
        };
        assert_eq!(
            ui.picker
                .as_ref()
                .ok_or("picker")?
                .selected(&ui.agents, &ui.session_pins),
            Some(expected)
        );
    }
    Ok(())
}

#[test]
fn review_ledger_models_open_before_same_view_rows_fill_the_picker() -> Result {
    let fixture = Fixture::ready()?;
    let mut ui = Ui::new(false);
    crate::deliver(
        &mut ui,
        vec![settled(Outcome::Command(done(
            "/models",
            commands::View::Models,
        )))],
        true,
    );
    ui.model_update(
        Some(fixture.model.clone()),
        Some(vec![fixture.model.clone()]),
        None,
    );
    assert_eq!(
        ui.models.as_ref().ok_or("model picker")?.selected(),
        Some(&fixture.model)
    );
    Ok(())
}

#[test]
fn review_ledger_send_second_producer_and_panic_quit_mint_one_identity_sequence() -> Result {
    let (commands, received) = mpsc::channel();
    let (wake, _ready) = mpsc::sync_channel(1);
    let backend = crate::backend::Backend {
        commands,
        updates: Handoff::default(),
        wake,
    };
    crate::send(&backend, Command::Models)?;
    let other = backend
        .updates
        .envelope(Command::SessionSearch("other producer".into()));
    let other_id = other.0;
    backend.commands.send(other)?;
    drop(crate::PanicClose::new(&backend));
    let first = received.try_recv()?;
    let second = received.try_recv()?;
    let third = received.try_recv()?;
    assert!(first.0 < second.0 && second.0 < third.0);
    assert_eq!(second.0, other_id);
    assert!(matches!(first.1, Command::Models));
    assert!(matches!(second.1, Command::SessionSearch(_)));
    assert!(matches!(third.1, Command::Quit(_)));
    assert!(received.try_recv().is_err());
    Ok(())
}

#[test]
fn review_ledger_not_current_only_defers_safety_and_still_settles_submissions() -> Result {
    let mut ui = Ui::new(false);
    ui.models = Some(crate::models::Picker::default());
    ui.composer.insert_str("new draft");
    let safety = done(
        "old",
        commands::View::NoticeText {
            notice: "prior effect".into(),
            rows: vec!["effect detail".into()],
        },
    );
    let outcomes = vec![
        settled(Outcome::Command(done("old", commands::View::Status))),
        settled(Outcome::Model(true)),
        settled(Outcome::Command(safety)),
        settled(Outcome::Submission("old draft".into(), true, true)),
    ];
    crate::deliver(&mut ui, outcomes, false);
    assert!(ui.models.is_some());
    assert!(ui.usage_pane.is_none());
    assert!(ui.command_pane.is_none());
    assert_eq!(ui.composer.lines(), &["new draft"]);
    assert!(ui.publish_command_result(None));
    assert_eq!(ui.command_notice.as_deref(), Some("prior effect"));
    assert!(ui.command_pane.is_some());
    assert_eq!(ui.composer.lines(), &["new draft"]);
    for current in [true, false] {
        let mut ui = Ui::new(false);
        ui.composer.insert_str("submitted draft");
        crate::deliver(
            &mut ui,
            vec![settled(Outcome::Submission(
                "submitted draft".into(),
                true,
                true,
            ))],
            current,
        );
        assert_eq!(ui.composer.lines(), &[""]);
    }
    Ok(())
}

#[test]
fn review_ledger_navigation_reshapes_every_pending_completion() -> Result {
    let mut results = Results::default();
    let old = session_id("ses_old")?;
    let new = session_id("ses_new")?;
    for view in [commands::View::Status, commands::View::Models] {
        let mut completion = done("old draft", view);
        completion.session = Some(old.clone());
        completion.consume = true;
        completion.replacement = Some("old expansion".into());
        results.submissions.command = Some((CommandId::TEST, completion));
        results.collect();
    }
    let mut reshaped = 0;
    for completion in results.ledger.completions() {
        assert!(!completion.keep_for(Some(&new)));
        reshaped += 1;
    }
    assert_eq!(reshaped, 2);
    let mut ui = Ui::new(false);
    ui.composer.insert_str("old draft");
    crate::deliver(&mut ui, results.ledger.take(), true);
    assert!(ui.usage_pane.is_none() && ui.models.is_none());
    assert_eq!(ui.composer.lines(), &["old draft"]);
    Ok(())
}

#[test]
fn review_ledger_two_followup_refusals_survive_the_occupied_view() -> Result {
    let fixture = Fixture::paused()?;
    let mut ids = Vec::new();
    for ticket in [11, 12] {
        ids.push(
            fixture.command(Command::Followup(crate::followups::Request {
                session: fixture.session.clone(),
                text: format!("task {ticket}"),
                files: vec![],
            }))?,
        );
    }
    let outcomes = fixture.outcomes(2)?;
    assert_eq!(
        outcomes
            .iter()
            .map(|settled| settled.id)
            .collect::<Vec<_>>(),
        ids
    );
    for outcome in &outcomes {
        assert!(matches!(
            outcome.outcome,
            Outcome::Followup(crate::followups::Custody::Refused)
        ));
    }
    Ok(())
}

#[test]
fn review_ledger_submission_and_completion_survive_the_pending_view_in_both_orders() -> Result {
    for submission_first in [true, false] {
        let fixture = Fixture::ready()?;
        let texts = if submission_first {
            ["human task", "/status"]
        } else {
            ["/status", "human task"]
        };
        let mut ids = Vec::new();
        for text in texts {
            ids.push(fixture.command(Command::Submit(text.into()))?);
        }
        let outcomes = fixture.outcomes(2)?;
        assert_eq!(
            outcomes
                .iter()
                .map(|settled| settled.id)
                .collect::<Vec<_>>(),
            ids
        );
        let submission = usize::from(!submission_first);
        let completion = usize::from(submission_first);
        assert!(
            matches!(&outcomes[submission].outcome, Outcome::Submission(text, true, true) if text == "human task"),
            "{outcomes:?}"
        );
        assert!(
            matches!(outcomes[completion].outcome, Outcome::Command(_)),
            "{outcomes:?}"
        );
        let mut ui = Ui::new(false);
        ui.session_update(Some(fixture.session.clone()), false);
        ui.composer.insert_str("human task");
        crate::deliver(&mut ui, outcomes, true);
        assert_eq!(ui.composer.lines(), &[""]);
        assert!(ui.usage_pane.is_some());
    }
    Ok(())
}

#[test]
fn review_ledger_a_stale_view_drops_form_replies_and_still_applies_session_messages() -> Result {
    let mut client = client()?;
    let session = client.selected().ok_or("session")?.clone();
    client
        .peer_mut()
        .raise_form(&session, "Question")
        .ok_or("form")?;
    let request = WireRequest::new(Operation::SessionFormList, Some(&session), None)?;
    let rows = grapt_client::wire::decode_session_forms(&client.peer_mut().call(&request)?.body)?;
    let form = rows.first().ok_or("form row")?.id.clone();
    let mut ui = Ui::new(false);
    ui.forms.update(Some(session.clone()), None, rows);
    let before = ui.forms.draft_state(0).cloned();
    let reply = grapt_tui_forms::Reply {
        target: grapt_tui_forms::Target::Session(session.clone()),
        form,
        state: Some(grapt_client::wire::FormState::Cancelled),
        validation_refused: false,
        note: "Form cancelled".into(),
    };
    let message = crate::sessions::Message::Managed {
        request: crate::sessions::Request::Rename {
            id: session,
            title: "New title".into(),
        },
        result: Err("rename refused".into()),
        refresh_error: None,
    };
    crate::deliver(
        &mut ui,
        vec![
            settled(Outcome::Form(reply)),
            settled(Outcome::Session(message)),
        ],
        false,
    );
    assert_eq!(ui.forms.draft_state(0), before.as_ref());
    assert!(ui.status.contains("rename refused"), "{}", ui.status);
    Ok(())
}

#[test]
fn review_ids_images_ignore_foreign_results_and_keep_retry_ownership() -> Result {
    let handoff = Handoff::default();
    let (owner, _) = handoff.envelope(Command::Submit("image prompt".into()));
    let (other, _) = handoff.envelope(Command::Submit("unrelated prompt".into()));
    let mut images = crate::images::Images::default();
    let image = |name: &str| {
        crate::images::prepared(name.into(), grapt_image::prepare_rgba(2, 2, vec![255; 16])?)
    };
    images.add(image("original.png")?)?;
    images.submission("image prompt".into())?;
    images.sent(owner);
    let revision = images.revision();
    assert!(!images.settle(other, crate::images::Settlement::Accepted));
    assert_eq!(images.revision(), revision);
    assert_eq!(images.len(), 1);
    assert!(!images.settle(owner, crate::images::Settlement::Unconfirmed));
    images.add(image("later.png")?)?;
    assert!(!images.settle(other, crate::images::Settlement::Rejected));
    assert!(images.settle(owner, crate::images::Settlement::Accepted));
    assert_eq!(images.len(), 1);
    assert!(!images.settle(owner, crate::images::Settlement::Accepted));
    let remaining = images.submission("later prompt".into())?;
    assert_eq!(
        serde_json::to_value(&remaining.files)?[0]["name"],
        "later.png"
    );
    assert!(images.settle_local(crate::images::Settlement::Accepted));
    assert_eq!(images.len(), 0);
    Ok(())
}
