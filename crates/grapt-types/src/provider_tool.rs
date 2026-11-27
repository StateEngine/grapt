//! Provider tool-call identity.
//!
//! Law (`CORRECT`), verbatim: "attempt-local opaque accumulation key versus
//! provider-scoped validated durable handle; conversion requires the adapter's
//! exact open/close receipt ... accumulation identity is never persisted as the
//! provider handle, and provider handles from different provider/account/attempt
//! scopes cannot collide."
//!
//! # The receipt is an audit identity; the proof is the authority
//!
//! [`AdapterOpenCloseReceipt`] is a parseable string. A conversion that merely
//! *takes* one consumes nothing unforgeable: any caller can type a receipt,
//! and a receipt legitimately issued in one scope carries no evidence that it
//! belongs to another. So the receipt stays, as the durable audit identifier of
//! the open/close pair, and it is no longer the proof.
//!
//! The proof is [`AdapterCloseProof`], and the chain that produces it is the
//! adapter's own lifecycle:
//!
//! ```text
//! ProviderStreamScope::enter(provider, account, attempt)   the adapter's live scope
//!   .open_tool_input(key)          -> ProviderToolAccumulation   (accumulation opened IN that scope)
//!   .close(receipt)                -> AdapterCloseProof          (consumes the accumulation, once)
//!   .into_handle(value)            -> ProviderToolHandle         (consumes the proof, once)
//! ```
//!
//! Every link consumes its predecessor by value, so an accumulation closes once
//! and a close proof mints one handle. And [`AdapterCloseProof::into_handle`]
//! takes **no scope arguments at all**: the handle's provider, account and
//! attempt come out of the proof. A proof closed in attempt A therefore cannot
//! mint a handle for attempt B — there is no parameter through which B could be
//! supplied, and [`ProviderToolHandle`]'s fields are private, so the handle
//! cannot be authored either.

use crate::attempt::PhysicalAttemptId;
use crate::credential::AccountId;
use crate::model::ProviderId;

crate::id::opaque_id! {
    /// Attempt-local accumulation key. Transient: it identifies a partially
    /// received tool input, never a durable provider handle.
    ProviderToolInputKey
}

crate::id::opaque_id! {
    /// The adapter's durable AUDIT identifier for one open/close pair.
    ///
    /// Deliberately not the authority. It is parseable data, so it records
    /// *which* open/close a handle came from and can be correlated in a log;
    /// it does not prove that the accumulation closed. That is
    /// [`AdapterCloseProof`], which is unforgeable and scope-bound.
    AdapterOpenCloseReceipt
}

crate::id::opaque_id! {
    /// The opaque value the provider itself uses to name a tool call.
    ///
    /// On its own this is NOT an identity: two providers, two accounts or two
    /// attempts may legitimately emit the same value. It becomes identity only
    /// inside a [`ProviderToolHandle`], which qualifies it.
    ProviderToolCallValue
}

/// The provider adapter's live scope for one physical attempt.
///
/// Every tool accumulation opened here inherits this exact
/// `(provider, account, attempt)` triple, so scope is a property of the stream
/// the adapter is actually running, not three loose arguments retyped at the
/// mint site. Not `Clone`: a scope is live authority for one attempt.
///
/// SEALED. An earlier revision left `enter` public and recorded restricting it
/// as an obligation on the provider-adapter lane. An external obligation is not
/// a seal — with a public entry the whole handle chain was forgeable end to end,
/// because every one of its inputs is an opaque id with a public constructor.
///
/// The route is crate-private now. This crate still cannot prove a caller holds
/// the stream it names, but it no longer lets anyone claim to: the chain has no
/// origin. The provider-adapter lane (`grapt-provider`), which owns the
/// `llm.stream()` boundary where a [`PhysicalAttemptId`] is actually minted, is
/// the named later owner.
#[derive(Debug)]
pub struct ProviderStreamScope {
    provider: ProviderId,
    account: AccountId,
    attempt: PhysicalAttemptId,
}

impl ProviderStreamScope {
    /// Enter the scope of one physical attempt against one provider account.
    /// SEALED. Entering a stream scope was public, and that made the entire
    /// handle chain forgeable end to end: enter with authored provider/account/
    /// attempt ids, open an accumulation under any key, close it with an
    /// authored `AdapterOpenCloseReceipt`, and mint a durable
    /// `ProviderToolHandle`. Every link consumed its predecessor, which made the
    /// chain look sealed, but its first link rested on nothing — no adapter fact
    /// existed anywhere in it.
    ///
    /// LATER-LANE DEPENDENCY: the owner is the provider-adapter lane, which owns
    /// the single `llm.stream` boundary where a `PhysicalAttemptId` is actually
    /// minted and can therefore prove the caller holds the stream it names.
    /// Until then the scope is unreachable and no durable handle can be
    /// fabricated.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn enter(
        provider: ProviderId,
        account: AccountId,
        attempt: PhysicalAttemptId,
    ) -> Self {
        Self {
            provider,
            account,
            attempt,
        }
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    /// Open one streaming tool input inside this exact scope.
    ///
    /// The scope is taken from `self`, never from arguments.
    pub fn open_tool_input(&self, key: ProviderToolInputKey) -> ProviderToolAccumulation {
        ProviderToolAccumulation {
            key,
            provider: self.provider.clone(),
            account: self.account.clone(),
            attempt: self.attempt.clone(),
        }
    }
}

