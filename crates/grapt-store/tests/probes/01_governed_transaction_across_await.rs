// A database guard may not survive an `.await`.
//
// The sync/async constitution states it as law ("No database transaction, mutex
// guard, or domain borrow survives an `.await`"), and F3B1b makes it a property
// of the type instead of a review question: `GovernedTransaction` borrows a
// `Connection`, `Connection` is not `Sync`, so the guard is not `Send` and any
// future holding one across a suspension point is not `Send` either.
//
// The compiler must reject this. There is exactly one error to reach: the async
// block is the only thing here required to be `Send`.
use grapt_store::GovernedTransaction;

fn requires_send_future<F: std::future::Future + Send>(_: F) {}

fn a_guard() -> GovernedTransaction<'static> {
    loop {}
}

async fn suspend() {}

fn main() {
    requires_send_future(async {
        let guard = a_guard();
        suspend().await;
        drop(guard);
    });
}
