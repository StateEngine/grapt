// A consumer may not put a notice into an open transaction.
//
// The second half of the same law as probe 02. Blocking only the finished
// carrier would leave the other end open: a consumer that could push into
// `GovernedTransaction` would have its notice released by the next ordinary
// commit, naming a fact that commit never wrote.
//
// `push_notice` is therefore crate-private, and the append path is its only
// caller. The compiler must reject this; there is exactly one error to reach.
use grapt_store::GovernedTransaction;

fn a_guard() -> GovernedTransaction<'static> {
    loop {}
}

fn main() {
    let tx = a_guard();
    tx.push_notice(loop {});
}
