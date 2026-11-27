// A live notice may not be manufactured beside the commit that earns it.
//
// `CommittedNotices` is the carrier that lets a consumer publish, and holding
// one is supposed to BE the proof that the named facts are durable. That proof
// is worth nothing if a consumer can assemble the carrier itself, so the fields
// are private and there is no public constructor, no `Default` and no `From`.
//
// The compiler must reject this. There is exactly one error to reach: the
// struct literal names fields the consumer cannot see.
use grapt_store::CommittedNotices;

fn main() {
    let forged = CommittedNotices {
        notices: Vec::new(),
        dropped: 0,
    };
    forged.discard();
}
