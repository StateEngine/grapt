//! A Session's latest provider-confirmed token calibration, kept so a later
//! process does not start from the conservative byte estimate. Never shown in
//! history: it is accounting, not a message.
use super::*;
pub(super) const TOKEN_CALIBRATION: &str = "token-calibration";
/// The largest count JSON readers carry exactly.
const MAX_COUNT: u64 = 9_007_199_254_740_991;

/// An opaque digest naming the exact model target a calibration was measured
/// on: 64 lowercase hexadecimal characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationTarget(String);

impl CalibrationTarget {
    pub fn new(digest: String) -> Option<Self> {
        (digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then_some(Self(digest))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider-confirmed input tokens per locally estimated unit, both non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCalibration {
    pub target: CalibrationTarget,
    pub confirmed: u64,
    pub estimated: u64,
}

impl TokenCalibration {
    fn validate(&self) -> Result<(), String> {
        if [self.confirmed, self.estimated]
            .iter()
            .any(|count| *count == 0 || *count > MAX_COUNT)
        {
            return Err("token calibration counts are invalid".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    kind: String,
    #[serde(rename = "targetKey")]
    target: String,
    confirmed: u64,
    estimated: u64,
}

pub fn record_token_calibration(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    calibration: &TokenCalibration,
) -> Result<CommittedPosition, String> {
    calibration.validate()?;
    let data = Data {
        session: session.as_str().into(),
        kind: TOKEN_CALIBRATION.into(),
        target: calibration.target.as_str().into(),
        confirmed: calibration.confirmed,
        estimated: calibration.estimated,
    };
    record_data_notice(
        store,
        authorities,
        session,
        &serde_json::to_value(data).map_err(|error| error.to_string())?,
    )
}

/// The Session's newest token calibration, or `None` when it has none.
pub fn latest_token_calibration(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<TokenCalibration>, String> {
    let Some((_, data)) =
        matching_rows_at::<Data>(connection, session, TOKEN_CALIBRATION, None, 1, None)?
            .into_iter()
            .next()
    else {
        return Ok(None);
    };
    if data.session != session.as_str() || data.kind != TOKEN_CALIBRATION {
        return Err("token calibration notice identity is invalid".into());
    }
    let calibration = TokenCalibration {
        target: CalibrationTarget::new(data.target).ok_or("token calibration target is invalid")?,
        confirmed: data.confirmed,
        estimated: data.estimated,
    };
    calibration.validate()?;
    Ok(Some(calibration))
}
