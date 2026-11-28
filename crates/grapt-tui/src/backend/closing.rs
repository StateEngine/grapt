//! Commands a quit discarded are refused, each with the settlement its kind
//! gives any refusal, so no draft, picker or form waits on them if the
//! terminal stays open.
use super::{commands::Completion, view::Ledger, Command, CommandId, Outcome};
use crate::bridge::Bridge;
use grapt_client::{wire::AgentRow, Client};

const NOT_SENT: &str = "Closing; not sent";

/// Where the protocol loop keeps what a view settles.
pub(super) struct Holders<'a> {
    pub(super) ledger: &'a mut Ledger,
    /// The newest Select a view answers.
    pub(super) selection: &'a mut Option<CommandId>,
    /// Commands that could have moved the page, discarded.
    pub(super) navigation_dropped: &'a mut Vec<CommandId>,
    pub(super) roster: &'a mut Option<Vec<AgentRow>>,
}

/// Settle every discarded command. A Select is answered as the view's
/// selection, never replacing a newer one; an agent choice by offering the
/// roster again. Any command that could have moved the page is listed as
/// dropped, which settles its page request and leaves the view where it is.
pub(super) fn refuse(
    discarded: Vec<(CommandId, Command)>,
    client: &Client<Bridge>,
    holders: Holders<'_>,
) {
    let Holders {
        ledger,
        selection,
        navigation_dropped,
        roster,
    } = holders;
    for (id, command) in discarded {
        if matches!(
            command,
            Command::Browse(_)
                | Command::Submit(_)
                | Command::SubmitImages(_)
                | Command::StartupPrompt(_)
                | Command::Retry(_)
        ) {
            navigation_dropped.push(id);
        }
        let outcome = match command {
            Command::Submit(text) | Command::Retry(text) => Outcome::Submission(text, false, true),
            Command::StartupPrompt(text) => Outcome::Submission(text, false, false),
            Command::SubmitImages(prompt) => {
                ledger.settle(id, Outcome::Image(crate::images::Settlement::Rejected));
                Outcome::Submission(prompt.text, false, true)
            }
            Command::Followup(_) => Outcome::Followup(crate::followups::Custody::Refused),
            Command::Form(request) => Outcome::Form(grapt_tui_forms::Reply {
                target: request.target,
                form: request.form.id,
                state: None,
                validation_refused: false,
                note: NOT_SENT.into(),
            }),
            Command::Model(_) => Outcome::Model(false),
            Command::SessionSearch(query) => {
                Outcome::Session(crate::sessions::Message::Search(crate::sessions::Search {
                    query,
                    result: Err(NOT_SENT.into()),
                    reach: grapt_client::CatalogReach::Read,
                }))
            }
            Command::ManageSession(request) => {
                Outcome::Session(crate::sessions::Message::Managed {
                    request,
                    result: Err(NOT_SENT.into()),
                    refresh_error: None,
                })
            }
            Command::RevokeGrant(request) => {
                let mut done = Completion::new("");
                done.session = Some(request.session);
                done.notice = "Closing; no consent was withdrawn.".into();
                Outcome::Command(done)
            }
            Command::Select(_) => {
                *selection = (*selection).max(Some(id));
                continue;
            }
            Command::ChooseAgent(_) => {
                *roster = Some(client.agents().to_vec());
                continue;
            }
            // Nothing waits on these: a permission ask stays open, and the
            // rest only refresh or show what the backend already has.
            Command::Reply(..)
            | Command::Browse(_)
            | Command::Interrupt(_)
            | Command::InterruptAndResume(_)
            | Command::InterruptSession(_)
            | Command::Quit(_)
            | Command::StartupRead
            | Command::Models
            | Command::RefreshModels
            | Command::Commands
            | Command::Roster => continue,
        };
        ledger.settle(id, outcome);
    }
}
