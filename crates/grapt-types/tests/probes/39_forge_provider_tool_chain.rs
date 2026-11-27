// Probe 39: the COMPLETE forge chain to a durable provider tool handle.
//
// Probes 27, 28 and 29 each proved one link: the accumulation key is not a
// close proof, the close proof's fields are private, and a handle cannot be
// re-scoped. Every link consumed its predecessor, so the chain LOOKED sealed.
// Its first link rested on nothing.
//
// Author a provider, an account and a physical attempt id — all opaque ids with
// public constructors — enter a stream scope nobody granted, open an
// accumulation under any key, close it with an authored
// `AdapterOpenCloseReceipt`, and mint a durable handle. No adapter fact appears
// anywhere in it.
//
// `ProviderStreamScope::enter` is now crate-private: the chain has no origin.
use grapt_types::{
    AccountId, AdapterOpenCloseReceipt, PhysicalAttemptId, ProviderId, ProviderStreamScope,
    ProviderToolCallValue, ProviderToolInputKey,
};

fn main() {
    let scope = ProviderStreamScope::enter(
        ProviderId::new("openai").expect("provider"),
        AccountId::new("acct-invented").expect("account"),
        PhysicalAttemptId::new("att-invented").expect("attempt"),
    );

    let accumulation = scope.open_tool_input(ProviderToolInputKey::new("key").expect("key"));
    let proof = accumulation.close(
        AdapterOpenCloseReceipt::new("receipt-invented").expect("receipt"),
    );
    let _durable = proof.into_handle(ProviderToolCallValue::new("call_1").expect("value"));
}
