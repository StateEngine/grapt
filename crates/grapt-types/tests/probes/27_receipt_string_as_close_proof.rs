// Probe 27 (routing/adapter provenance, provider tool row): a parseable
// open/close receipt is an audit identifier, not proof that an accumulation
// closed.
//
// Law: "conversion requires the adapter's exact open/close receipt". A
// conversion that merely TAKES an `AdapterOpenCloseReceipt` consumes nothing
// unforgeable — the receipt is a string any caller can type, and one issued in
// another scope carries no evidence that it belongs here.
//
// So the accumulation key no longer converts itself. The chain is
// `ProviderStreamScope::open_tool_input -> ProviderToolAccumulation::close ->
// AdapterCloseProof::into_handle`, and the scope is carried by the proof rather
// than supplied at the mint.
use grapt_types::{
    AccountId, AdapterOpenCloseReceipt, PhysicalAttemptId, ProviderId, ProviderToolCallValue,
    ProviderToolInputKey,
};

fn main() {
    let _durable = ProviderToolInputKey::new("acc-1").unwrap().into_handle(
        AdapterOpenCloseReceipt::new("open-close-1").unwrap(),
        ProviderId::new("openai").unwrap(),
        AccountId::new("acct-1").unwrap(),
        PhysicalAttemptId::new("att-1").unwrap(),
        ProviderToolCallValue::new("call_1").unwrap(),
    );
}
