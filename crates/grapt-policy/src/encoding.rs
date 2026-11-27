//! The effective-graph canonical encoding.
//!
//! This is deliberately NOT the JSON canonical-bytes form used for durable
//! events (that form reproduces the oracle's `JSON.stringify` byte-for-byte and
//! lives in `grapt-event`). An authority receipt has a different adversary:
//! two semantically distinct graphs must never hash equally, and equivalent
//! graphs supplied in different provenance order must. So the encoding is:
//!
//! - **Versioned + domain-separated.** Every canonical stream begins with a
//!   fixed domain label and a format version. A byte stream from another domain
//!   or version can never be mistaken for an effective graph.
//! - **Length-delimited (TLV).** Every value is `tag · u64 length · payload`.
//!   Concatenation is therefore unambiguous: the classic `"a"+"bc"` vs
//!   `"ab"+"c"` collision is unrepresentable because each piece carries its own
//!   length. This is what the delimiter/collision witnesses exercise.
//! - **Order-canonical.** A [`Node::Set`] sorts its already-encoded children,
//!   so element input order cannot change the bytes. A [`Node::Map`] sorts by
//!   `(key, encoded value)`, so field order is irrelevant AND a (hypothetical)
//!   duplicate key is still canonical. Ordered sequences ([`Node::Seq`])
//!   preserve order because their order is semantic.
//!
//! The value model ([`Node`]) and the byte/hash functions are crate-private: no
//! external caller can construct a non-canonical node. Only [`Digest`] and the
//! receipt's own bytes cross the crate boundary.

use sha2::Sha256;

/// Domain label written first in every canonical stream. Changing it changes
/// every receipt — that is the point of domain separation.
const DOMAIN: &[u8] = b"secode-policy/effective-graph";

/// Encoding format version. A structural change to the encoding MUST bump this
/// so old and new receipts are never confused.
const FORMAT_VERSION: u64 = 1;

/// Domain label for the KEYED contribution-identity mint. It is deliberately a
/// different label from [`DOMAIN`]: a graph preimage and a contribution preimage
/// are different questions, and a byte stream built for one must never verify as
/// the other even under the same key.
const MINT_DOMAIN: &[u8] = b"grapt-policy/contribution-id";

/// Mint format version, separate from [`FORMAT_VERSION`] because the two
/// streams evolve independently.
const MINT_FORMAT_VERSION: u64 = 1;

// Type tags. Distinct leading bytes so a value's kind is unambiguous before its
// length is read.
const TAG_STR: u8 = b'S';
const TAG_U64: u8 = b'U';
const TAG_BYTES: u8 = b'B';
const TAG_SEQ: u8 = b'Q';
const TAG_MAP: u8 = b'M';
const TAG_SET: u8 = b'X';

/// SHA-256 of `bytes`, fully qualified so no trait alias enters scope (the
/// crate has its own `Digest` type).
fn sha256(bytes: &[u8]) -> [u8; 32] {
    <Sha256 as sha2::Digest>::digest(bytes).into()
}

/// A 32-byte SHA-256 digest of a WHOLE canonical stream.
///
/// # Why nothing here is public any more
///
/// This type used to expose `of_bytes` and `to_hex` publicly, and two
/// contributor fields carried one directly. Together those were a complete
/// offline guessing oracle over environment values and config blobs: the
/// receipt published the digest, and any holder could digest a candidate and
/// compare. Master §8.4 forbids exactly that — "no public raw SHA-256 secret
/// fingerprint or other stable offline equality oracle".
///
/// Withholding the field bytes alone was not sufficient, because a deterministic
/// unkeyed whole-graph digest is the same oracle with extra steps once the other
/// coordinates are known or controlled. So secret-derived material no longer
/// enters the graph preimage in unkeyed form at all: it enters as a
/// [`ContributionId`](crate::ContributionId) minted under an owner-held key.
/// Only then is the whole-graph digest safe to publish, which is why
/// `EffectiveGraphReceipt::digest_hex` remains public and this type does not
/// leave the crate.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Digest([u8; 32]);

