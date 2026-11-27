//! Display-only approval context; the pending effect remains the authority.
//! Copyright 2025 OpenAI. Modified by Grapt: additive, bounded wire previews.
use super::prelude::*;
use grapt_schema::{Optional, Required};

/// The existing metadata scalar bound also bounds a command or patch preview.
pub const MAX_PERMISSION_PREVIEW_TEXT_BYTES: usize = 8192;
/// A complete preview shares the existing 16 KiB metadata envelope with policy.
pub const MAX_PERMISSION_PREVIEW_BYTES: usize = 8192;

string_enum! { pub enum PermissionShellPreviewDtoType { Shell => "shell", } }
string_enum! { pub enum PermissionPatchPreviewDtoType { Patch => "patch", } }
string_enum! { pub enum PermissionUnavailablePreviewDtoType { Unavailable => "unavailable", } }
string_enum! {
    pub enum PermissionPatchFormatDto {
        ApplyPatch => "apply-patch",
        Operations => "operations",
        Replacement => "replacement",
        Write => "write",
    }
}

closed_object! {
    pub struct PermissionShellPreviewDto => "Permission.Preview.Shell" {
        required kind: PermissionShellPreviewDtoType => "type",
        required command: BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES> => "command",
        required cwd: BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES> => "cwd",
        required shell: BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES> => "shell",
        required truncated: bool => "truncated",
        optional classifier_verdict: BoundedString<128> => "classifierVerdict",
    }
}

closed_object! {
    pub struct PermissionPatchPreviewDto => "Permission.Preview.Patch" {
        required kind: PermissionPatchPreviewDtoType => "type",
        required format: PermissionPatchFormatDto => "format",
        required patch: BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES> => "patch",
        required truncated: bool => "truncated",
    }
}

closed_object! {
    pub struct PermissionUnavailablePreviewDto => "Permission.Preview.Unavailable" {
        required kind: PermissionUnavailablePreviewDtoType => "type",
        required reason: BoundedString<256> => "reason",
    }
}

// Previews travel through bounded stored JSON metadata, which can reorder
// keys. Read bounded fields directly rather than requiring a tag-first map or
// buffering an untyped JSON tree. Other union codecs keep their own contract.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionPreviewDto {
    Shell(Box<PermissionShellPreviewDto>),
    Patch(Box<PermissionPatchPreviewDto>),
    Unavailable(Box<PermissionUnavailablePreviewDto>),
}

impl serde::Serialize for PermissionPreviewDto {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Shell(value) => serde::Serialize::serialize(value, serializer),
            Self::Patch(value) => serde::Serialize::serialize(value, serializer),
            Self::Unavailable(value) => serde::Serialize::serialize(value, serializer),
        }
    }
}

