//! Rebuild a request only from a committed descriptor and verified artifacts.

use super::*;
use grapt_types::{CompactionSummary, EventId, EventSeq, RetainedHistory};

pub(super) const RETENTION_POLICY: &str = "scc-original-users-v4";
pub(super) const MAX_SELECTION_BYTES: u64 = 16 * 1024;
pub(super) const MAX_SUMMARY_BYTES: u64 = 64 * 1024;
/// The most a provider compaction artifact may hold: its items' retained
/// states (each within the adapter state bound) and their framing.
pub(super) const MAX_PROVIDER_SUMMARY_BYTES: u64 = 320 * 1024;

/// What a committed compaction left in place of the history it covered.
pub(super) enum RetainedSummary {
    /// SE's own summary text.
    Text(String),
    /// The provider's opaque compaction items. Only the target that produced
    /// them may be sent them (`ProviderReplay::redeemable_for`).
    Provider(Vec<grapt_provider::ProviderReplay>),
}

#[derive(Debug)]
pub(super) struct Selection {
    pub session: SessionId,
    pub input: MessageId,
    pub compaction: grapt_types::CompactionId,
    pub from: u64,
    pub source_head: u64,
    pub admission: Option<AdmissionDiagnosis>,
    /// What provider compaction did for this descriptor, when it was asked to.
    pub server: Option<ServerOutcome>,
    /// The request planned for SE's own first summary call, so a benchmark can
    /// count cold summaries by cause. The one retry after
    /// an unusable summary is always cold; the call
    /// count shows whether it ran.
    pub summary_plan: Option<SummaryPlan>,
}

/// The request planned for a committed SE summary's first call: the cache-warm
/// extension of the coding request, or the cold request (history quoted as
/// data) and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SummaryPlan {
    Warm,
    ColdNoCodingRequest,
    /// The coding request offered hosted search and the summary attempt could
    /// not grant it (SC-CACHE-07).
    ColdHostedToolOffered,
    ColdIssuedModelChanged,
    ColdWarmOverBudget,
    /// The provider compaction, offered hosted search, answered with anything
    /// but exactly one compaction item.
    ColdCompactionResponseNotSingleItem,
}

impl SummaryPlan {
    const ALL: [Self; 6] = [
        Self::Warm,
        Self::ColdNoCodingRequest,
        Self::ColdHostedToolOffered,
        Self::ColdIssuedModelChanged,
        Self::ColdWarmOverBudget,
        Self::ColdCompactionResponseNotSingleItem,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::ColdNoCodingRequest => "cold:no-coding-request",
            Self::ColdHostedToolOffered => "cold:hosted-tool-offered",
            Self::ColdIssuedModelChanged => "cold:issued-model-changed",
            Self::ColdWarmOverBudget => "cold:warm-over-budget",
            Self::ColdCompactionResponseNotSingleItem => "cold:compaction-response-not-single-item",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|plan| plan.name() == name)
    }
}

/// The outcome of a provider compaction attempt, recorded with the
/// descriptor it led to so a benchmark can count fallbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServerOutcome {
    /// The provider's items were committed, from a call reporting these
    /// input and output tokens.
    Committed {
        items: usize,
        bytes: usize,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        /// Text the provider sent beside its items, which the commit ignored,
        /// so row A can count it. Encoded only when
        /// nonzero.
        ignored_text_bytes: usize,
    },
    /// SE's own summary was committed instead, for this reason.
    Fallback { reason: String },
}

#[derive(Debug)]
pub(super) struct AdmissionDiagnosis {
    pub trigger: grapt_types::CompactionTrigger,
    pub estimate_basis: &'static str,
    pub estimate: u64,
    pub rebuilt_estimate: Option<u64>,
    pub budget: u64,
    pub removed_bytes: usize,
    /// Retained history rows plus the labelled summary. The deterministic facts
    /// block is not included; admission and the rebuilt check count it.
    pub retained_bytes: usize,
}

