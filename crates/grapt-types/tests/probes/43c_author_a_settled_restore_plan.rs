// Probe 43c (row 12, second clause): a settled restore plan cannot be assembled
// field by field.
//
// Probe 43 seals the mint. This one seals the other way in: a struct literal
// naming every coordinate would be an alternate mutation entry point that
// bypasses policy and receipt, which amendment §8.3 forbids outright. It would
// also let a caller pin a capability of its choosing into the plan, which is why
// `required_capability` is derived inside the mint from the plan's own resolved
// coordinates and is not a field a caller ever supplies.
//
// Error phase: privacy (private fields), resolved before type checking.
use grapt_types::SettledRestorePlan;

fn author() -> SettledRestorePlan {
    SettledRestorePlan {
        plan: todo!(),
        operation: todo!(),
        checkpoint: todo!(),
        root: todo!(),
        lanes: todo!(),
        consequences: todo!(),
        unusual: todo!(),
        preconditions: todo!(),
        enforcement: todo!(),
        required_capability: todo!(),
        settled_at: todo!(),
        expires_at: todo!(),
    }
}

fn main() {
    let _ = author;
}
