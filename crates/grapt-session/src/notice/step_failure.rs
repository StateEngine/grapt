//! Advisory, fixed provider diagnosis after a committed Step failure.
use super::*;
use grapt_types::{
    failure_diagnostic::{
        AdmissionRefusalWord, RetryExhaustion, RetryFailureClass, StaleCoordinateWord,
        StepFailureDiagnostic, UsageDiagnostic, UsageIssue,
    },
    provider_limit::ProviderLimitReset,
    StepNumber,
};

pub(super) const STEP_FAILURE: &str = "step-failure-detail";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailureNotice {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
    pub step: StepNumber,
    /// A future code is retained as an explicit unsupported explanation.
    pub diagnostic: Option<StepFailureDiagnostic>,
}
impl StepFailureNotice {
    pub fn text(&self) -> String {
        match self.diagnostic {
            None => "The Step failed. Its explanation requires a newer version.".into(),
            Some(StepFailureDiagnostic::ProviderLimit { reset }) => {
                crate::execution::provider_limit_message(reset)
            }
            Some(StepFailureDiagnostic::Usage(diagnostic)) => {
                let issue = match diagnostic.issue {
                    UsageIssue::Null => "a null count",
                    UsageIssue::WrongType => "a count with an invalid type",
                    UsageIssue::OutOfRange => "a count outside the supported range",
                    UsageIssue::PartitionExceedsTotal => "input counts exceeding their total",
                    UsageIssue::TotalMismatch => "counts inconsistent with their total",
                    UsageIssue::ConflictingSnapshots => {
                        "conflicting counts across response snapshots"
                    }
                };
                format!(
                    "The provider reported {issue} for {}. The Step could not complete.",
                    diagnostic.field_word()
                )
            }
            Some(StepFailureDiagnostic::AdmissionRefused { refusal, coordinate }) => {
                match refusal {
                    AdmissionRefusalWord::NoIssuer => {
                        "The Step was refused before it reached the model: no admission issuer is installed."
                            .to_owned()
                    }
                    AdmissionRefusalWord::MissingCampaignTruth => {
                        "The Step was refused before it reached the model: the campaign truth it needs is missing."
                            .to_owned()
                    }
                    AdmissionRefusalWord::StaleCoordinate => {
                        let which = match coordinate {
                            Some(word) => word.word(),
                            None => "a campaign",
                        };
                        format!(
                            "The Step was refused before it reached the model: its {which} coordinate is stale."
                        )
                    }
                    AdmissionRefusalWord::MissingStepTerminal => {
                        "The Step was refused before it reached the model: the step terminal it would name is missing."
                            .to_owned()
                    }
                    AdmissionRefusalWord::PlanRefused => {
                        "The Step was refused before it reached the model: the attempt plan was refused."
                            .to_owned()
                    }
                    AdmissionRefusalWord::OpenRefused => {
                        "The request was refused before it reached the model. Sending the same prompt again won't help."
                            .to_owned()
                    }
                    AdmissionRefusalWord::Saturated => {
                        "The Step was refused before it reached the model: the provider adapter had no free attempt slot."
                            .to_owned()
                    }
                    AdmissionRefusalWord::StoreRefused => {
                        "The Step was refused before it reached the model: the durable attempt receipt was refused."
                            .to_owned()
                    }
                }
            }
            Some(StepFailureDiagnostic::RetryExhausted(retry)) => {
                let mut transitions = Vec::with_capacity(5);
                for class in retry.classes() {
                    if transitions.last() != Some(class) {
                        transitions.push(*class);
                    }
                }
                let names: Vec<_> = transitions
                    .iter()
                    .map(|class| match class {
                        RetryFailureClass::RateLimited => "rate-limited",
                        RetryFailureClass::ServerError => "server errors",
                        RetryFailureClass::Transport => "transport failures",
                    })
                    .collect();
                format!(
                    "The provider kept failing ({}) after {} {}.",
                    names.join(", then "),
                    retry.attempts(),
                    if retry.attempts() == 1 {
                        "attempt"
                    } else {
                        "attempts"
                    }
                )
            }
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    kind: String,
    step: u32,
    field: String,
    code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    coordinate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    classes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
}
fn decode(
    session: &SessionId,
    position: CommittedPosition,
    data: Data,
) -> Result<StepFailureNotice, String> {
    let word = |s: &str| {
        !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if data.session != session.as_str()
        || data.kind != STEP_FAILURE
        || !word(&data.field)
        || !word(&data.code)
    {
        return Err("Step failure notice identity or code is invalid".into());
    }
    Ok(StepFailureNotice {
        position,
        execution: ExecutionId::new(data.execution).ok_or("invalid notice execution")?,
        step: StepNumber::new(data.step).ok_or("invalid notice Step")?,
        diagnostic: if data.code == "retry-exhausted" {
            if data.resets_at.is_some() {
                return Err("retry diagnostic contains a provider reset".into());
            }
            let classes = data.classes.ok_or("retry diagnostic has no classes")?;
            let attempts = data.attempts.ok_or("retry diagnostic has no count")?;
            if classes.len() > 5
                || attempts < classes.len() as u32
                || attempts > RetryExhaustion::MAX_TOTAL_ATTEMPTS
            {
                return Err("retry diagnostic count or bound is invalid".into());
            }
            if classes.is_empty()
                || classes.iter().any(|class| !word(class))
                || data.last_class.as_deref() != classes.last().map(String::as_str)
                || data.field != "provider"
            {
                return Err("retry diagnostic class or last class is invalid".into());
            }
            // Unknown nonempty vocabulary is a future explanation, never text
            // to display and never a guess at the provider's class.
            classes
                .iter()
                .map(|word| RetryFailureClass::from_word(word))
                .collect::<Option<Vec<_>>>()
                .and_then(|classes| RetryExhaustion::with_total_attempts(&classes, attempts))
                .map(StepFailureDiagnostic::RetryExhausted)
        } else if data.field == "admission" {
            if data.attempts.is_some()
                || data.classes.is_some()
                || data.last_class.is_some()
                || data.resets_at.is_some()
            {
                return Err("admission diagnostic carries provider coordinates".into());
            }
            match AdmissionRefusalWord::from_word(&data.code) {
                None => None,
                Some(refusal) => {
                    let coordinate = match data.coordinate.as_deref() {
                        None => None,
                        Some(word) => Some(
                            StaleCoordinateWord::from_word(word)
                                .ok_or("admission diagnostic coordinate is invalid")?,
                        ),
                    };
                    Some(
                        StepFailureDiagnostic::admission_refused(refusal, coordinate)
                            .ok_or("admission diagnostic coordinate is misplaced")?,
                    )
                }
            }
        } else if data.code == "provider-limit" {
            if data.field != "provider"
                || data.attempts.is_some()
                || data.classes.is_some()
                || data.last_class.is_some()
            {
                return Err("provider limit diagnostic coordinates are invalid".into());
            }
            let reset = data
                .resets_at
                .map(|seconds| {
                    ProviderLimitReset::from_unix_seconds(seconds)
                        .ok_or("provider reset is outside the supported range")
                })
                .transpose()?;
            Some(StepFailureDiagnostic::ProviderLimit { reset })
        } else {
            if data.attempts.is_some() || data.classes.is_some() || data.last_class.is_some() {
                return Err("usage diagnostic contains retry coordinates".into());
            }
            if data.resets_at.is_some() {
                return Err("usage diagnostic contains a provider reset".into());
            }
            UsageDiagnostic::from_words(&data.field, &data.code).map(StepFailureDiagnostic::Usage)
        },
    })
}
/// One advisory append per execution/Step, under the existing elected writer.
/// The failed Step must already exist; this function cannot settle or resurrect it.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn record_step_failure(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    step: StepNumber,
    diagnostic: StepFailureDiagnostic,
) -> Result<Option<StepFailureNotice>, String> {
    if let StepFailureDiagnostic::AdmissionRefused {
        refusal,
        coordinate,
    } = diagnostic
    {
        if StepFailureDiagnostic::admission_refused(refusal, coordinate).is_none() {
            return Err("admission diagnostic coordinate is misplaced".into());
        }
    }
    use rusqlite::OptionalExtension;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let terminal: Option<(String, String)> = tx
        .query_row(
            "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
         AND type IN ('session.step.failed.2','session.step.failed.3')
         AND json_extract(data,'$.step')=?2 ORDER BY seq DESC LIMIT 1",
            (session.as_str(), i64::from(step.value())),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let (kind, raw) = terminal.ok_or("Step failure notice has no committed failure")?;
    let payload = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let reason = if kind == "session.step.failed.3" {
        grapt_event::session_step_observed::decode_failed(&payload)
            .map_err(|error| error.to_string())?
            .fact()
            .reason()
            .clone()
    } else {
        grapt_event::session_step_failed::decode(&payload)
            .map_err(|error| error.to_string())?
            .reason()
            .clone()
    };
    let agrees = match diagnostic {
        StepFailureDiagnostic::Usage(_) => matches!(
            reason,
            grapt_types::StepTerminalReason::ProviderFailure {
                class: grapt_types::ProviderFailureClass::InvalidProviderOutput
            }
        ),
        StepFailureDiagnostic::RetryExhausted(_) => {
            matches!(reason, grapt_types::StepTerminalReason::PolicyLimit { .. })
        }
        StepFailureDiagnostic::ProviderLimit { .. } => matches!(
            reason,
            grapt_types::StepTerminalReason::ProviderFailure {
                class: grapt_types::ProviderFailureClass::Quota
            }
        ),
        StepFailureDiagnostic::AdmissionRefused { .. } => matches!(
            reason,
            grapt_types::StepTerminalReason::ProviderFailure {
                class: grapt_types::ProviderFailureClass::LocalRefusal
            }
        ),
    };
    if !agrees {
        return Err("diagnosis does not match the committed failure".into());
    }
    let prior: Option<(String, i64, i64, String)> = tx
        .query_row(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END, seq, created,
         CASE WHEN length(CAST(data AS BLOB)) <= ?5 THEN data END FROM event
         WHERE aggregate_family='session' AND aggregate_id=?1 AND type='session.notice.recorded.1'
         AND json_extract(data,'$.kind')=?2 AND json_extract(data,'$.executionID')=?3
         AND json_extract(data,'$.step')=?4 ORDER BY seq DESC LIMIT 1",
            (
                session.as_str(),
                STEP_FAILURE,
                execution.as_str(),
                i64::from(step.value()),
                MAX_NOTICE_DATA_BYTES as i64,
            ),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some((id, seq, created_ms, raw)) = prior {
        if created_ms < 0 {
            return Err("notice time is negative".into());
        }
        let position = CommittedPosition {
            event: id
                .parse()
                .map_err(|error: grapt_types::IdError| error.to_string())?,
            seq: u64::try_from(seq).map_err(|error| error.to_string())?,
            created_ms,
        };
        let prior = decode(
            session,
            position,
            serde_json::from_str(&raw).map_err(|error| error.to_string())?,
        )?;
        if prior.diagnostic != Some(diagnostic) {
            return Err("Step failure notice diagnosis changed".into());
        }
        return Ok(None);
    }
    let (field, code, coordinate, attempts, classes, last_class, resets_at) = match diagnostic {
        StepFailureDiagnostic::Usage(usage) => (
            usage.field_word(),
            usage.issue.word().into(),
            None,
            None,
            None,
            None,
            None,
        ),
        StepFailureDiagnostic::RetryExhausted(retry) => (
            "provider".into(),
            "retry-exhausted".into(),
            None,
            Some(retry.attempts()),
            Some(
                retry
                    .classes()
                    .iter()
                    .map(|class| class.word().into())
                    .collect(),
            ),
            Some(retry.last().word().into()),
            None,
        ),
        StepFailureDiagnostic::ProviderLimit { reset } => (
            "provider".into(),
            "provider-limit".into(),
            None,
            None,
            None,
            None,
            reset.map(ProviderLimitReset::unix_seconds),
        ),
        StepFailureDiagnostic::AdmissionRefused {
            refusal,
            coordinate,
        } => (
            "admission".into(),
            refusal.word().into(),
            coordinate.map(|word| word.word().into()),
            None,
            None,
            None,
            None,
        ),
    };
    let data = serde_json::to_value(Data {
        session: session.as_str().into(),
        execution: execution.as_str().into(),
        kind: STEP_FAILURE.into(),
        step: step.value(),
        field,
        code,
        coordinate,
        attempts,
        classes,
        last_class,
        resets_at,
    })
    .map_err(|error| error.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, &data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(Some(StepFailureNotice {
        position,
        execution: execution.clone(),
        step,
        diagnostic: Some(diagnostic),
    }))
}
pub fn step_failure_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<StepFailureNotice>, String> {
    kind_rows::<Data>(connection, session, STEP_FAILURE)?
        .into_iter()
        .map(|(position, data)| decode(session, position, data))
        .collect()
}

/// Decode exactly the notice selected by a public history page.
pub(super) fn at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    sequence: u64,
) -> Result<Vec<StepFailureNotice>, String> {
    kind_rows_at::<Data>(connection, session, STEP_FAILURE, Some(sequence))?
        .into_iter()
        .map(|(position, data)| decode(session, position, data))
        .collect()
}
