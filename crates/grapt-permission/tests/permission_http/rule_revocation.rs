use super::*;
use grapt_permission::{
    PermissionRevokeRuleCommand, PermissionRuleWithdrawal, SavedPermissionRule,
};

fn ask(
    store: &mut EventStore,
    state: &mut PermissionHttpState,
    owner: &PermissionOwner<'_>,
    session: &SessionId,
    suffix: &str,
) -> TestResult {
    let id: PermissionId = format!("per_grant_{suffix}").parse()?;
    let layers = [grapt_permission::preset::Layer {
        name: grapt_permission::preset::LayerName::Owner,
        floor: true,
        compose: grapt_permission::preset::Compose::Narrow,
        rules: vec![grapt_permission::preset::Rule::new(
            "write",
            "*",
            grapt_permission::preset::Effect::Ask,
        )],
    }];
    assert_eq!(
        state.create(
            store,
            owner,
            &event(&format!("evt_ask_{suffix}"))?,
            &id,
            session,
            &agent()?,
            &layers,
            &project()?,
            "write",
            &["/outside/literal".into()],
            Some(&["/outside/literal".into()]),
            None,
            None,
            0
        )?,
        grapt_permission::PermissionCreateEffect::Ask
    );
    Ok(())
}

fn approve(
    store: &mut EventStore,
    state: &mut PermissionHttpState,
    owner: &PermissionOwner<'_>,
    session: &SessionId,
    suffix: &str,
) -> TestResult {
    let id: PermissionId = format!("per_grant_{suffix}").parse()?;
    state.reply(
        store,
        PermissionReplyCommand {
            owner: owner.clone(),
            session,
            id: &id,
            reply: PermissionHttpReply::Always,
            project: Some(&project()?),
            created_ms: 0,
            decision: decision(suffix)?,
        },
        || Err(PermissionHttpError::Identity("unexpected cascade".into())),
    )?;
    Ok(())
}

