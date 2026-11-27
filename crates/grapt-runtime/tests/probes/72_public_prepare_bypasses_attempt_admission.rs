// Probe 72: no downstream crate may invoke the crate-private protocol opener.

use grapt_runtime::{
    attempt_admission::AttemptProtocol, AttemptOwner, ProviderReader, ToolRunner,
};

fn bypass<R, T, O>()
where
    R: ProviderReader,
    T: ToolRunner,
    O: AttemptOwner,
{
    let _ = AttemptProtocol::prepare::<R, T, O>;
}

fn main() {
}
