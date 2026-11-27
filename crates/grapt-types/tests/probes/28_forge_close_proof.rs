// Probe 28 (routing/adapter provenance, provider tool row): the adapter's
// close proof is minted by the adapter's own lifecycle, never assembled.
//
// With `ProviderToolInputKey::into_handle` gone, the next shortest path is to
// forge the proof `AdapterCloseProof::into_handle` consumes — and to bind it to
// whatever provider, account and attempt the attacker wants the handle to claim.
//
// The fields are private and the only producer is
// `ProviderToolAccumulation::close`, which consumes an accumulation that a
// `ProviderStreamScope` actually opened.
use grapt_types::AdapterCloseProof;

fn main() {
    let _forged = AdapterCloseProof {
        receipt: todo!(),
        key: todo!(),
        provider: todo!(),
        account: todo!(),
        attempt: todo!(),
    };
}