/// One tool input accumulating inside a [`ProviderStreamScope`].
///
/// Fields are private and the only producer is
/// [`ProviderStreamScope::open_tool_input`]. Not `Clone`: an accumulation is
/// the live, partially received input, and two copies would close twice.
#[derive(Debug)]
pub struct ProviderToolAccumulation {
    key: ProviderToolInputKey,
    provider: ProviderId,
    account: AccountId,
    attempt: PhysicalAttemptId,
}

impl ProviderToolAccumulation {
    /// The attempt-local accumulation key. Transient identity.
    pub fn key(&self) -> &ProviderToolInputKey {
        &self.key
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    /// Close the accumulation, yielding the one-shot proof that it opened and
    /// then closed in this exact scope.
    ///
    /// Takes `self`, so an accumulation closes once and cannot be closed again
    /// under a second receipt. `receipt` is recorded for audit; it is not what
    /// makes the proof valid.
    pub fn close(self, receipt: AdapterOpenCloseReceipt) -> AdapterCloseProof {
        AdapterCloseProof {
            receipt,
            key: self.key,
            provider: self.provider,
            account: self.account,
            attempt: self.attempt,
        }
    }
}

/// Unforgeable proof that the adapter opened and then closed one accumulation,
/// bound to the exact `(provider, account, attempt)` scope and to the
/// accumulation key.
///
/// Fields are private and the only producer is
/// [`ProviderToolAccumulation::close`]. Not `Clone` and not `Copy`: one close
/// mints one handle.
#[derive(Debug)]
pub struct AdapterCloseProof {
    receipt: AdapterOpenCloseReceipt,
    key: ProviderToolInputKey,
    provider: ProviderId,
    account: AccountId,
    attempt: PhysicalAttemptId,
}

impl AdapterCloseProof {
    /// The durable audit identifier of the open/close pair.
    pub fn receipt(&self) -> &AdapterOpenCloseReceipt {
        &self.receipt
    }

    /// The accumulation this proof closed. Bound here so a proof cannot be
    /// paired with an accumulation it never covered.
    pub fn key(&self) -> &ProviderToolInputKey {
        &self.key
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    /// The ONE conversion from attempt-local accumulation to a durable,
    /// provider-scoped handle.
    ///
    /// The scope is not a parameter: provider, account and attempt are moved
    /// out of the proof, so a proof closed in one attempt cannot mint a handle
    /// in another. The accumulation key is dropped here rather than carried,
    /// because "accumulation identity is never persisted as the provider
    /// handle".
    pub fn into_handle(self, value: ProviderToolCallValue) -> ProviderToolHandle {
        ProviderToolHandle {
            provider: self.provider,
            account: self.account,
            attempt: self.attempt,
            value,
            receipt: self.receipt,
        }
    }
}

/// A durable, provider-scoped tool-call handle.
///
/// Qualified by provider, account and physical attempt so handles from
/// different scopes cannot collide even when the provider's own value repeats.
/// Fields are private and the only constructor is
/// [`AdapterCloseProof::into_handle`], which consumes the adapter's close
/// proof.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderToolHandle {
    provider: ProviderId,
    account: AccountId,
    attempt: PhysicalAttemptId,
    value: ProviderToolCallValue,
    receipt: AdapterOpenCloseReceipt,
}

impl ProviderToolHandle {
    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    pub fn value(&self) -> &ProviderToolCallValue {
        &self.value
    }

    /// The audit identifier of the open/close that produced this handle.
    pub fn receipt(&self) -> &AdapterOpenCloseReceipt {
        &self.receipt
    }

    /// Whether this handle belongs to the named scope.
    ///
    /// A handle is only usable inside the exact provider, account and attempt
    /// it was opened in; reuse across any of the three is refused.
    pub fn is_in_scope(
        &self,
        provider: &ProviderId,
        account: &AccountId,
        attempt: &PhysicalAttemptId,
    ) -> bool {
        &self.provider == provider && &self.account == account && &self.attempt == attempt
    }

