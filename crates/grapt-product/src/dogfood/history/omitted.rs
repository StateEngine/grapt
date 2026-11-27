//! Report one page's unreadable items as data on the rows that own them.
//!
//! A gap is a fact about durable history, not about the words in a row: a
//! user's prompt and an assistant's content are what they are, and a reader
//! that wants to explain the hole reads this metadata instead. At most
//! [`MAX_ROW_GAPS`] coordinates are listed and the rest are counted, so naming
//! gaps can never push a row past its byte bound.
use serde_json::Value;

/// Exact coordinates one row's metadata lists before it summarizes the rest.
const MAX_ROW_GAPS: usize = 16;

/// One page's unreadable owned items, addressed by durable coordinate so a row
/// can report only the gaps that fall inside it.
#[derive(Debug, Default)]
pub(in crate::dogfood) struct OmittedItems(Vec<grapt_session::HistoryOmission>);

impl OmittedItems {
    pub(super) fn new(omissions: Vec<grapt_session::HistoryOmission>) -> Self {
        Self(omissions)
    }

    /// The gaps inside one row's durable range `[from, until)`, or empty.
    fn within(&self, from: u64, until: u64) -> Vec<&grapt_session::HistoryOmission> {
        self.0
            .iter()
            .filter(|omission| omission.seq >= from && omission.seq < until)
            .collect()
    }

    /// Add this row's gaps to its rendered JSON, under `metadata.omittedItems`.
    ///
    /// The gap is reported as data, keyed by exact durable coordinate, never by
    /// editing the row's own text. The row is NOT re-serialized: the public
    /// union is tag-first and a `serde_json` round trip sorts keys, which would
    /// put `id` before `type` and make the row undecodable — exactly the
    /// failure a splice must avoid. A row that already carries `metadata` has
    /// its object extended in place; a row without one gets the field inserted
    /// immediately after its tag.
    pub(super) fn annotate(&self, from: u64, until: u64, raw: String) -> Result<String, String> {
        let gaps = self.within(from, until);
        if gaps.is_empty() {
            return Ok(raw);
        }
        let listed = gaps
            .iter()
            .take(MAX_ROW_GAPS)
            .map(|gap| {
                serde_json::json!({
                    "seq": gap.seq,
                    "kind": gap.kind,
                    "class": gap.class.name(),
                })
            })
            .collect::<Vec<_>>();
        let total = u64::try_from(gaps.len()).unwrap_or(u64::MAX);
        let value: Value = serde_json::from_str(&raw)
            .map_err(|error| format!("history row is not JSON: {error}"))?;
        let metadata = value
            .get("metadata")
            .filter(|metadata| metadata.is_object())
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let mut metadata = metadata;
        let object = metadata
            .as_object_mut()
            .ok_or("history row metadata is not an object")?;
        object.insert("omittedItems".to_owned(), Value::Array(listed));
        object.insert("omittedTotal".to_owned(), Value::from(total));
        let encoded = serde_json::to_string(&metadata).map_err(|error| error.to_string())?;
        if let Some(existing) = value.get("metadata") {
            // Replace the old `metadata` value with the extended one, located
            // by its own encoding so the splice cannot land elsewhere.
            let old = serde_json::to_string(existing).map_err(|error| error.to_string())?;
            Ok(raw.replacen(
                &format!("\"metadata\":{old}"),
                &format!("\"metadata\":{encoded}"),
                1,
            ))
        } else {
            insert_after_tag(&raw, encoded)
        }
    }
}

/// Insert `"metadata":<encoded>` immediately after a row's leading tag pair.
///
/// The union decoder reads the tag FIRST and requires it, so a field may be
/// added after `"type":"…"` but never before it. The row is produced by this
/// module's own encoders, so its tag is present in that exact encoded form; a
/// row that does not carry it keeps its bytes and the gap is still logged,
/// rather than being rewritten into something else.
fn insert_after_tag(raw: &str, encoded: String) -> Result<String, String> {
    let value: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let tag = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or("history row has no type tag")?;
    let tag_pair = format!(
        "\"type\":{}",
        serde_json::to_string(tag).map_err(|error| error.to_string())?
    );
    let at = raw
        .find(&tag_pair)
        .ok_or("history row tag is not in its encoded form")?;
    let insert = at + tag_pair.len();
    let mut out = String::with_capacity(raw.len() + encoded.len() + 13);
    out.push_str(&raw[..insert]);
    out.push_str(&format!(",\"metadata\":{encoded}"));
    out.push_str(&raw[insert..]);
    Ok(out)
}