impl PermissionPreviewDto {
    pub const VARIANT_COUNT: usize = 3;
    pub const fn variant_name(&self) -> &'static str {
        match self {
            Self::Shell(_) => "shell",
            Self::Patch(_) => "patch",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

impl<'de> serde::Deserialize<'de> for PermissionPreviewDto {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PreviewVisitor;
        impl<'de> serde::de::Visitor<'de> for PreviewVisitor {
            type Value = PermissionPreviewDto;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a closed bounded approval preview")
            }
            #[expect(clippy::too_many_lines, reason = "legacy")]
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                use serde::de::Error;
                let mut kind: Option<BoundedString<16>> = None;
                let mut command: Option<BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES>> = None;
                let mut cwd: Option<BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES>> = None;
                let mut shell: Option<BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES>> = None;
                let mut patch: Option<BoundedString<MAX_PERMISSION_PREVIEW_TEXT_BYTES>> = None;
                let mut format: Option<PermissionPatchFormatDto> = None;
                let mut truncated: Option<bool> = None;
                let mut classifier: Option<BoundedString<128>> = None;
                let mut reason: Option<BoundedString<256>> = None;
                const FIELDS: &[&str] = &[
                    "type",
                    "command",
                    "cwd",
                    "shell",
                    "patch",
                    "format",
                    "truncated",
                    "classifierVerdict",
                    "reason",
                ];
                macro_rules! field {
                    ($slot:ident, $name:literal) => {
                        if $slot
                            .replace(serde::de::MapAccess::next_value(&mut map)?)
                            .is_some()
                        {
                            return Err(A::Error::duplicate_field($name));
                        }
                    };
                }
                while let Some(key) =
                    serde::de::MapAccess::next_key::<BoundedString<256>>(&mut map)?
                {
                    match key.as_str() {
                        "type" => field!(kind, "type"),
                        "command" => field!(command, "command"),
                        "cwd" => field!(cwd, "cwd"),
                        "shell" => field!(shell, "shell"),
                        "patch" => field!(patch, "patch"),
                        "format" => field!(format, "format"),
                        "truncated" => field!(truncated, "truncated"),
                        "classifierVerdict" => field!(classifier, "classifierVerdict"),
                        "reason" => field!(reason, "reason"),
                        unknown => return Err(A::Error::unknown_field(unknown, FIELDS)),
                    }
                }
                let kind = kind.ok_or_else(|| A::Error::missing_field("type"))?;
                match kind.as_str() {
                    "shell" => {
                        if patch.is_some() || format.is_some() || reason.is_some() {
                            return Err(A::Error::custom(
                                "field does not belong to a shell preview",
                            ));
                        }
                        Ok(PermissionPreviewDto::Shell(Box::new(
                            PermissionShellPreviewDto::new(
                                Required::new(PermissionShellPreviewDtoType::Shell),
                                Required::new(
                                    command.ok_or_else(|| A::Error::missing_field("command"))?,
                                ),
                                Required::new(cwd.ok_or_else(|| A::Error::missing_field("cwd"))?),
                                Required::new(
                                    shell.ok_or_else(|| A::Error::missing_field("shell"))?,
                                ),
                                Required::new(
                                    truncated
                                        .ok_or_else(|| A::Error::missing_field("truncated"))?,
                                ),
                                classifier.map_or_else(Optional::absent, Optional::present),
                            ),
                        )))
                    }
                    "patch" => {
                        if command.is_some()
                            || cwd.is_some()
                            || shell.is_some()
                            || classifier.is_some()
                            || reason.is_some()
                        {
                            return Err(A::Error::custom(
                                "field does not belong to a patch preview",
                            ));
                        }
                        Ok(PermissionPreviewDto::Patch(Box::new(
                            PermissionPatchPreviewDto::new(
                                Required::new(PermissionPatchPreviewDtoType::Patch),
                                Required::new(
                                    format.ok_or_else(|| A::Error::missing_field("format"))?,
                                ),
                                Required::new(
                                    patch.ok_or_else(|| A::Error::missing_field("patch"))?,
                                ),
                                Required::new(
                                    truncated
                                        .ok_or_else(|| A::Error::missing_field("truncated"))?,
                                ),
                            ),
                        )))
                    }
                    "unavailable" => {
                        if command.is_some()
                            || cwd.is_some()
                            || shell.is_some()
                            || patch.is_some()
                            || format.is_some()
                            || classifier.is_some()
                            || truncated.is_some()
                        {
                            return Err(A::Error::custom(
                                "field does not belong to an unavailable preview",
                            ));
                        }
                        Ok(PermissionPreviewDto::Unavailable(Box::new(
                            PermissionUnavailablePreviewDto::new(
                                Required::new(PermissionUnavailablePreviewDtoType::Unavailable),
                                Required::new(
                                    reason.ok_or_else(|| A::Error::missing_field("reason"))?,
                                ),
                            ),
                        )))
                    }
                    unknown => Err(A::Error::unknown_variant(
                        unknown,
                        &["shell", "patch", "unavailable"],
                    )),
                }
            }
        }
        let preview = deserializer.deserialize_map(PreviewVisitor)?;
        // Every field above is bounded, so canonical size validation cannot
        // allocate an unbounded intermediate value.
        let bytes = serde_json::to_vec(&preview).map_err(<D::Error as serde::de::Error>::custom)?;
        if bytes.len() > MAX_PERMISSION_PREVIEW_BYTES {
            return Err(<D::Error as serde::de::Error>::custom(
                "approval preview exceeds its byte bound",
            ));
        }
        Ok(preview)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn previews_are_optional_on_legacy_rows_and_typed_when_present(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut row = json!({"id":"per_preview", "sessionID":"ses_preview",
            "action":"shell", "resources":["pwd"], "status":"pending"});
        let legacy: super::super::PermissionRequestStateDto = serde_json::from_value(row.clone())?;
        let super::super::PermissionRequestStateDto::Pending(legacy) = legacy else {
            panic!("pending");
        };
        assert!(legacy.preview().as_present().is_none());
        row["preview"] = json!({"type":"shell","command":"pwd", "cwd":"/repo", "shell":"/bin/sh", "truncated":false});
        let current: super::super::PermissionRequestStateDto = serde_json::from_value(row.clone())?;
        let super::super::PermissionRequestStateDto::Pending(current) = current else {
            panic!("pending");
        };
        let Some(PermissionPreviewDto::Shell(shell)) = current.preview().as_present() else {
            panic!("shell preview");
        };
        assert_eq!(shell.command().get().as_str(), "pwd");
        assert!(shell.classifier_verdict().as_present().is_none());
        row["preview"]["classifierVerdict"] = json!("read-only");
        serde_json::from_value::<super::super::PermissionRequestStateDto>(row)?;
        Ok(())
    }