impl Digest {
    /// Lowercase hex. Crate-private: the ONE rendering that crosses the crate
    /// boundary is the whole-graph digest on the receipt.
    ///
    /// There is deliberately no `as_bytes` any more. It existed so a contributor
    /// could lower its digest into the canonical stream; no contributor does
    /// that now, and a raw-byte accessor left behind for symmetry is how the
    /// next field quietly acquires one.
    pub(crate) fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

/// The canonical value model. The snapshot lowers itself into a single
/// [`Node::Map`]; the encoder is a total function of that node. Crate-private:
/// callers cannot fabricate a non-canonical node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Str(String),
    U64(u64),
    Bytes(Vec<u8>),
    /// Ordered sequence — order is semantic and preserved.
    Seq(Vec<Node>),
    /// Keyed fields. Encoded in sorted `(key, encoded-value)` order, so field
    /// order is irrelevant and even a duplicate key is canonical.
    Map(Vec<(String, Node)>),
    /// Unordered set — encoded children are sorted, so element input order does
    /// not affect the bytes.
    Set(Vec<Node>),
}

/// The full canonical byte stream for a root node: domain label, format
/// version, then the node.
pub(crate) fn canonical_bytes(root: &Node) -> Vec<u8> {
    let mut out = Vec::new();
    write_len_prefixed(&mut out, TAG_BYTES, DOMAIN);
    write_u64(&mut out, FORMAT_VERSION);
    write_node(&mut out, root);
    out
}

/// Content address of a root node: SHA-256 of its canonical bytes.
pub(crate) fn content_digest(root: &Node) -> Digest {
    Digest(sha256(&canonical_bytes(root)))
}

/// The KEYED contribution-identity mint.
///
/// Domain-separated and length-framed over the authority's declared realm and
/// key version, the contributor class, its coordinate, and the canonical
/// material. Every component is `tag · u64 length · payload`, so no byte can
/// migrate across a boundary without changing the stream — a realm of `"a"` with
/// version `"bc"` and a realm of `"ab"` with version `"c"` are different
/// preimages, and so are an env var named `"AB"` holding `"C"` and one named
/// `"A"` holding `"BC"`.
///
/// The result is an HMAC, not a bare hash, and that is the whole security
/// property: a holder of the receipt cannot recompute the identity for a
/// candidate value without the owner-held key, so there is no offline equality
/// oracle. Identical inputs under the same realm/key-version still produce
/// identical bytes, so the graph stays content-addressed; a deliberate key or
/// key-version rotation produces different bytes, so rotation stays observable.
///
/// Crate-private, and the only route to a [`ContributionId`](crate::ContributionId).
pub(crate) fn contribution_mint(
    key: &[u8],
    realm: &str,
    key_version: &str,
    class: &str,
    coordinate: &str,
    material: &[u8],
) -> [u8; 32] {
    let mut preimage = Vec::new();
    write_len_prefixed(&mut preimage, TAG_BYTES, MINT_DOMAIN);
    write_u64(&mut preimage, MINT_FORMAT_VERSION);
    write_len_prefixed(&mut preimage, TAG_STR, realm.as_bytes());
    write_len_prefixed(&mut preimage, TAG_STR, key_version.as_bytes());
    write_len_prefixed(&mut preimage, TAG_STR, class.as_bytes());
    write_len_prefixed(&mut preimage, TAG_STR, coordinate.as_bytes());
    write_len_prefixed(&mut preimage, TAG_BYTES, material);
    hmac_sha256(key, &preimage)
}