impl Selection {
    pub(super) fn encode(&self) -> serde_json::Value {
        let mut value = serde_json::json!({"session":self.session.as_str(), "input":self.input.as_str(),
            "compaction":self.compaction.as_str(), "from":self.from, "source_head":self.source_head});
        if let (Some(root), Some(diagnosis)) = (value.as_object_mut(), &self.admission) {
            let mut admission = serde_json::json!({
                "trigger": trigger_name(diagnosis.trigger),
                "estimateBasis": diagnosis.estimate_basis,
                "estimate": diagnosis.estimate,
                "budget": diagnosis.budget,
                "removedBytes": diagnosis.removed_bytes,
                "retainedBytes": diagnosis.retained_bytes,
            });
            if let (Some(object), Some(estimate)) =
                (admission.as_object_mut(), diagnosis.rebuilt_estimate)
            {
                object.insert("rebuiltEstimate".to_owned(), estimate.into());
            }
            root.insert("admission".to_owned(), admission);
        }
        if let (Some(root), Some(server)) = (value.as_object_mut(), &self.server) {
            root.insert(
                "serverSide".to_owned(),
                match server {
                    ServerOutcome::Committed {
                        items,
                        bytes,
                        input_tokens,
                        output_tokens,
                        ignored_text_bytes,
                    } => {
                        let mut outcome = serde_json::json!({"outcome":"committed","items":items,
                            "bytes":bytes,"inputTokens":input_tokens,"outputTokens":output_tokens});
                        if let (Some(object), true) =
                            (outcome.as_object_mut(), *ignored_text_bytes > 0)
                        {
                            object.insert(
                                "ignoredTextBytes".to_owned(),
                                (*ignored_text_bytes).into(),
                            );
                        }
                        outcome
                    }
                    ServerOutcome::Fallback { reason } => {
                        serde_json::json!({"outcome":"fallback","reason":reason})
                    }
                },
            );
        }
        if let (Some(root), Some(plan)) = (value.as_object_mut(), self.summary_plan) {
            root.insert("summaryPlan".to_owned(), plan.name().into());
        }
        value
    }

    pub(super) fn decode(text: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let object = value.as_object().ok_or("selection is not an object")?;
        let optional = usize::from(object.contains_key("admission"))
            + usize::from(object.contains_key("serverSide"))
            + usize::from(object.contains_key("summaryPlan"));
        if object.len() != 5 + optional {
            return Err("selection has missing or unknown coordinates".to_owned());
        }
        let string = |key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| value.len() <= 256)
                .ok_or("selection identity is absent or oversized")
        };
        let admission = object.get("admission").map(decode_admission).transpose()?;
        let server = object.get("serverSide").map(decode_server).transpose()?;
        let summary_plan = object
            .get("summaryPlan")
            .map(|plan| {
                plan.as_str()
                    .and_then(SummaryPlan::parse)
                    .ok_or("selection summary plan is invalid")
            })
            .transpose()?;
        Ok(Self {
            session: string("session")?
                .parse::<SessionId>()
                .map_err(|e| e.to_string())?,
            input: string("input")?
                .parse::<MessageId>()
                .map_err(|e| e.to_string())?,
            compaction: grapt_types::CompactionId::new(string("compaction")?)
                .ok_or("selection compaction identity is invalid")?,
            from: object
                .get("from")
                .and_then(serde_json::Value::as_u64)
                .ok_or("selection start is invalid")?,
            source_head: object
                .get("source_head")
                .and_then(serde_json::Value::as_u64)
                .ok_or("selection source head is invalid")?,
            admission,
            server,
            summary_plan,
        })
    }
}

fn decode_server(value: &serde_json::Value) -> Result<ServerOutcome, String> {
    let object = value
        .as_object()
        .ok_or("selection server outcome is not an object")?;
    let count = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or("selection server outcome count is invalid")
    };
    match object.get("outcome").and_then(serde_json::Value::as_str) {
        Some("committed")
            if object.len() == 5 + usize::from(object.contains_key("ignoredTextBytes")) =>
        {
            let tokens = |key: &str| match object.get(key) {
                Some(serde_json::Value::Null) => Ok(None),
                Some(value) => value
                    .as_u64()
                    .map(Some)
                    .ok_or("selection server token count is invalid"),
                None => Err("selection server token count is missing"),
            };
            Ok(ServerOutcome::Committed {
                items: count("items")?,
                bytes: count("bytes")?,
                input_tokens: tokens("inputTokens")?,
                output_tokens: tokens("outputTokens")?,
                ignored_text_bytes: match object.get("ignoredTextBytes") {
                    None => 0,
                    Some(_) => match count("ignoredTextBytes")? {
                        0 => return Err("selection server ignored text is invalid".to_owned()),
                        bytes => bytes,
                    },
                },
            })
        }
        Some("fallback") if object.len() == 2 => Ok(ServerOutcome::Fallback {
            reason: object
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .filter(|reason| !reason.is_empty() && reason.len() <= 64)
                .ok_or("selection server fallback reason is invalid")?
                .to_owned(),
        }),
        _ => Err("selection server outcome is malformed".to_owned()),
    }
}

