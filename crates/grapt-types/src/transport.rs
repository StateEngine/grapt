//! Transport identities. Law: live notification is
//! bounded advisory state, never a projection source.

crate::id::opaque_id! { RequestId }
crate::id::opaque_id! { Cursor }
crate::id::opaque_id! { SubscriptionId }

crate::id::opaque_id! {
    /// The answering client's identity, as recorded in a settlement receipt.
    ///
    /// Law (`CORRECT`, amendment §8.2): "The transport supplies client identity;
    /// request JSON cannot."
    ///
    /// **This type is an INERT ID, not that guarantee.** Read the distinction
    /// carefully, because the doc here used to claim "a caller may not
    /// self-assert it" and that was simply untrue: `opaque_id!` generates a
    /// public `new`, so anyone can construct one out of a string. A reviewer
    /// found a live bypass that began exactly here — author a client id, author
    /// a selector version, and reach a public mint that trusted them.
    ///
    /// Where the law actually lives is the *provenance* of a value, and that is
    /// enforced by sealing every route that turns one into authority, not by
    /// pretending the id is unforgeable:
    ///
    /// - [`crate::authority::LivePermissionDecision`] carries the answering
    ///   client privately and has no public mint;
    /// - the subagent user-selection surface is crate-private.
    ///
    /// So holding one of these grants nothing, and a settlement receipt bearing
    /// one is audit truth about who answered — never proof that they did. The
    /// transport/settings owner that will issue authenticated ids is a named
    /// later lane.
    AuthenticatedClientId
}

crate::id::opaque_id! {
    /// A single-use nonce bound into one decision reply.
    ///
    /// Law: the first valid answer wins; an exact duplicate reconciles against
    /// the same `{PermissionId, principal, client, request, nonce}` tuple while
    /// a conflicting duplicate refuses.
    ///
    /// Inert in exactly the same sense as [`AuthenticatedClientId`]: publicly
    /// constructible, and load-bearing only once a sealed authority has bound it
    /// into a decision it privately carries.
    DecisionNonce
}
