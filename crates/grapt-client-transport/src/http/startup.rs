//! Construction of the connection, and the active startup allowance seam.
//!
//! Observed startup forms (an MCP inspect or list) spend an ACTIVE allowance,
//! and that clock PAUSES while a human is asked, charging the longer human
//! allowance instead. Production reaches [`STARTUP_ACTIVE_ALLOWANCE`]; a
//! witness builds a smaller one through the validated constructor.
use super::*;

impl HttpConnection {
    /// Attach to a live endpoint. No I/O happens here.
    ///
    /// Attaching is separated from subscribing because the two fail for
    /// different reasons and a caller should be able to tell them apart: a bad
    /// endpoint or credential is a configuration fault, while a subscription
    /// that will not open is an ordinary transport condition the client
    /// reconciles through.
    pub fn attach(address: SocketAddr, credential: BasicCredential, bounds: Bounds) -> Self {
        Self::build(address, credential, bounds, STARTUP_ACTIVE_ALLOWANCE)
    }

    /// Attach with an explicit active-acquisition allowance for observed
    /// startup forms.
    ///
    /// The allowance is refused at zero: a zero allowance would refuse every
    /// startup form while still looking configured, which is worse than
    /// refusing to start.
    pub fn attach_with_startup_allowance(
        address: SocketAddr,
        credential: BasicCredential,
        bounds: Bounds,
        startup_active_allowance: Duration,
    ) -> Result<Self, HttpError> {
        if startup_active_allowance.is_zero() {
            return Err(HttpError::InvalidStartupAllowance);
        }
        Ok(Self::build(
            address,
            credential,
            bounds,
            startup_active_allowance,
        ))
    }

    /// The one construction path. `attach` reaches it with the production
    /// allowance, whose own constant makes it nonzero; the fallible builder
    /// validates first.
    fn build(
        address: SocketAddr,
        credential: BasicCredential,
        bounds: Bounds,
        startup_active_allowance: Duration,
    ) -> Self {
        Self {
            address,
            credential,
            bounds,
            startup_active_allowance,
            stream: None,
            watched: WatchedSessions::default(),
            reader: SseReader::new(bounds),
            expected_product: None,
            subscription_attempt: None,
            pending_read: None,
            pending_catalog: None,
            pending_tools: None,
            pending_operator: None,
            identity_pending: false,
            refusal: None,
            resume: None,
            stream_binding: None,
        }
    }

    /// Attach by the endpoint spelling the product's readiness receipt carries.
    pub fn attach_endpoint(
        endpoint: &str,
        credential: BasicCredential,
        bounds: Bounds,
    ) -> Result<Self, HttpError> {
        Ok(Self::attach(
            endpoint_address(endpoint)?,
            credential,
            bounds,
        ))
    }
}
