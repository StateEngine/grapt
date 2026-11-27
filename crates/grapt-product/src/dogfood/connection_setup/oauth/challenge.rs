//! The existing issuer owner serves either browser PKCE or device approval.
use super::*;
use grapt_types::Secret;

pub(super) enum Challenge {
    Device(chatgpt::DeviceCode),
    Browser(chatgpt::BrowserSignIn),
}
impl Endpoints {
    async fn browser(
        &self,
        verifier: Secret<[u8; 32]>,
        state: Secret<[u8; 32]>,
    ) -> Result<chatgpt::BrowserSignIn, chatgpt::SignInError> {
        #[cfg(feature = "loopback-provider-testkit")]
        if self.issuer != chatgpt::ISSUER {
            return chatgpt::BrowserSignIn::listen_loopback(&self.issuer, verifier, state).await;
        }
        chatgpt::BrowserSignIn::listen(verifier, state).await
    }
}
impl Challenge {
    pub(super) async fn start(
        method: &str,
        endpoints: &Endpoints,
    ) -> Result<Self, InvalidRequestErrorDto> {
        if method == "chatgpt-browser" {
            let verifier = grapt_store::OsEntropy::draw_256()
                .map_err(|_| invalid("secure sign-in state is unavailable"))?;
            let state = grapt_store::OsEntropy::draw_256()
                .map_err(|_| invalid("secure sign-in state is unavailable"))?;
            return endpoints
                .browser(verifier, state)
                .await
                .map(Self::Browser)
                .map_err(|_| {
                    invalid("browser sign-in could not start; complete or cancel another attempt")
                });
        }
        chatgpt::request_code(&endpoints.client, &endpoints.issuer)
            .await
            .map(Self::Device)
            .map_err(|_| authorization_failed())
    }
    pub(super) fn user_code(&self) -> Option<&str> {
        match self {
            Self::Device(code) => Some(&code.user_code),
            Self::Browser(_) => None,
        }
    }
    pub(super) fn public(&self) -> (&str, String) {
        match self {
            Self::Device(code) => (
                &code.verification_url,
                format!(
                    "Enter {} in your browser to sign in with ChatGPT.",
                    code.user_code
                ),
            ),
            Self::Browser(flow) => (
                flow.authorization_url().expose(),
                "Complete sign-in in your browser, then return to Grapt.".into(),
            ),
        }
    }
    pub(super) async fn tokens(
        self,
        endpoints: &Endpoints,
    ) -> Result<chatgpt::Tokens, chatgpt::SignInError> {
        match self {
            Self::Device(code) => {
                chatgpt::wait_for_approval(
                    &endpoints.client,
                    &endpoints.issuer,
                    &code,
                    &grapt_store::SystemClock,
                )
                .await
            }
            Self::Browser(flow) => {
                flow.wait(&endpoints.client, &grapt_store::SystemClock)
                    .await
            }
        }
    }
}
