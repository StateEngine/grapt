//! Original patch receipts survive presentation hooks unchanged.
use crate::session_tool_terminal::{
    decode_artifact, encode_artifact, invalid, object, reject_unknown, required, required_str,
    PayloadError,
};
use grapt_types::{MessageId, SessionId, ToolCallId, ToolTerminalArtifact};
use serde_json::Value;

const AT: &str = "session.patch.receipt.1";
pub const MAX_RECEIPT_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchReceiptFact {
    session: SessionId,
    input: MessageId,
    call: ToolCallId,
    receipt: ToolTerminalArtifact,
}

impl PatchReceiptFact {
    pub fn new(
        session: SessionId,
        input: MessageId,
        call: ToolCallId,
        receipt: ToolTerminalArtifact,
    ) -> Result<Self, PayloadError> {
        Self::validate_coordinates(&session, &input, &call)?;
        if !receipt.disposition().is_complete() || receipt.reference().length() > MAX_RECEIPT_BYTES
        {
            return Err(invalid(
                AT,
                "receipt",
                "a complete original patch receipt within 128 KiB",
            ));
        }
        Ok(Self {
            session,
            input,
            call,
            receipt,
        })
    }
    /// Call before filesystem effects so the original receipt can name them.
    pub fn validate_coordinates(
        session: &SessionId,
        input: &MessageId,
        call: &ToolCallId,
    ) -> Result<(), PayloadError> {
        if session.as_str().len() > 512 || input.as_str().len() > 512 || call.as_str().len() > 512 {
            return Err(invalid(AT, "identity", "identities of at most 512 bytes"));
        }
        Ok(())
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }
    pub fn input(&self) -> &MessageId {
        &self.input
    }
    pub fn call(&self) -> &ToolCallId {
        &self.call
    }
    pub fn receipt(&self) -> &ToolTerminalArtifact {
        &self.receipt
    }
}

pub fn encode(fact: &PatchReceiptFact) -> Value {
    serde_json::json!({"sessionID":fact.session.as_str(),"inputID":fact.input.as_str(),"callID":fact.call.as_str(),"receipt":encode_artifact(&fact.receipt)})
}

pub fn decode(value: &Value) -> Result<PatchReceiptFact, PayloadError> {
    let root = object(value, AT)?;
    reject_unknown(root, AT, &["sessionID", "inputID", "callID", "receipt"])?;
    let session = required_str(root, AT, "sessionID")?
        .parse::<SessionId>()
        .map_err(|_| invalid(AT, "sessionID", "a session identity"))?;
    let input = required_str(root, AT, "inputID")?
        .parse::<MessageId>()
        .map_err(|_| invalid(AT, "inputID", "a message identity"))?;
    let call = ToolCallId::new(required_str(root, AT, "callID")?)
        .ok_or_else(|| invalid(AT, "callID", "a tool call identity"))?;
    let receipt = decode_artifact(required(root, AT, "receipt")?, AT)?;
    PatchReceiptFact::new(session, input, call, receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Value {
        serde_json::json!({"sessionID":"ses_patch","inputID":"msg_patch","callID":"call_patch","receipt":{"id":"artifact","length":128,"digest":"digest","disposition":"complete"}})
    }
    #[test]
    fn pu_receipt_coordinate_boundary_matches_construction_and_decoding() {
        let mut value = valid();
        value["sessionID"] = format!("ses_{}", "x".repeat(508)).into();
        value["inputID"] = format!("msg_{}", "x".repeat(508)).into();
        value["callID"] = "x".repeat(512).into();
        let fact = decode(&value).unwrap_or_else(|error| panic!("boundary: {error}"));
        assert!(
            PatchReceiptFact::validate_coordinates(fact.session(), fact.input(), fact.call())
                .is_ok()
        );
        for field in ["sessionID", "inputID", "callID"] {
            let mut excessive = value.clone();
            let identity = excessive[field]
                .as_str()
                .unwrap_or_else(|| panic!("fixture identity"));
            excessive[field] = format!("{identity}x").into();
            assert!(
                decode(&excessive).is_err(),
                "{field} exceeded its declared bound"
            );
        }
    }

    #[test]
    fn pu_applied_receipt_roundtrips_and_rejects_missing_null_extra_and_truncation() {
        let value = valid();
        let fact = decode(&value).unwrap_or_else(|error| panic!("receipt decode: {error}"));
        assert_eq!(encode(&fact), value);
        for field in ["sessionID", "inputID", "callID", "receipt"] {
            let mut missing = value.clone();
            missing
                .as_object_mut()
                .unwrap_or_else(|| panic!("fixture object"))
                .remove(field);
            assert!(decode(&missing).is_err());
            missing[field] = Value::Null;
            assert!(decode(&missing).is_err());
        }
        let mut extra = value.clone();
        extra["extra"] = 1.into();
        assert!(decode(&extra).is_err());
        let mut large = value.clone();
        large["receipt"]["length"] = (MAX_RECEIPT_BYTES + 1).into();
        assert!(decode(&large).is_err());
        let mut truncated = value;
        truncated["receipt"]["disposition"] = serde_json::json!("truncated");
        truncated["receipt"]["captured"] = 128.into();
        assert!(decode(&truncated).is_err());
    }
}