pub(super) struct RetainedContext {
    pub compaction: grapt_types::CompactionId,
    pub identity: grapt_types::RequestContextIdentity,
    pub original_users: bool,
    pub generation: EventId,
    /// The input kept verbatim when this descriptor was committed, including
    /// when the cut lies after its promotion. It stays in the exact tail.
    pub input: MessageId,
    pub from: EventSeq,
    /// The newest durable event the committed compaction had seen.
    pub source_head: EventSeq,
    /// The durable event that replaced the conversation prefix.
    pub rewritten_at: EventSeq,
    pub summary: RetainedSummary,
}

impl RetainedContext {
    pub(super) fn load(
        inbox: &Inbox,
        artifacts: &FilesystemArtifactStore,
        session: &SessionId,
        input: &MessageId,
    ) -> Result<Option<Self>, String> {
        let Some((generation, committed, descriptor)) =
            grapt_store::compaction::latest_visible_descriptor(inbox.store().connection(), session)
                .map_err(|error| format!("committed context is unreadable: {error}"))?
        else {
            return Ok(None);
        };
        if !matches!(
            descriptor.retention_policy().as_str(),
            RETENTION_POLICY
                | "scc-working-tail-v3"
                | "scc-completed-prefix-v2"
                | "scc-recent-step-v1"
        ) {
            return Err("committed context uses an unsupported retention policy".to_owned());
        }
        let RetainedHistory::DeterministicSelection {
            descriptor: selection,
        } = descriptor.retained()
        else {
            return Err("committed context has no deterministic selection".to_owned());
        };
        if descriptor.baseline() != selection || descriptor.next_request().context() != selection {
            return Err("retained baseline and request context disagree with selection".to_owned());
        }
        if selection.length() > MAX_SELECTION_BYTES {
            return Err("retained selection exceeds its artifact bound".to_owned());
        }
        let raw = read_text(
            artifacts,
            selection,
            CaptureDisposition::Complete,
            "retained selection",
        )?;
        let selection = Selection::decode(raw.as_str())
            .map_err(|error| format!("retained selection is malformed: {error}"))?;
        if &selection.session != session
            || &selection.compaction != descriptor.id()
            || selection.from > selection.source_head
            || selection.source_head >= committed.value()
        {
            return Err("retained selection does not bind the committed lineage".to_owned());
        }
        let selected = inbox
            .promoted_input(session, &selection.input)
            .map_err(|error| error.to_string())?;
        let current = inbox
            .promoted_input(session, input)
            .map_err(|error| error.to_string())?;
        if selected.seq > selection.source_head || current.seq < selected.seq {
            return Err("retained context cannot be applied to an earlier input".to_owned());
        }
        let summary = match descriptor.summary() {
            CompactionSummary::ClientReadable { artifact } => {
                if artifact.length() > MAX_SUMMARY_BYTES {
                    return Err("compaction summary exceeds its artifact bound".to_owned());
                }
                let summary = read_text(
                    artifacts,
                    artifact,
                    CaptureDisposition::Complete,
                    "compaction summary",
                )?;
                if summary.as_str().trim().is_empty() {
                    return Err("committed compaction summary is empty".to_owned());
                }
                RetainedSummary::Text(summary.as_str().to_owned())
            }
            // An unreadable or corrupt provider compaction is not a failure:
            // the history it covered is still durable, so the request is
            // rebuilt from that history as if the compaction were absent.
            CompactionSummary::ProviderBound {
                artifact,
                compatibility,
            } => match provider_items(artifacts, artifact, compatibility) {
                Some(items) => RetainedSummary::Provider(items),
                None => return Ok(None),
            },
        };
        Ok(Some(Self {
            compaction: descriptor.id().clone(),
            identity: grapt_types::RequestContextIdentity {
                window: descriptor.target_window().clone(),
                epoch: descriptor.target_epoch(),
                descriptor: generation.clone(),
            },
            generation,
            original_users: descriptor.retention_policy().as_str() == RETENTION_POLICY,
            input: selection.input,
            from: EventSeq::new(selection.from),
            source_head: EventSeq::new(selection.source_head),
            rewritten_at: committed,
            summary,
        }))
    }
}