    /// Whether this handle belongs to a live adapter scope.
    pub fn belongs_to(&self, scope: &ProviderStreamScope) -> bool {
        self.is_in_scope(scope.provider(), scope.account(), scope.attempt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(provider: &str, account: &str, attempt: &str) -> Option<ProviderStreamScope> {
        Some(ProviderStreamScope::enter(
            ProviderId::new(provider)?,
            AccountId::new(account)?,
            PhysicalAttemptId::new(attempt)?,
        ))
    }

    fn handle(provider: &str, account: &str, attempt: &str) -> Option<ProviderToolHandle> {
        Some(
            scope(provider, account, attempt)?
                .open_tool_input(ProviderToolInputKey::new("acc-1")?)
                .close(AdapterOpenCloseReceipt::new("open-close-1")?)
                .into_handle(ProviderToolCallValue::new("call_1")?),
        )
    }

    #[test]
    fn accumulation_key_and_durable_handle_are_distinct_identities(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let scope = scope("openai", "acct-1", "att-1").ok_or("scope")?;
        let accumulation =
            scope.open_tool_input(ProviderToolInputKey::new("acc-1").ok_or("accumulation key")?);
        assert_eq!(accumulation.key().as_str(), "acc-1");

        // The accumulation is consumed by the close, and the key it carried is
        // dropped by the mint, so it cannot survive as the handle. The handle's
        // value is the PROVIDER's value even when the text coincides.
        let built = accumulation
            .close(AdapterOpenCloseReceipt::new("open-close-1").ok_or("receipt")?)
            .into_handle(ProviderToolCallValue::new("acc-1").ok_or("value")?);
        assert_eq!(built.value().as_str(), "acc-1");
        Ok(())
    }

    #[test]
    fn the_same_provider_value_in_another_scope_is_another_handle(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let base = handle("openai", "acct-1", "att-1").ok_or("handle")?;

        assert_ne!(
            base,
            handle("anthropic", "acct-1", "att-1").ok_or("handle")?,
            "another provider is another handle"
        );
        assert_ne!(
            base,
            handle("openai", "acct-2", "att-1").ok_or("handle")?,
            "another account is another handle"
        );
        assert_ne!(
            base,
            handle("openai", "acct-1", "att-2").ok_or("handle")?,
            "another attempt is another handle"
        );
        Ok(())
    }

    #[test]
    fn a_handle_is_refused_outside_its_exact_scope() -> Result<(), Box<dyn std::error::Error>> {
        let held = handle("openai", "acct-1", "att-1").ok_or("handle")?;
        let provider = ProviderId::new("openai").ok_or("provider")?;
        let account = AccountId::new("acct-1").ok_or("account")?;
        let attempt = PhysicalAttemptId::new("att-1").ok_or("attempt")?;

        assert!(held.is_in_scope(&provider, &account, &attempt));
        assert!(!held.is_in_scope(
            &provider,
            &account,
            &PhysicalAttemptId::new("att-2").ok_or("attempt")?
        ));
        assert!(!held.is_in_scope(
            &ProviderId::new("anthropic").ok_or("provider")?,
            &account,
            &attempt
        ));
        Ok(())
    }

    #[test]
    fn a_close_proof_carries_the_scope_it_actually_closed_in(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let scope = scope("openai", "acct-1", "att-1").ok_or("scope")?;
        let proof = scope
            .open_tool_input(ProviderToolInputKey::new("acc-1").ok_or("key")?)
            .close(AdapterOpenCloseReceipt::new("open-close-1").ok_or("receipt")?);

        assert_eq!(proof.provider().as_str(), "openai");
        assert_eq!(proof.account().as_str(), "acct-1");
        assert_eq!(proof.attempt().as_str(), "att-1");
        assert_eq!(
            proof.key().as_str(),
            "acc-1",
            "the proof is bound to the accumulation it closed"
        );
        assert_eq!(proof.receipt().as_str(), "open-close-1");
        Ok(())
    }

    #[test]
    fn a_proof_closed_in_one_attempt_cannot_mint_a_handle_in_another(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let attempt_a = scope("openai", "acct-1", "att-1").ok_or("scope")?;
        let attempt_b = scope("openai", "acct-1", "att-2").ok_or("scope")?;

        let proof = attempt_a
            .open_tool_input(ProviderToolInputKey::new("acc-1").ok_or("key")?)
            .close(AdapterOpenCloseReceipt::new("open-close-1").ok_or("receipt")?);

        // `into_handle` takes only the provider's value: there is no parameter
        // through which attempt B could be supplied, and the handle's fields
        // are private, so the scope cannot be rewritten afterwards either.
        let minted = proof.into_handle(ProviderToolCallValue::new("call_1").ok_or("value")?);
        assert!(minted.belongs_to(&attempt_a));
        assert!(
            !minted.belongs_to(&attempt_b),
            "a proof from attempt A never names attempt B"
        );
        Ok(())
    }

    #[test]
    fn an_audit_receipt_reused_in_another_scope_still_scopes_to_that_scope(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The receipt is audit data, so nothing stops it being repeated. What
        // it can no longer do is carry a handle across scopes: the scope comes
        // from the proof, and a proof only exists where an accumulation was
        // actually opened and closed.
        let receipt = AdapterOpenCloseReceipt::new("open-close-1").ok_or("receipt")?;
        let elsewhere = scope("anthropic", "acct-9", "att-9").ok_or("scope")?;
        let minted = elsewhere
            .open_tool_input(ProviderToolInputKey::new("acc-1").ok_or("key")?)
            .close(receipt)
            .into_handle(ProviderToolCallValue::new("call_1").ok_or("value")?);

        assert!(minted.belongs_to(&elsewhere));
        assert!(
            !minted.belongs_to(&scope("openai", "acct-1", "att-1").ok_or("scope")?),
            "repeating a receipt does not import another scope's authority"
        );
        Ok(())
    }
}
