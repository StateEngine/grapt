//! Provider-counter progress and its delayed-progress regression.
use super::*;

pub(super) fn wait_calls(calls: &AtomicUsize, wanted: usize) {
    let wait = PollDeadline::new();
    loop {
        let observed = calls.load(Ordering::SeqCst);
        if observed >= wanted {
            return;
        }
        assert!(
            wait.pending(),
            "provider did not reach call {wanted}; observed {observed}, {wait}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn provider_call_wait_allows_progress_after_the_former_short_budget() {
    let calls = AtomicUsize::new(0);
    thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_secs(3));
            calls.store(1, Ordering::SeqCst);
        });
        wait_calls(&calls, 1);
    });
}