    #[test]
    fn preview_limits_and_closed_variants_reject_unreadable_payloads() {
        let valid =
            json!({"type":"patch","format":"apply-patch", "patch":"+hello", "truncated":false});
        assert!(serde_json::from_value::<PermissionPreviewDto>(valid.clone()).is_ok());
        let mut oversized = valid.clone();
        oversized["patch"] = json!("x".repeat(MAX_PERMISSION_PREVIEW_TEXT_BYTES + 1));
        assert!(serde_json::from_value::<PermissionPreviewDto>(oversized).is_err());
        let mut unknown = valid;
        unknown["grantsPermission"] = json!(true);
        assert!(serde_json::from_value::<PermissionPreviewDto>(unknown).is_err());
        assert!(serde_json::from_value::<PermissionPreviewDto>(
            json!({"type":"shell", "command":"pwd"})
        )
        .is_err());
    }

    #[test]
    fn stored_field_order_round_trips_every_closed_variant(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for value in [
            json!({"type":"shell","command":"pwd","cwd":"/repo","shell":"/bin/sh","truncated":false,"classifierVerdict":"read-only"}),
            json!({"type":"patch","format":"apply-patch","patch":"+hello","truncated":false}),
            json!({"type":"unavailable","reason":"legacy context"}),
        ] {
            let parsed: PermissionPreviewDto = serde_json::from_value(value.clone())?;
            let wire = serde_json::to_string(&parsed)?;
            assert!(wire.starts_with("{\"type\":"));
            assert_eq!(serde_json::to_value(&parsed)?, value);
            assert_eq!(serde_json::from_str::<PermissionPreviewDto>(&wire)?, parsed);
        }
        let tag_last =
            r#"{"command":"pwd","cwd":"/repo","shell":"/bin/sh","truncated":false,"type":"shell"}"#;
        serde_json::from_str::<PermissionPreviewDto>(tag_last)?;
        Ok(())
    }

    #[test]
    fn reordered_previews_reject_duplicates_nulls_cross_variant_fields_and_total_overflow() {
        for wire in [
            r#"{"command":"pwd","cwd":"/repo","shell":"/bin/sh","truncated":false,"type":"shell","command":"again"}"#,
            r#"{"command":"pwd","cwd":"/repo","shell":"/bin/sh","truncated":false,"type":"shell","classifierVerdict":null}"#,
            r#"{"type":"unavailable","reason":"legacy","command":"pwd"}"#,
            r#"{"type":"patch","format":"apply-patch","patch":"x","truncated":false,"shell":"sh"}"#,
            r#"{"type":"unknown","reason":"legacy"}"#,
        ] {
            assert!(
                serde_json::from_str::<PermissionPreviewDto>(wire).is_err(),
                "accepted {wire}"
            );
        }
        let total = json!({"type":"shell","command":"x".repeat(4100),"cwd":"x".repeat(4100),"shell":"sh","truncated":false});
        assert!(serde_json::from_value::<PermissionPreviewDto>(total).is_err());
    }
}
