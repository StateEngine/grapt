//! A service restart continues work; closing its owner never grants that right.
use super::*;

#[path = "restart_resume/fixture.rs"]
mod fixture;
use fixture::{Journey, Lifetime};

#[test]
fn closing_a_running_window_keeps_its_turn_stopped_after_rebinding() {
    let _journey = process_journey_custody();
    let mut journey = Journey::start("restart-closed-turn", Lifetime::Terminal);
    journey.begin();
    journey.close();
    journey.reopen();
    journey.bind();
    journey.assert_calls_stay(1);
    journey.follow_up();
    journey.await_answer(2);
}

#[test]
fn a_legacy_shutdown_terminal_alone_does_not_authorize_continuation() {
    let _journey = process_journey_custody();
    let mut journey = Journey::start("restart-legacy-turn", Lifetime::Terminal);
    journey.begin();
    journey.close();
    // Both old service stops and terminal closes persisted this same Shutdown
    // execution vocabulary. It cannot distinguish authority to resume.
    journey.assert_shutdown_terminal_without_resume_notice();
    journey.reopen();
    journey.bind();
    journey.assert_calls_stay(1);
    journey.follow_up();
    journey.await_answer(2);
}

#[cfg(unix)]
#[test]
fn a_later_completed_followup_retires_the_older_service_interruption() {
    let _journey = process_journey_custody();
    let mut journey = Journey::start("restart-settled-turn", Lifetime::Service);
    journey.begin();
    journey.stop_service();
    journey.reopen();
    // Explicit work after re-provisioning supersedes the interrupted turn.
    journey.bind();
    journey.follow_up();
    journey.await_answer(2);
    journey.bind();
    journey.assert_calls_stay(2);
}

#[cfg(unix)]
#[test]
fn a_later_window_close_retires_the_older_service_interruption() {
    let _journey = process_journey_custody();
    let mut journey = Journey::start_with_held_followup("restart-later-close");
    journey.begin();
    journey.stop_service();
    journey.reopen();
    journey.bind();
    journey.follow_up();
    journey.await_held_followup();
    journey.close();
    journey.reopen();
    journey.bind();
    journey.assert_calls_stay(2);
}

#[cfg(unix)]
#[test]
fn a_service_stop_is_preserved_as_one_history_notice_after_restart() {
    let _journey = process_journey_custody();
    let mut journey = Journey::start("restart-service-notice", Lifetime::Service);
    journey.begin();
    journey.stop_service();
    journey.reopen();
    journey.bind();
    journey.assert_calls_stay(1);
    journey.assert_stop_notice();
    journey.bind();
    journey.assert_stop_notice();
    journey.follow_up();
    journey.await_answer(2);
    journey.assert_stop_notice();
}

#[test]
fn an_expired_service_window_is_preserved_as_one_history_notice_after_restart() {
    let _journey = process_journey_custody();
    let mut journey = Journey::start("restart-window-notice", Lifetime::Window);
    journey.begin();
    journey.await_exit();
    journey.reopen();
    journey.bind();
    journey.assert_calls_stay(1);
    journey.assert_stop_notice();
    journey.follow_up();
    journey.await_answer(2);
    journey.assert_stop_notice();
}
