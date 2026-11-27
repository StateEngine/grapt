//! The INJECTED, OWNER-HELD identity authority for secret-bearing contributors.
//!
//! # The defect this closes
//!
//! Two contributor fields — `EnvExposure.value_digest` and `ConfigLayer.digest`
//! — carried an unsalted public SHA-256 over an environment value or a config
//! blob, and the receipt published it. Master §8.4: "There is no public raw
//! SHA-256 secret fingerprint or other stable offline equality oracle. Authority
//! graphs name a credential record/version or a keyed opaque identity, never the
//! raw-secret digest."
//!
//! Hiding the fields alone would not have closed it. A deterministic whole-graph
//! digest is the same oracle whenever the remaining coordinates are known or
//! attacker-controlled: recompute the graph for each candidate value and compare.
//! Security may not rest on the other coordinates staying unknown. So the fix is
//! at the preimage: no unkeyed secret-derived bytes enter the graph at all.
//!
//! # What replaces it
//!
//! A [`ContributionAuthority`] is injected from above this crate. It declares:
//!
//! - an [`AuthorityRealm`] — which identity space these contributions live in;
//! - a [`KeyVersion`] — the declared, stable version of the key in force;
//! - the key itself, held as [`Secret`] and never readable through this type.
//!
//! Realm and key version are **stable across snapshots and physical attempts**,
//! deliberately. A per-snapshot or random key would make identical authority
//! inputs mint different identities, and the graph would stop being
//! content-addressed — two attempts under the same authority could no longer be
//! recognised as the same authority at all. The version is a declared label
//! precisely so that rotation is a decision someone makes and records, rather
//! than an accident of process lifetime.
//!
//! It mints [`ContributionId`]s: opaque, non-constructible, with no public
//! from-bytes, digest, hex or byte accessor, and no public mint. A holder of a
//! receipt cannot recompute one for a candidate value without the key, so the
//! offline oracle is gone. A holder of the value AND the key gets the same
//! identity every time, so the graph stays content-addressed. A deliberate key
//! or key-version rotation changes every identity it touches, so rotation stays
//! observable — which is the signal the effective graph exists to carry, and the
//! thing blanking the field would have destroyed along with the oracle.
//!
//! # What this is NOT
//!
//! It is not a credential model. Where a value is credential-backed, the graph
//! names [`grapt_types::CredentialRecordId`] and
//! [`grapt_types::CredentialVersion`] through
//! [`CredentialFingerprint`](crate::CredentialFingerprint) and no keyed identity
//! is involved. The keyed identity is for content whose exact VALUE must remain
//! rotation-observable and which has no record/version of its own: environment
//! values and config-layer content.

use crate::encoding::contribution_mint;
use crate::secret::Secret;

/// The identity space a set of contributions belongs to.
///
/// Declared by the owner and stable across snapshots. Changing it is a
/// deliberate re-partitioning of the identity space, not a rotation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityRealm(String);

impl AuthorityRealm {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The declared version of the key in force.
///
/// It is part of every minted preimage, so bumping it rotates the identity
/// space even if the key material were somehow reused. It is a label, never
/// derived from the key: a version derived from the material would be a
/// fingerprint of the key, which is the class of value this whole module exists
/// to remove.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyVersion(String);

impl KeyVersion {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which kind of contributor a keyed identity was minted for.
///
/// Crate-private and closed. It separates identity spaces so an environment
/// variable named `5` and a config layer at rank `5` cannot mint one identity
/// from the same content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContributionClass {
    EnvValue,
    ConfigContent,
}

impl ContributionClass {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ContributionClass::EnvValue => "env-value",
            ContributionClass::ConfigContent => "config-content",
        }
    }
}

/// An opaque keyed identity for one secret-bearing contribution.
///
/// # The absent surface, and why each piece is absent
///
/// There is no `from_bytes`, no `new`, no `as_bytes`, no `to_hex`, and no public
/// mint. A `ContributionId` can be obtained in exactly one way: by handing a
/// [`ContributionAuthority`] the material through the owning contributor's
/// constructor. That is what makes it non-constructible rather than merely
/// inconvenient to construct — there is no route, not a discouraged one.
///
/// `Debug` renders a fixed marker. The identity is keyed, so printing it is not
/// an offline oracle for a reader without the key; it is still absent because a
/// rendering is a byte accessor with extra steps, and the ruling names byte
/// accessors specifically.
///
/// `PartialEq` IS derived, and is required: the snapshot builder distinguishes a
/// duplicate contributor from a conflicting one by comparing values. This is not
/// an escalation. Comparing two identities requires holding both, and minting the
/// second requires the authority — that is, the key. A party holding the key can
/// already compute anything this comparison would tell them. The property the
/// ruling protects is that a party holding only the RECEIPT can confirm nothing,
/// and no comparison here gives them that.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContributionId([u8; 32]);

impl ContributionId {
    /// The raw bytes, for the canonical encoder only.
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for ContributionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No hex, no length, no prefix. Contrast `Digest`, whose Debug prints
        // hex because a whole-graph digest is publishable; a per-contribution
        // identity is not.
        f.write_str("ContributionId(<opaque>)")
    }
}