/// The compaction items a provider-bound descriptor committed, restored for
/// the generating target, or `None` when the artifact cannot be read or any
/// item does not restore.
fn provider_items(
    artifacts: &FilesystemArtifactStore,
    artifact: &grapt_types::ArtifactRef,
    compatibility: &grapt_types::SummaryCompatibility,
) -> Option<Vec<grapt_provider::ProviderReplay>> {
    if artifact.length() > MAX_PROVIDER_SUMMARY_BYTES {
        return None;
    }
    let text = read_text(
        artifacts,
        artifact,
        CaptureDisposition::Complete,
        "provider compaction",
    )
    .ok()?;
    let value: serde_json::Value = serde_json::from_str(text.as_str()).ok()?;
    let states = value.get("items")?.as_array()?;
    if states.is_empty() || states.len() > super::compaction::MAX_COMPACTION_ITEMS {
        return None;
    }
    states
        .iter()
        .map(|state| {
            grapt_provider::ProviderReplay::restore(
                compatibility.provider().clone(),
                compatibility.model().clone(),
                compatibility.variant().cloned(),
                state.as_str()?,
            )
            .ok()
            .filter(grapt_provider::ProviderReplay::is_responses_compaction)
        })
        .collect()
}

fn decode_admission(value: &serde_json::Value) -> Result<AdmissionDiagnosis, String> {
    let object = value
        .as_object()
        .ok_or("selection admission diagnosis is not an object")?;
    if !matches!(object.len(), 6 | 7)
        || (object.len() == 7 && !object.contains_key("rebuiltEstimate"))
    {
        return Err("selection admission diagnosis has missing or unknown fields".to_owned());
    }
    let string = |key| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .ok_or("selection admission diagnosis string is absent")
    };
    let number = |key| {
        object
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .ok_or("selection admission diagnosis number is absent")
    };
    let trigger = match string("trigger")? {
        "contextOverflow" => grapt_types::CompactionTrigger::ContextOverflow,
        "policyThreshold" => grapt_types::CompactionTrigger::PolicyThreshold,
        "userRequested" => grapt_types::CompactionTrigger::UserRequested,
        "emergencyHeadroom" => grapt_types::CompactionTrigger::EmergencyHeadroom,
        "structuralBound" => grapt_types::CompactionTrigger::StructuralBound,
        _ => return Err("selection admission trigger is unknown".to_owned()),
    };
    let estimate_basis = match string("estimateBasis")? {
        "confirmed-prefix" => "confirmed-prefix",
        "estimated" => "estimated",
        "structural-window" => "structural-window",
        _ => return Err("selection admission estimate basis is unknown".to_owned()),
    };
    Ok(AdmissionDiagnosis {
        trigger,
        estimate_basis,
        estimate: number("estimate")?,
        rebuilt_estimate: object
            .get("rebuiltEstimate")
            .map(|_| number("rebuiltEstimate"))
            .transpose()?,
        budget: number("budget")?,
        removed_bytes: usize::try_from(number("removedBytes")?)
            .map_err(|_| "selection removed byte count exceeds this platform")?,
        retained_bytes: usize::try_from(number("retainedBytes")?)
            .map_err(|_| "selection retained byte count exceeds this platform")?,
    })
}

fn trigger_name(trigger: grapt_types::CompactionTrigger) -> &'static str {
    match trigger {
        grapt_types::CompactionTrigger::ContextOverflow => "contextOverflow",
        grapt_types::CompactionTrigger::PolicyThreshold => "policyThreshold",
        grapt_types::CompactionTrigger::UserRequested => "userRequested",
        grapt_types::CompactionTrigger::EmergencyHeadroom => "emergencyHeadroom",
        grapt_types::CompactionTrigger::StructuralBound => "structuralBound",
    }
}

#[cfg(test)]
mod tests;
