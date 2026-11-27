use super::*;
use grapt_client::{wire::session_id, SessionSummary};
use ratatui::{backend::TestBackend, Terminal};

fn agents(children: usize, status: &str) -> Result<Vec<Agent>, Box<dyn std::error::Error>> {
    let parent = session_id("ses_main")?;
    let member = |id: &str, parent: Option<SessionWireId>, created, current| {
        Ok::<_, Box<dyn std::error::Error>>(Agent {
            display_name: None,
            context: Default::default(),
            row: SessionSummary {
                id: session_id(id)?,
                title: format!("task of {id}"),
                parent,
                detail: grapt_client::wire::SessionDetail {
                    agent: Some("explore".into()),
                    model: Some("anthropic/claude#high".into()),
                    context_tokens: Some(42),
                    created_ms: Some(created),
                    ..Default::default()
                },
            },
            status: status.into(),
            current,
        })
    };
    // Reported newest-first; the switcher must still list spawn order.
    let mut rows = Vec::new();
    for index in (0..children).rev() {
        rows.push(member(
            &format!("ses_c{index}"),
            Some(parent.clone()),
            index as u64,
            false,
        )?);
    }
    rows.push(member("ses_main", None, 0, true)?);
    Ok(rows)
}

#[test]
fn family_keeps_active_and_unseen_failures_but_collapses_finished_children(
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = agents(9, "idle")?;
    let ids: Vec<_> = family(&rows).iter().map(|a| a.row.title.clone()).collect();
    assert_eq!(
        ids[..3],
        ["task of ses_main", "task of ses_c0", "task of ses_c1"]
    );
    let mut switcher = Switcher::default();
    let now = tokio::time::Instant::now();
    switcher.observe(&rows, now);
    assert!(switcher.height(&rows, 40) <= MAX_ROWS as u16 + 1);
    assert_eq!(
        switcher.members(&rows).len(),
        10,
        "idle alone does not prove completion"
    );
    let mut changed = agents(9, "completed")?;
    for (agent, status) in changed.iter_mut().zip([
        "running",
        "unreported",
        "waiting for permission",
        "waiting for question",
        "failed",
        "interrupted",
        "succeeded",
        "completed",
        "completed",
    ]) {
        agent.status = status.into();
    }
    switcher.observe(&changed, now);
    assert_eq!(switcher.members(&changed).len(), 6);
    assert!(switcher.tick(now + Duration::from_secs(1)));
    for agent in &mut changed {
        agent.current = agent.status == "failed";
    }
    switcher.observe(&changed, now);
    assert_eq!(
        switcher.members(&changed).len(),
        5,
        "viewing the failure acknowledges it"
    );
    for agent in &mut changed {
        agent.current = agent.row.parent.is_none();
    }
    let mut away = changed.clone();
    for agent in &mut away {
        agent.current = false;
    }
    let mut other = agents(0, "idle")?.pop().ok_or("other parent")?;
    other.row.id = session_id("ses_other_parent")?;
    away.push(other);
    switcher.observe(&away, now);
    switcher.observe(&changed, now);
    assert_eq!(
        switcher.members(&changed).len(),
        5,
        "returning to the family must not resurrect an acknowledged failure"
    );
    changed[4].row.detail.updated_ms = Some(1);
    changed[4].current = false;
    switcher.observe(&changed, now);
    assert_eq!(
        switcher.members(&changed).len(),
        6,
        "a newer failure needs another view"
    );
    for agent in &mut changed {
        agent.status = "completed".into();
    }
    switcher.observe(&changed, now);
    assert_eq!(switcher.height(&changed, 40), 0);
    assert!(!switcher.ticking());
    assert_eq!(
        changed.len(),
        10,
        "saved sessions remain available outside the panel"
    );
    Ok(())
}

#[test]
fn clicking_a_row_below_the_composer_views_that_session_and_long_families_scroll(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ui = crate::ui::Ui::new(false);
    ui.agents = agents(9, "running")?;
    ui.switcher.observe(&ui.agents, tokio::time::Instant::now());
    let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
    terminal.draw(|frame| ui.draw(frame))?;
    let area = ui.switcher.area;
    assert_eq!(area.width, 100);
    assert!(area.y > ui.composer_area.y);
    assert!(ui.switcher.click(&ui.agents, area.x + 3, area.y + 1));
    let first = session_id("ses_c0")?;
    assert!(matches!(ui.switcher.action.take(), Some(Action::View(id)) if id == first));
    assert_eq!(
        ui.switcher.focus,
        Some(1),
        "a mouse selection must retain switcher key focus so Esc returns to main"
    );
    // The wheel scrolls to the end while main stays viewed, and repainting keeps it there.
    for _ in 0..5 {
        assert!(ui.switcher.scroll(area.x, area.y, -3));
    }
    terminal.draw(|frame| ui.draw(frame))?;
    assert!(ui
        .switcher
        .click(&ui.agents, area.x, area.y + area.height - 1));
    let last = session_id("ses_c8")?;
    assert!(matches!(ui.switcher.action.take(), Some(Action::View(id)) if id == last));
    // Viewing a completed child must preserve the header's return-to-main click.
    for agent in &mut ui.agents {
        agent.current = agent.row.id == last;
        if agent.current {
            agent.status = "completed".into();
        }
    }
    ui.switcher.observe(&ui.agents, tokio::time::Instant::now());
    terminal.draw(|frame| ui.draw(frame))?;
    let area = ui.switcher.area;
    assert!(ui.switcher.click(&ui.agents, area.x, area.y - 1));
    assert!(
        matches!(ui.switcher.action.take(), Some(Action::View(id)) if id == session_id("ses_main")?)
    );
    Ok(())
}

#[test]
fn escape_from_a_child_without_switcher_focus_only_returns_to_main(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ui = crate::ui::Ui::new(false);
    ui.agents = agents(1, "running")?;
    for agent in &mut ui.agents {
        agent.current = agent.row.parent.is_some();
    }
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = std::sync::mpsc::sync_channel(1);
    let backend = crate::backend::Backend {
        commands,
        updates,
        wake,
    };
    assert!(key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend
    ));
    assert!(
        matches!(received.try_recv()?, Command::Select(request) if request.id == session_id("ses_main")?)
    );
    assert!(received.try_recv().is_err());
    // The return target can itself be a child; a completed viewed child need not occupy the panel.
    ui.agents = agents(1, "completed")?;
    for agent in &mut ui.agents {
        agent.current = agent.row.parent.is_some();
        if !agent.current {
            agent.row.parent = Some(session_id("ses_outer")?);
            agent.row.detail.created_ms = Some(100);
        }
    }
    let now = tokio::time::Instant::now();
    ui.switcher.observe(&ui.agents, now);
    assert_eq!(ui.switcher.height(&ui.agents, 40), 0);
    assert!(key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend
    ));
    assert!(
        matches!(received.try_recv()?, Command::Select(request) if request.id == session_id("ses_main")?)
    );
    assert!(received.try_recv().is_err());
    Ok(())
}
