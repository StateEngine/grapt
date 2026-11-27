//! forms witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

#[test]
fn a_pending_form_is_rendered_from_the_real_list_route_and_scoped_to_its_session() {
    // Forms are read through `v2.session.form.list`, decoded as the real
    // `SessionFormListResponseDto`. The scoping is the part worth witnessing:
    // the floor-wide list would attribute another session's question to this
    // transcript, which is the same class of defect as rendering another
    // session's events.
    let mut client = client();
    let Ok(first) = client.create_session() else {
        panic!("create")
    };
    let Ok(second) = client.create_session() else {
        panic!("create")
    };
    let Some(raised) = client
        .peer_mut()
        .raise_form(&first, "which branch should I work on?")
    else {
        panic!("the double must raise a form on a known session")
    };
    let Some(_) = client
        .peer_mut()
        .raise_form(&second, "another session's question")
    else {
        panic!("raise")
    };

    // `second` is selected, so its own question is the one on screen.
    let Ok(()) = client.refresh_forms() else {
        panic!("forms")
    };
    assert_eq!(client.forms().len(), 1);
    assert_eq!(client.forms()[0].title, "another session's question");

    // Selecting the first session picks up ITS pending question, without a
    // separate call: a run waiting on a form must not look like one that
    // simply stopped.
    let Ok(()) = client.select(&first) else {
        panic!("select")
    };
    assert_eq!(client.forms().len(), 1, "one session, one question");
    let form = &client.forms()[0];
    assert_eq!(form.id, raised);
    assert_eq!(form.title, "which branch should I work on?");
    assert_eq!(
        form.fields.len(),
        2,
        "the fields the frozen validator accepted are carried"
    );
    assert_eq!(form.fields[0].key, "branch");
    assert!(form.fields[0].required, "and their requirement is carried");
    assert!(
        !form.fields[1].required,
        "an absent `required` is not required, rather than defaulted to true"
    );
}

#[test]
fn a_form_on_an_unknown_session_is_refused_rather_than_raised() {
    // The producer half. A form raised against a session that does not exist is
    // a question nothing could ever answer, and admitting one would let a
    // witness "prove" form rendering against a floor no client could reach.
    let mut client = client();
    let Ok(known) = client.create_session() else {
        panic!("create")
    };
    let Ok(unknown) = crate::wire::session_id("ses_ffffffffffff") else {
        panic!("id")
    };
    assert!(
        client
            .peer_mut()
            .raise_form(&unknown, "nobody's form")
            .is_none(),
        "an unknown session cannot be given a question"
    );
    assert!(
        client
            .peer_mut()
            .raise_form(&known, "a real question")
            .is_some(),
        "and a known one can"
    );
}

#[test]
fn answering_a_form_settles_it_through_the_real_route_and_takes_it_off_the_list() {
    // The POSITIVE witness. The answer goes out as a real `Form.Reply`, and the
    // settlement is read back from the product's own state route rather than
    // from the client's belief about what it just sent.
    let (mut client, session, form) = a_raised_form();
    assert_eq!(client.forms().len(), 1, "the question is outstanding");
    assert_eq!(
        client.form_state(&session, &form),
        Ok(crate::wire::FormState::Pending),
        "and the product agrees it is pending"
    );

    let answer = an_answer();
    let Ok(()) = client.answer_form(&session, &form, &answer) else {
        panic!("answer")
    };

    assert_eq!(
        client.form_state(&session, &form),
        Ok(crate::wire::FormState::Answered(answer)),
        "the product holds the answer this client sent, value for value"
    );
    assert!(
        client.forms().is_empty(),
        "and a settled question is no longer outstanding"
    );
}

#[test]
fn a_suspended_form_is_still_listed_and_still_answerable() {
    // Suspension is NONTERMINAL. This is the property the durable-blockers
    // spec turns on: a shutdown or a lost process does not answer a question
    // and does not cancel it. A client that treated "not answered" as terminal
    // would strand a form the product still expects an answer for.
    let (mut client, session, form) = a_raised_form();
    assert!(
        client
            .peer_mut()
            .suspend_form(&form, crate::wire::FormSuspension::ProcessLoss),
        "the producer suspends the raised form"
    );

    assert_eq!(
        client.form_state(&session, &form),
        Ok(crate::wire::FormState::Suspended(
            crate::wire::FormSuspension::ProcessLoss
        )),
        "suspended is its own state, carrying WHY"
    );
    let Ok(()) = client.refresh_forms() else {
        panic!("forms")
    };
    assert_eq!(
        client.forms().len(),
        1,
        "a suspended question is still outstanding, or there is no way back to it"
    );

    // And the settlement still lands.
    let answer = an_answer();
    let Ok(()) = client.answer_form(&session, &form, &answer) else {
        panic!("a suspended form must still take an answer")
    };
    assert_eq!(
        client.form_state(&session, &form),
        Ok(crate::wire::FormState::Answered(answer)),
        "answering later settles it truthfully"
    );
}

