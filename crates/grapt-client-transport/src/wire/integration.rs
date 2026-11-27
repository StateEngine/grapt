//! Integration coordinates retain their owning API identities until encoding.
use super::Spelled;
use grapt_api::u3a::{CredentialIdDto, IntegrationAttemptIdDto, IntegrationIdDto};

impl Spelled for IntegrationIdDto {
    fn spelled(&self) -> &str {
        self.as_str()
    }
}
impl Spelled for CredentialIdDto {
    fn spelled(&self) -> &str {
        self.as_str()
    }
}
impl Spelled for IntegrationAttemptIdDto {
    fn spelled(&self) -> &str {
        self.as_inner().as_str()
    }
}
