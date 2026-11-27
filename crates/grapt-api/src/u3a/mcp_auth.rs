//! Explicit authenticated sign-in operations. Authorization URLs contain OAuth
//! state and stay redacted outside this API's intentional response encoding.
use super::{BoundedString, WireId};
use grapt_types::{McpLoginId, Secret};
use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpAuthRequestDto {
    Login {
        client_id: Option<BoundedString<8192>>,
        scope: Option<BoundedString<4096>>,
    },
    Status {
        login_id: WireId<McpLoginId>,
    },
    Logout {},
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthState {
    AwaitingBrowser,
    SignedIn,
    Failed,
    SignedOut,
}

#[derive(Debug)]
pub struct McpAuthResponseDto {
    pub login_id: Option<WireId<McpLoginId>>,
    pub state: McpAuthState,
    pub authorization_url: Option<Secret<String>>,
}
impl Serialize for McpAuthResponseDto {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut value = serializer.serialize_struct("McpAuthResponse", 3)?;
        value.serialize_field("login_id", &self.login_id)?;
        value.serialize_field("state", &self.state)?;
        // An authenticated operator requested this URL explicitly. This local
        // wire boundary is the sole intentional release of its OAuth state.
        value.serialize_field(
            "authorization_url",
            &self.authorization_url.as_ref().map(Secret::expose),
        )?;
        value.end()
    }
}
impl<'de> Deserialize<'de> for McpAuthResponseDto {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            login_id: Option<WireId<McpLoginId>>,
            state: McpAuthState,
            authorization_url: Option<BoundedString<32768>>,
        }
        let wire = Wire::deserialize(decoder)?;
        let login = wire.login_id.is_some();
        let url = wire.authorization_url.is_some();
        if matches!(
            wire.state,
            McpAuthState::AwaitingBrowser | McpAuthState::SignedIn | McpAuthState::Failed
        ) && !login
            || url && !matches!(wire.state, McpAuthState::AwaitingBrowser)
            || matches!(wire.state, McpAuthState::SignedOut) && login
        {
            return Err(serde::de::Error::custom(
                "invalid MCP authentication response",
            ));
        }
        Ok(Self {
            login_id: wire.login_id,
            state: wire.state,
            authorization_url: wire.authorization_url.map(|u| Secret::new(u.into_string())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auth_wire_is_closed_bounded_and_redacts_browser_secrets(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for value in [
            r#"{"action":"logout","url":"https://injected.example"}"#.to_owned(),
            format!(r#"{{"action":"login","client_id":"{}"}}"#, "x".repeat(8193)),
            format!(
                r#"{{"action":"status","login_id":"mcl_{}"}}"#,
                "x".repeat(129)
            ),
        ] {
            assert!(serde_json::from_str::<McpAuthRequestDto>(&value).is_err());
        }
        for value in [
            r#"{"state":"signed_in","login_id":null,"authorization_url":null}"#,
            r#"{"state":"failed","login_id":null,"authorization_url":null}"#,
            r#"{"state":"signed_out","login_id":"mcl_fixture","authorization_url":null}"#,
        ] {
            assert!(serde_json::from_str::<McpAuthResponseDto>(value).is_err());
        }
        let response = McpAuthResponseDto {
            login_id: WireId::parse("mcl_fixture".to_owned()),
            state: McpAuthState::AwaitingBrowser,
            authorization_url: Some(Secret::new(
                "https://auth.example/authorize?state=held-secret".to_owned(),
            )),
        };
        assert!(!format!("{response:?}").contains("held-secret"));
        let bytes = serde_json::to_vec(&response)?;
        let restored: McpAuthResponseDto = serde_json::from_slice(&bytes)?;
        assert_eq!(
            restored
                .authorization_url
                .as_ref()
                .map(|u| u.expose().as_str()),
            Some("https://auth.example/authorize?state=held-secret")
        );
        Ok(())
    }
}