/// The owner-held authority that mints [`ContributionId`]s.
///
/// Construct it once, above this crate, from the owner's key material, and pass
/// it by reference to every contributor constructor for one authority scope. It
/// is deliberately not `Clone`: a key is a thing you hold, not a value you
/// scatter, and a second copy is a second place to lose it from.
pub struct ContributionAuthority {
    realm: AuthorityRealm,
    key_version: KeyVersion,
    key: Secret,
}

impl ContributionAuthority {
    /// Inject an identity authority.
    ///
    /// The caller owns the lifetime of `key` and the decision to rotate it.
    /// Nothing in this crate reads the key back out, generates one, or reaches
    /// for ambient randomness: an identity authority this crate could mint for
    /// itself would be a per-process key, and a per-process key is exactly the
    /// per-snapshot randomness the ruling refuses.
    pub fn new(realm: AuthorityRealm, key_version: KeyVersion, key: Secret) -> Self {
        Self {
            realm,
            key_version,
            key,
        }
    }

    /// The declared realm. A label, safe to render; it is not the key.
    pub fn realm(&self) -> &AuthorityRealm {
        &self.realm
    }

    /// The declared key version. Also a label: it says WHICH key is in force,
    /// never anything about its material.
    pub fn key_version(&self) -> &KeyVersion {
        &self.key_version
    }

    /// Mint the identity for one contribution.
    ///
    /// Crate-private, which is the "no caller-mint route" half of the ruling.
    /// A public mint would hand any caller an online oracle: mint a candidate,
    /// compare with a contributor they hold. Every public route into this
    /// function goes through a contributor constructor that keeps its result
    /// private.
    pub(crate) fn mint(
        &self,
        class: ContributionClass,
        coordinate: &str,
        material: &Secret,
    ) -> ContributionId {
        ContributionId(contribution_mint(
            self.key.expose(),
            self.realm.as_str(),
            self.key_version.as_str(),
            class.label(),
            coordinate,
            material.expose(),
        ))
    }
}

impl std::fmt::Debug for ContributionAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Realm and version are declared labels and are the two facts an
        // operator needs to answer "which key was this graph resolved under".
        // The key is not a field here at all, so a later `derive(Debug)` cannot
        // reintroduce it by accident.
        f.debug_struct("ContributionAuthority")
            .field("realm", &self.realm)
            .field("key_version", &self.key_version)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(realm: &str, version: &str, key: &[u8]) -> ContributionAuthority {
        let realm = match AuthorityRealm::new(realm) {
            Some(v) => v,
            None => panic!("realm"),
        };
        let version = match KeyVersion::new(version) {
            Some(v) => v,
            None => panic!("version"),
        };
        ContributionAuthority::new(realm, version, Secret::new(key.to_vec()))
    }

    #[test]
    fn one_authority_mints_one_identity_for_one_value() {
        let a = authority("grapt", "v1", b"k");
        assert_eq!(
            a.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"x".to_vec())
            ),
            a.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"x".to_vec())
            )
        );
    }

    #[test]
    fn a_changed_value_changes_the_identity_so_rotation_stays_observable() {
        // This is the signal blanking the field would have destroyed: the graph
        // must still observe that an environment value changed.
        let a = authority("grapt", "v1", b"k");
        assert_ne!(
            a.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"old".to_vec())
            ),
            a.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"new".to_vec())
            )
        );
    }

    #[test]
    fn two_authorities_with_the_same_declared_labels_but_different_keys_disagree() {
        // The key is what makes the identity unguessable; a witness that varied
        // only the labels could pass with the key ignored entirely.
        let one = authority("grapt", "v1", b"key-one");
        let two = authority("grapt", "v1", b"key-two");
        assert_ne!(
            one.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"x".to_vec())
            ),
            two.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"x".to_vec())
            )
        );
    }

    #[test]
    fn a_declared_version_bump_rotates_the_identity_space() {
        let one = authority("grapt", "v1", b"k");
        let two = authority("grapt", "v2", b"k");
        assert_ne!(
            one.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"x".to_vec())
            ),
            two.mint(
                ContributionClass::EnvValue,
                "TOKEN",
                &Secret::new(b"x".to_vec())
            )
        );
    }

    #[test]
    fn the_debug_rendering_carries_no_key_and_no_identity_bytes() {
        let a = authority("grapt", "v1", b"SENTINEL_KEY_MATERIAL");
        let rendered = format!("{a:?}");
        assert!(rendered.contains("grapt"));
        assert!(rendered.contains("v1"));
        assert!(!rendered.contains("SENTINEL"));

        let id = a.mint(
            ContributionClass::EnvValue,
            "TOKEN",
            &Secret::new(b"SENTINEL_VALUE".to_vec()),
        );
        let rendered = format!("{id:?}");
        assert_eq!(rendered, "ContributionId(<opaque>)");
        // Not merely "the sentinel is absent": the hex of the identity is absent
        // too, so the rendering is not a byte accessor for a reader who already
        // knows the key.
        assert!(!rendered.contains(&hex(id.as_bytes()[..4].as_ref())));
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn an_empty_realm_or_key_version_is_unrepresentable() {
        // An empty label would silently merge two identity spaces, and an empty
        // version would make "which key" unanswerable.
        assert!(AuthorityRealm::new("").is_none());
        assert!(KeyVersion::new("").is_none());
    }
}
