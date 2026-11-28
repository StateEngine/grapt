//! Commands a quit discarded are refused, each with the settlement its kind
//! gives any refusal, so no draft, picker or form waits on them if the
//! terminal stays open.
use super::{commands::Completion, view::Ledger, Command, CommandId, Outcome};

const NOT_SENT: &str = "Closing; not sent";

/// Settle every discarded command; a discarded Select is answered as the
/// view's selection.
pub(super) fn refuse(
    discarded: Vec<(CommandId, Command)>,
    ledger: &mut Ledger,
    selection: &mut Option<CommandId>,
) {
    for (id, command) in discarded {
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
                *selection = Some(id);
                continue;
            }
            // Nothing waits on these: a permission ask stays open, and the
            // rest only refresh or show what the backend already has.
            Command::Reply(..)
            | Command::Interrupt(_)
            | Command::InterruptAndResume(_)
            | Command::InterruptSession(_)
            | Command::Quit(_)
            | Command::StartupRead
            | Command::Models
            | Command::RefreshModels
            | Command::Commands
            | Command::Roster
            | Command::ChooseAgent(_)
            | Command::Browse(_) => continue,
        };
        ledger.settle(id, outcome);
    }
}