#[test]
fn a_second_settlement_is_refused_as_too_late_and_does_not_overwrite_the_answer() {
    // The RESURRECTION witness. A settled form is closed. A second answer must
    // be refused as `409` — "you are too late" — and must not silently replace
    // the answer the product already holds, which is how a stale pane could
    // rewrite a decision that was already made.
    let (mut client, session, form) = a_raised_form();
    let answer = an_answer();
    let Ok(()) = client.answer_form(&session, &form, &answer) else {
        panic!("answer")
    };

    let mut different = std::collections::BTreeMap::new();
    different.insert(
        "branch".to_owned(),
        crate::wire::FormValue::Text("some-other-branch".to_owned()),
    );
    assert_eq!(
        client.answer_form(&session, &form, &different),
        Err(ClientError::Status {
            operation: "v2.session.form.reply",
            status: 409,
        }),
        "a second answer is refused as already settled, not as absent"
    );
    assert_eq!(
        client.form_state(&session, &form),
        Ok(crate::wire::FormState::Answered(answer)),
        "and the first answer stands"
    );

    // Cancelling after an answer is the same refusal, not a downgrade.
    assert_eq!(
        client.cancel_form(&session, &form),
        Err(ClientError::Status {
            operation: "v2.session.form.cancel",
            status: 409,
        }),
        "a settled form cannot be cancelled out from under its answer"
    );
}

#[test]
fn cancelling_a_form_settles_it_as_cancelled_rather_than_as_answered() {
    // Cancel is a DECISION with its own terminal state. Folding it into
    // "answered with nothing" would make a declined question indistinguishable
    // from one answered with empty values.
    let (mut client, session, form) = a_raised_form();
    let Ok(()) = client.cancel_form(&session, &form) else {
        panic!("cancel")
    };
    assert_eq!(
        client.form_state(&session, &form),
        Ok(crate::wire::FormState::Cancelled),
        "cancelled is its own state"
    );
    assert!(client.forms().is_empty(), "and it leaves the list");
}

#[test]
fn a_form_is_addressed_within_its_own_session_and_not_across_sessions() {
    // The same scoping the list route already enforces, now on the settlement
    // routes — where getting it wrong would answer a question that belongs to
    // another transcript.
    let mut client = client();
    let Ok(first) = client.create_session() else {
        panic!("create")
    };
    let Ok(second) = client.create_session() else {
        panic!("create")
    };
    let Some(form) = client.peer_mut().raise_form(&first, "first's question") else {
        panic!("raise")
    };
    assert_eq!(
        client.form_state(&second, &form),
        Err(ClientError::Status {
            operation: "v2.session.form.state",
            status: 404,
        }),
        "another session's form is absent here, not readable"
    );
    assert_eq!(
        client.answer_form(&second, &form, &an_answer()),
        Err(ClientError::Status {
            operation: "v2.session.form.reply",
            status: 404,
        }),
        "and it cannot be settled from the wrong session"
    );
}

#[test]
fn a_form_owned_by_something_that_is_not_a_session_is_rendered_rather_than_refused() {
    // `Form.Info.sessionID` is a plain string in the frozen record on purpose:
    // the form routes look session-scoped but use a form-specific location
    // middleware with a `global` sentinel for MCP elicitation forms, which are
    // location-scoped and not session-owned.
    //
    // This client parsed that field as a SessionId first. The structural
    // identity gate is satisfied either way, but that reading REFUSES the
    // sentinel at the wire edge — so a client would have dropped every
    // elicitation form and shown nothing where the product had asked a
    // question. Carrying the owner as what it is renders it instead.
    let Ok(rendered) = crate::wire::decode_form_requests(
        br#"{"location":{"directory":"/work/project-a","project":{"id":"prj_double_a","directory":"/work/project-a"}},"data":[{"id":"frm_000000000001","sessionID":"global","title":"an elicitation","fields":[{"key":"token","type":"string"}]}]}"#,
    ) else {
        panic!("a non-session owner must not refuse the row")
    };
    assert_eq!(rendered.len(), 1, "the question is rendered");
    let form = &rendered[0];
    assert_eq!(form.title, "an elicitation");
    assert_eq!(
        form.owner,
        crate::wire::FormOwner::Global,
        "and its owner is carried as what the product spelled"
    );
    assert_eq!(
        form.owner.session(),
        None,
        "while never being usable as a session identity"
    );
}

