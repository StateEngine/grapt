// Probe 29 (routing/adapter provenance, provider tool row): a handle cannot be
// re-scoped away from the proof that minted it.
//
// Law: "provider handles from different provider/account/attempt scopes cannot
// collide". A proof closed in attempt `att-1` must not produce a handle for
// `att-2`.
//
// The first half of that refusal is not probeable, because it is unnameable:
// `AdapterCloseProof::into_handle(self, value)` takes no scope arguments at
// all, so there is no parameter through which another attempt could be
// supplied. This file probes the second half — that the attacker cannot simply
// author the handle instead, copying a real handle's value and receipt while
// swapping the attempt.
use grapt_types::{
    AccountId, AdapterOpenCloseReceipt, PhysicalAttemptId, ProviderId, ProviderToolCallValue,
    ProviderToolHandle,
};

fn main() {
    let _rescoped = ProviderToolHandle {
        provider: ProviderId::new("openai").unwrap(),
        account: AccountId::new("acct-1").unwrap(),
        // The lie: a handle opened under `att-1`, claimed for another attempt.
        attempt: PhysicalAttemptId::new("att-2").unwrap(),
        value: ProviderToolCallValue::new("call_1").unwrap(),
        receipt: AdapterOpenCloseReceipt::new("open-close-1").unwrap(),
    };
}