/// HMAC-SHA256 (RFC 2104), written here rather than pulled in as a dependency:
/// this workspace tree is shared and a lockfile change is not a free act.
///
/// `H(key ‖ message)` would have been simpler and is wrong: it is
/// length-extendable, and while the identities minted here are never published,
/// a construction whose safety depends on an output staying private is the kind
/// of assumption that survives exactly until the first accessor is added.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;

    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&sha256(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(BLOCK + message.len());
    inner.extend(padded.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(message);
    let inner = sha256(&inner);

    let mut outer = Vec::with_capacity(BLOCK + inner.len());
    outer.extend(padded.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner);
    sha256(&outer)
}

fn write_node(out: &mut Vec<u8>, node: &Node) {
    match node {
        Node::Str(s) => write_len_prefixed(out, TAG_STR, s.as_bytes()),
        Node::U64(n) => write_u64(out, *n),
        Node::Bytes(b) => write_len_prefixed(out, TAG_BYTES, b),
        Node::Seq(items) => {
            out.push(TAG_SEQ);
            write_u64_raw(out, items.len() as u64);
            for item in items {
                write_node(out, item);
            }
        }
        Node::Map(entries) => {
            // Sort by (key, encoded value) so field order is irrelevant AND two
            // entries sharing a key are still ordered deterministically.
            let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = entries
                .iter()
                .map(|(key, value)| {
                    let mut kbuf = Vec::new();
                    write_len_prefixed(&mut kbuf, TAG_STR, key.as_bytes());
                    let mut vbuf = Vec::new();
                    write_node(&mut vbuf, value);
                    (kbuf, vbuf)
                })
                .collect();
            encoded.sort();
            out.push(TAG_MAP);
            write_u64_raw(out, encoded.len() as u64);
            for (kbuf, vbuf) in encoded {
                out.extend_from_slice(&kbuf);
                out.extend_from_slice(&vbuf);
            }
        }
        Node::Set(items) => {
            // Sort by the child's OWN canonical encoding so element input order
            // is irrelevant. Each element is length-prefixed so boundaries stay
            // explicit after sorting.
            let mut encoded: Vec<Vec<u8>> = items
                .iter()
                .map(|item| {
                    let mut buf = Vec::new();
                    write_node(&mut buf, item);
                    buf
                })
                .collect();
            encoded.sort();
            out.push(TAG_SET);
            write_u64_raw(out, encoded.len() as u64);
            for element in encoded {
                write_u64_raw(out, element.len() as u64);
                out.extend_from_slice(&element);
            }
        }
    }
}

/// `tag · u64 length · payload`.
fn write_len_prefixed(out: &mut Vec<u8>, tag: u8, payload: &[u8]) {
    out.push(tag);
    write_u64_raw(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// A tagged, length-delimited u64 (fixed 8-byte big-endian payload). Tagged and
/// length-prefixed like every other value so it composes uniformly.
fn write_u64(out: &mut Vec<u8>, n: u64) {
    out.push(TAG_U64);
    write_u64_raw(out, 8);
    out.extend_from_slice(&n.to_be_bytes());
}

fn write_u64_raw(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_be_bytes());
}

/// Lowercase hex without a `fmt::Write` import (a nibble table, so no trait
/// alias is needed).
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_delimiting_defeats_the_concatenation_collision() {
        // Two DISTINCT sequences that share a naive concatenation ("a"+"bc" vs
        // "ab"+"c") must not collide.
        let left = Node::Seq(vec![Node::Str("a".into()), Node::Str("bc".into())]);
        let right = Node::Seq(vec![Node::Str("ab".into()), Node::Str("c".into())]);
        assert_ne!(content_digest(&left), content_digest(&right));
    }

    #[test]
    fn a_str_and_bytes_with_equal_payload_do_not_collide() {
        // Distinct tags keep a string apart from raw bytes of the same content.
        let s = Node::Str("xy".into());
        let b = Node::Bytes(b"xy".to_vec());
        assert_ne!(content_digest(&s), content_digest(&b));
    }

    #[test]
    fn set_order_is_irrelevant_but_map_keys_are_sorted_stably() {
        let a = Node::Set(vec![Node::Str("one".into()), Node::Str("two".into())]);
        let b = Node::Set(vec![Node::Str("two".into()), Node::Str("one".into())]);
        assert_eq!(content_digest(&a), content_digest(&b));

        let m1 = Node::Map(vec![("b".into(), Node::U64(2)), ("a".into(), Node::U64(1))]);
        let m2 = Node::Map(vec![("a".into(), Node::U64(1)), ("b".into(), Node::U64(2))]);
        assert_eq!(content_digest(&m1), content_digest(&m2));
    }

    #[test]
    fn a_map_with_a_duplicate_key_is_order_canonical() {
        // Adversary: a Node with a repeated key must not depend on input order,
        // so a hostile caller (were the type reachable) could not produce two
        // distinct byte streams for the same multiset of entries.
        let a = Node::Map(vec![("k".into(), Node::U64(1)), ("k".into(), Node::U64(2))]);
        let b = Node::Map(vec![("k".into(), Node::U64(2)), ("k".into(), Node::U64(1))]);
        assert_eq!(content_digest(&a), content_digest(&b));
    }

    #[test]
    fn a_set_is_not_a_seq_even_with_the_same_members() {
        let seq = Node::Seq(vec![Node::Str("one".into()), Node::Str("two".into())]);
        let set = Node::Set(vec![Node::Str("one".into()), Node::Str("two".into())]);
        assert_ne!(content_digest(&seq), content_digest(&set));
    }

    #[test]
    fn digest_debug_is_hex_only() {
        let d = content_digest(&Node::Str("anything".into()));
        let shown = format!("{d:?}");
        assert!(shown.starts_with("Digest("));
        assert_eq!(d.to_hex().len(), 64);
    }

    // ---- the keyed mint ---------------------------------------------------
    //
    // Each witness below varies exactly ONE component of the preimage, so the
    // separation it names is the only mechanism that can be keeping the two
    // sides apart.

    const KEY: &[u8] = b"owner-held-key";

    fn mint(realm: &str, version: &str, class: &str, coord: &str, material: &[u8]) -> [u8; 32] {
        contribution_mint(KEY, realm, version, class, coord, material)
    }

    #[test]
    fn identical_inputs_mint_the_same_identity() {
        // Content-addressing: without this the graph digest would move on every
        // rebuild and nothing downstream could compare two attempts.
        assert_eq!(
            mint("r", "v1", "env-value", "TOKEN", b"secret"),
            mint("r", "v1", "env-value", "TOKEN", b"secret")
        );
    }

    #[test]
    fn a_key_version_rotation_changes_the_identity() {
        // Rotation must be OBSERVABLE. Only the declared version differs here.
        assert_ne!(
            mint("r", "v1", "env-value", "TOKEN", b"secret"),
            mint("r", "v2", "env-value", "TOKEN", b"secret")
        );
    }

    #[test]
    fn rotating_the_key_material_changes_the_identity() {
        // …and so must a new key under an unchanged declared version, or a
        // rotation that forgot to bump the label would be invisible.
        assert_ne!(
            contribution_mint(b"key-one", "r", "v1", "env-value", "TOKEN", b"secret"),
            contribution_mint(b"key-two", "r", "v1", "env-value", "TOKEN", b"secret")
        );
    }

    #[test]
    fn two_realms_do_not_share_an_identity_space() {
        assert_ne!(
            mint("realm-a", "v1", "env-value", "TOKEN", b"secret"),
            mint("realm-b", "v1", "env-value", "TOKEN", b"secret")
        );
    }

    #[test]
    fn a_class_separates_two_contributors_with_the_same_coordinate() {
        // An env var named "5" and a config layer at rank 5 must not collide.
        assert_ne!(
            mint("r", "v1", "env-value", "5", b"x"),
            mint("r", "v1", "config-content", "5", b"x")
        );
    }

    #[test]
    fn length_framing_keeps_the_coordinate_and_material_apart() {
        // The classic concatenation collision, applied to the mint: without the
        // length prefixes, coordinate "AB" + material "C" and coordinate "A" +
        // material "BC" would produce one preimage.
        assert_ne!(
            mint("r", "v1", "env-value", "AB", b"C"),
            mint("r", "v1", "env-value", "A", b"BC")
        );
    }

    #[test]
    fn length_framing_keeps_the_realm_and_key_version_apart() {
        assert_ne!(
            mint("a", "bc", "env-value", "TOKEN", b"x"),
            mint("ab", "c", "env-value", "TOKEN", b"x")
        );
    }

    #[test]
    fn the_mint_is_domain_separated_from_the_graph_encoding() {
        // A contribution preimage and a graph preimage are different questions.
        // Asserted by the labels themselves: the mint's domain cannot be a
        // prefix-compatible relabelling of the graph's, because the two are
        // compared here and a future edit that unified them fails.
        assert_ne!(DOMAIN, MINT_DOMAIN);
    }

    #[test]
    fn hmac_matches_the_rfc_2104_test_vector() {
        // RFC 4231 case 2: key "Jefe", data "what do ya want for nothing?".
        // Hand-rolled crypto is worth exactly one known-answer test; without it
        // a transposed pad constant would still produce stable, distinct,
        // entirely wrong identities and every witness above would pass.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            to_hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_handles_a_key_longer_than_one_block() {
        // RFC 4231 case 3 uses a 131-byte key, which HMAC must hash down first.
        // The long-key branch is otherwise unexercised by any mint in this
        // crate, so a wrong reduction would never be observed.
        let key = [0xaau8; 131];
        let mac = hmac_sha256(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            to_hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
