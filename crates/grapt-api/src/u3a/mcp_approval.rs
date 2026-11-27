//! An explicit operator preview; configured command arguments stay redacted in logs.
use super::BoundedString;
use grapt_types::Secret;
use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug)]
pub struct McpApprovalPreviewDto {
    pub fingerprint: BoundedString<64>,
    pub source: BoundedString<4096>,
    pub directory: BoundedString<8192>,
    pub description: Secret<BoundedString<8192>>,
    pub approved: bool,
}

impl Serialize for McpApprovalPreviewDto {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut value = serializer.serialize_struct("McpApprovalPreview", 5)?;
        value.serialize_field("fingerprint", &self.fingerprint)?;
        value.serialize_field("source", &self.source)?;
        value.serialize_field("directory", &self.directory)?;
        value.serialize_field("description", self.description.expose())?;
        value.serialize_field("approved", &self.approved)?;
        value.end()
    }
}

impl<'de> Deserialize<'de> for McpApprovalPreviewDto {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            fingerprint: BoundedString<64>,
            source: BoundedString<4096>,
            directory: BoundedString<8192>,
            description: BoundedString<8192>,
            approved: bool,
        }
        let value = Wire::deserialize(decoder)?;
        if value.fingerprint.as_str().len() != 64
            || !value
                .fingerprint
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(serde::de::Error::custom("invalid MCP approval fingerprint"));
        }
        Ok(Self {
            fingerprint: value.fingerprint,
            source: value.source,
            directory: value.directory,
            description: Secret::new(value.description),
            approved: value.approved,
        })
    }
}
