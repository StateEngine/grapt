//! Whether the selected Session is running, from the newest fact the server
//! ordered.
use grapt_client::{
    projection::{supersedes, FactStamp},
    Client, Connection, ExecutionState,
};

/// The selected transcript owns activity even outside the bounded picker page.
///
/// The view's quiescence, the view's live terminal and the picker's
/// observation each say running or not. The fact the server ordered last
/// wins in either direction: a newer running edge beats an older terminal,
/// and a newer terminal beats a stale busy fact. At an equal position the
/// settled fact wins. Facts the server never ordered against each other keep
/// the old precedence: the view's quiescence, then its execution, then the
/// picker.
pub(super) fn selected_activity<C: Connection>(client: &Client<C>) -> Option<bool> {
    let view = client.view();
    let quiescence = view.and_then(|view| Some((!view.idle?, view.idle_stamp())));
    let execution = view.and_then(|view| match &view.execution {
        ExecutionState::Running { .. } => Some((true, FactStamp::default())),
        ExecutionState::Settled { .. } => Some((false, view.terminal_stamp())),
        ExecutionState::Idle => None,
    });
    let observed = client
        .selected()
        .and_then(|id| client.observed_activity_stamped(id))
        .map(|(activity, stamp)| (activity.is_busy(), stamp));
    let mut facts = [quiescence, execution, observed].into_iter().flatten();
    let first = facts.next()?;
    let (running, _) = facts.fold(first, |best, fact| {
        if supersedes(fact, best) == Some(true) {
            fact
        } else {
            best
        }
    });
    Some(running)
}

/// Routing and the activity row use the same selected-session facts.
pub(super) fn selected_running<C: Connection>(client: &Client<C>) -> bool {
    selected_activity(client).unwrap_or(false)
}