#[test]
fn a_permission_can_be_listed_allowed_and_denied() {
    // Deliverable 2. Allow and deny are the same route with different bodies,
    // and settling removes the ask from the outstanding list.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create");
    };
    let first = seed_ask(client.peer_mut(), &id, "write a file");
    let second = seed_ask(client.peer_mut(), &id, "run a command");

    let Ok(()) = client.refresh_permissions() else {
        panic!("permission list")
    };
    assert_eq!(client.permissions().len(), 2);

    let Ok(()) = client.reply_permission(&id, &first, crate::wire::PermissionReplyChoice::Once)
    else {
        panic!("allow")
    };
    assert_eq!(client.permissions().len(), 1, "the allowed ask is settled");

    let Ok(()) = client.reply_permission(&id, &second, crate::wire::PermissionReplyChoice::Reject)
    else {
        panic!("deny")
    };
    assert!(
        client.permissions().is_empty(),
        "the denied ask is settled too"
    );
}

#[test]
fn answering_a_permission_twice_is_refused() {
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create");
    };
    let ask = seed_ask(client.peer_mut(), &id, "write a file");
    let Ok(()) = client.reply_permission(&id, &ask, crate::wire::PermissionReplyChoice::Once)
    else {
        panic!("first answer")
    };
    match client.reply_permission(&id, &ask, crate::wire::PermissionReplyChoice::Reject) {
        Err(ClientError::Status { status, .. }) => assert_eq!(status, 404),
        other => panic!("a second answer must be refused, got {other:?}"),
    }
}

#[test]
fn form_field_capabilities_survive_the_wire_projection_and_roundtrip(
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = br#"{"data":[{"id":"frm_capabilities","sessionID":"ses_capabilities","title":"Branch","fields":[{"key":"branch","type":"string","description":"Choose an offered branch","options":[{"value":"main","label":"Main"}],"when":[{"key":"enabled","op":"eq","value":true}]}]}]}"#;
    let rows = crate::wire::decode_session_forms(bytes)?;
    let field = &rows[0].fields[0];
    assert_eq!(
        field.attributes.get("description"),
        Some(&serde_json::json!("Choose an offered branch"))
    );
    assert!(field.attributes.contains_key("options"));
    assert!(field.attributes.contains_key("when"));
    assert_eq!(
        crate::wire::decode_session_forms(&crate::wire::encode_session_forms(&rows)?)?,
        rows
    );
    Ok(())
}

#[test]
fn location_and_message_survive_projection_without_promoting_unknown_owners(
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = br#"{"location":{"directory":"/work/a","project":{"id":"prj_location","directory":"/work/a"}},"data":[{"id":"frm_global","sessionID":"global","title":"MCP","metadata":{"message":"Choose a branch","server":"ask"},"fields":[{"key":"branch","type":"string"}]},{"id":"frm_unknown","sessionID":"future-owner","title":"Unknown","fields":[{"key":"branch","type":"string"}]}]}"#;
    let (location, rows) = crate::wire::decode_location_forms(bytes)?;
    assert_eq!(location.directory, "/work/a");
    assert_eq!(location.project, crate::wire::project_id("prj_location")?);
    assert_eq!(rows[0].owner, crate::wire::FormOwner::Global);
    assert_eq!(
        rows[0].metadata.get("message"),
        Some(&serde_json::json!("Choose a branch"))
    );
    assert_eq!(
        rows[1].owner,
        crate::wire::FormOwner::Unattributed("future-owner".into())
    );
    for row in rows {
        assert_eq!(
            crate::wire::decode_form(&crate::wire::encode_form(&row)?)?,
            row
        );
    }
    Ok(())
}

#[test]
fn canonical_form_validation_refusals_keep_their_identity_and_message(
) -> Result<(), Box<dyn std::error::Error>> {
    let client = client();
    let body = br#"{"_tag":"FormInvalidAnswerError","id":"frm_validation","message":"Expected email for form field: email"}"#.to_vec();
    for operation in [Operation::SessionFormReply, Operation::GlobalFormReply] {
        let error = client.accept_response(
            operation,
            crate::WireResponse {
                status: 400,
                body: body.clone(),
            },
        );
        assert!(
            matches!(error, Err(ClientError::FormInvalidAnswer { form, message, .. }) if form == crate::wire::form_id("frm_validation")? && message == "Expected email for form field: email")
        );
    }
    assert!(matches!(
        client.accept_response(
            Operation::SessionPrompt,
            crate::WireResponse { status: 400, body }
        ),
        Err(ClientError::Status { status: 400, .. })
    ));
    Ok(())
}