#[test]
fn withdrawal_survives_reordered_session_replay_and_preserves_a_new_grant() -> TestResult {
    let root = std::env::temp_dir().join(format!("grapt-rule-withdrawal-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let client = AuthenticatedClientId::new("withdrawal-client").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human",
        client: &client,
    };
    let orders = [
        ["ses_a_old", "ses_z_revoke", "ses_m_fresh"],
        ["ses_z_old", "ses_a_revoke", "ses_m_fresh"],
        ["ses_z_old", "ses_m_revoke", "ses_a_fresh"],
    ];
    for (index, names) in orders.iter().enumerate() {
        for fresh in [false, true] {
            let database = root.join(format!("{index}-{fresh}.db"));
            let old: SessionId = names[0].parse()?;
            let revoke: SessionId = names[1].parse()?;
            let renewed: SessionId = names[2].parse()?;
            let mut store = store_at(Some(&database), &old)?;
            for session in [&revoke, &renewed] {
                store.elect_writer(&AggregateId::Session(session.clone()))?;
            }
            let mut state = PermissionHttpState::default();
            // Both asks predate remembered consent; otherwise the second
            // create is already allowed and cannot produce another origin.
            ask(&mut store, &mut state, &owner, &old, "old_first")?;
            ask(&mut store, &mut state, &owner, &old, "old_second")?;
            approve(&mut store, &mut state, &owner, &old, "old_first")?;
            approve(&mut store, &mut state, &owner, &old, "old_second")?;
            let rule = SavedPermissionRule {
                project: project()?,
                action: "write".into(),
                resource: "/outside/literal".into(),
            };
            assert!(state
                .revoke_saved_rule(
                    &mut store,
                    PermissionRevokeRuleCommand {
                        owner: owner.clone(),
                        session: &revoke,
                        rule: &rule,
                        event: &event("evt_ask_old_first")?,
                        created_ms: 0,
                    }
                )
                .is_err());
            assert_eq!(
                PermissionHttpState::saved_rules(&store, Some(&project()?))?,
                vec![rule.clone()],
                "failed canonical append must preserve the active grant"
            );
            assert_eq!(
                state.revoke_saved_rule(
                    &mut store,
                    PermissionRevokeRuleCommand {
                        owner: owner.clone(),
                        session: &revoke,
                        rule: &rule,
                        event: &event("evt_withdraw")?,
                        created_ms: 0,
                    }
                )?,
                PermissionRuleWithdrawal::Complete
            );
            assert!(PermissionHttpState::saved_rules(&store, Some(&project()?))?.is_empty());
            if fresh {
                ask(&mut store, &mut state, &owner, &renewed, "fresh")?;
                approve(&mut store, &mut state, &owner, &renewed, "fresh")?;
            }
            drop(store);
            let log = EventLog::open_read_only(&database)?;
            let mut rebuilt = EventStore::open_in_memory()?;
            for _ in 0..2 {
                PermissionHttpState::rebuild_from_event_log(&mut rebuilt, &log)?;
                let rules = PermissionHttpState::saved_rules(&rebuilt, Some(&project()?))?;
                assert_eq!(rules, if fresh { vec![rule.clone()] } else { vec![] });
            }
        }
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn existing_saved_consent_migrates_to_revocable_origins_and_refuses_corrupt_storage() -> TestResult
{
    let root = std::env::temp_dir().join(format!("grapt-rule-migrate-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let database = root.join("existing.db");
    let session: SessionId = "ses_existing_rule".parse()?;
    let client = AuthenticatedClientId::new("migration-client").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human",
        client: &client,
    };
    let mut store = store_at(Some(&database), &session)?;
    let mut state = PermissionHttpState::default();
    // Legacy installations could accumulate more origins than fit in one
    // bounded canonical withdrawal event. These asks predate all approvals.
    for index in 0..129 {
        ask(
            &mut store,
            &mut state,
            &owner,
            &session,
            &format!("existing_{index}"),
        )?;
    }
    for index in 0..129 {
        if index == 128 {
            // The new command owner caps new origin growth. Legacy commands
            // had no origin index; clear this derived index while seeding the
            // last old decision, retaining every canonical ask and reply.
            drop(store);
            let legacy = rusqlite::Connection::open(&database)?;
            legacy.execute("DELETE FROM permission_http_rule_origin", [])?;
            drop(legacy);
            store = store_at(Some(&database), &session)?;
        }
        approve(
            &mut store,
            &mut state,
            &owner,
            &session,
            &format!("existing_{index}"),
        )?;
    }
    let rule = PermissionHttpState::saved_rules(&store, Some(&project()?))?
        .into_iter()
        .next()
        .ok_or("saved consent absent")?;
    drop(store);
    // Model the installed schema before this additive migration. Its actual
    // durable ask, reply and saved consent remain intact.
    let connection = rusqlite::Connection::open(&database)?;
    connection.execute_batch(
        "DROP TABLE permission_http_rule_origin; DROP TABLE permission_http_rule_revocation;",
    )?;
    drop(connection);
    EventStore::migrate(&database, &root.join("backups"), "0.0.1")?;
    let mut migrated = store_at(Some(&database), &session)?;
    assert_eq!(
        PermissionHttpState::default().revoke_saved_rule(
            &mut migrated,
            PermissionRevokeRuleCommand {
                owner: owner.clone(),
                session: &session,
                rule: &rule,
                event: &event("evt_withdraw_migrated")?,
                created_ms: 0
            }
        )?,
        PermissionRuleWithdrawal::Pending
    );
    assert_eq!(
        PermissionHttpState::saved_rules(&migrated, Some(&project()?))?,
        vec![rule.clone()]
    );
    assert_eq!(
        PermissionHttpState::default().revoke_saved_rule(
            &mut migrated,
            PermissionRevokeRuleCommand {
                owner: owner.clone(),
                session: &session,
                rule: &rule,
                event: &event("evt_withdraw_migrated_final")?,
                created_ms: 0,
            }
        )?,
        PermissionRuleWithdrawal::Complete
    );
    assert!(PermissionHttpState::saved_rules(&migrated, Some(&project()?))?.is_empty());
    let log = EventLog::open_read_only(&database)?;
    let mut rebuilt = EventStore::open_in_memory()?;
    PermissionHttpState::rebuild_from_event_log(&mut rebuilt, &log)?;
    assert!(PermissionHttpState::saved_rules(&rebuilt, Some(&project()?))?.is_empty());
    drop(log);
    drop(migrated);
    for corrupt in [
        "CREATE TABLE permission_http_rule_origin(project_id TEXT)",
        "CREATE TABLE permission_http_rule_origin(project_id TEXT NOT NULL, action TEXT NOT NULL, resource TEXT COLLATE NOCASE NOT NULL, permission_id TEXT NOT NULL, PRIMARY KEY(project_id,action,resource,permission_id))",
    ] {
        let connection = rusqlite::Connection::open(&database)?;
        connection.execute_batch("DROP TABLE permission_http_rule_origin")?;
        connection.execute_batch(corrupt)?;
        drop(connection);
        assert!(EventStore::open(&database, "0.0.1").is_err(), "corrupt origin storage was accepted");
    }
    let connection = rusqlite::Connection::open(&database)?;
    let remaining: i64 =
        connection.query_row("SELECT COUNT(*) FROM permission_http_rule", [], |row| {
            row.get(0)
        })?;
    assert_eq!(remaining, 0, "failed open resurrected revoked consent");
    drop(connection);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
