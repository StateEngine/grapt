//! Hook identity. Law (foundational report row `path`/`hook`/`credential`):
//! "trust is never positional." A hook has a STABLE identity, not a position in
//! a list or an automatic trust hash; consent/trust and effect are separate
//! recorded facts (hook implementation itself lands later).

crate::id::opaque_id! {
    /// A stable hook identity. Trust and effect are recorded against this id, not
    /// inferred from a hook's position or a content hash.
    HookId
}

#[cfg(test)]
mod tests {
    use super::HookId;

    #[test]
    fn hook_id_is_a_non_empty_stable_identity() {
        assert!(HookId::new("pre-tool-use").is_some());
        assert!(HookId::new("").is_none());
    }
}
