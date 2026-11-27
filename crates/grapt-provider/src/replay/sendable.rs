//! Which stored replays one request may carry. A replay another protocol,
//! model or set of credentials produced is dropped, or reduced to its rendered
//! text, before the request is sized or sent; it never refuses the turn.
use super::{ProviderReplay, ReplayBinding, ReplayPayload};

/// Drop every reasoning or thinking replay this attempt cannot send: another
/// protocol family (the two Responses families included), another model, or
/// another endpoint, route, account or credential version than the one that
/// produced it. Native Anthropic text it cannot send becomes its rendered
/// text. Reasoning helps a model continue; its absence is never a reason to
/// refuse the turn.
pub(crate) fn drop_unreplayable_reasoning(input: &mut crate::transport::adapter::AttemptInput) {
    let family = input.request.family();
    let accepts = Accepts::Exact {
        responses: ReplayBinding::for_reasoning(input),
        search: ReplayBinding::for_search(input),
        anthropic: family
            .is_anthropic()
            .then(|| ReplayBinding::from_input(input).ok())
            .flatten(),
        bedrock: (family == crate::ProtocolFamily::BedrockConverse)
            .then(|| ReplayBinding::for_bedrock(input).ok())
            .flatten(),
        gemini: matches!(
            family,
            crate::ProtocolFamily::GoogleGemini | crate::ProtocolFamily::VertexGemini
        )
        .then(|| ReplayBinding::from_input(input).ok())
        .flatten(),
    };
    drop_unaccepted(&mut input.request, &accepts);
}

/// The same drop for sizing a request before any attempt exists: replays its
/// protocol can never send go exactly as they will at send time; ones it may
/// send stay, so the estimate never undercounts what is sent. Returns the
/// original positions of the items it removed, ascending, so a caller's
/// indexes into the input can follow.
pub fn drop_foreign_replays(request: &mut crate::ProviderRequest) -> Vec<usize> {
    let family = request.family();
    drop_unaccepted(request, &Accepts::Family(family))
}

/// Which replays an attempt's coordinates accept, by replay kind.
enum Accepts {
    Exact {
        responses: Option<ReplayBinding>,
        search: Option<ReplayBinding>,
        anthropic: Option<ReplayBinding>,
        bedrock: Option<ReplayBinding>,
        gemini: Option<ReplayBinding>,
    },
    Family(crate::ProtocolFamily),
}

#[derive(Clone, Copy)]
enum Kind {
    Responses,
    Search,
    Anthropic,
    Bedrock,
    Gemini,
}

impl Accepts {
    fn binds(&self, kind: Kind, replay: &ProviderReplay) -> bool {
        use crate::ProtocolFamily as F;
        match self {
            Self::Exact {
                responses,
                search,
                anthropic,
                bedrock,
                gemini,
            } => match kind {
                Kind::Responses => responses,
                Kind::Search => search,
                Kind::Anthropic => anthropic,
                Kind::Bedrock => bedrock,
                Kind::Gemini => gemini,
            }
            .as_ref()
            .is_some_and(|binding| replay.bound_to(binding)),
            Self::Family(family) => match kind {
                Kind::Responses | Kind::Search => {
                    matches!(family, F::OpenAiResponses | F::ChatGptCodex)
                }
                Kind::Anthropic => family.is_anthropic(),
                Kind::Bedrock => *family == F::BedrockConverse,
                Kind::Gemini => matches!(family, F::GoogleGemini | F::VertexGemini),
            },
        }
    }
}

fn drop_unaccepted(request: &mut crate::ProviderRequest, accepts: &Accepts) -> Vec<usize> {
    let model = request.model().clone();
    let effort = crate::effort::policy(request);
    let bound = |replay: &ProviderReplay, kind: Kind| {
        replay.matches_effort(&model, effort) && accepts.binds(kind, replay)
    };
    let items = match request {
        crate::ProviderRequest::OpenAiResponses(request) => &mut request.input,
        crate::ProviderRequest::ChatGptCodex(request) => &mut request.input,
        crate::ProviderRequest::AnthropicMessages(request)
        | crate::ProviderRequest::VertexAnthropic(request) => &mut request.messages,
        crate::ProviderRequest::OpenAiCompatible(request)
        | crate::ProviderRequest::WatsonxChat(request) => &mut request.messages,
        crate::ProviderRequest::BedrockConverse(request) => &mut request.messages,
        crate::ProviderRequest::GoogleGemini(request)
        | crate::ProviderRequest::VertexGemini(request) => &mut request.messages,
        crate::ProviderRequest::CohereChat(request) => &mut request.messages,
    };
    for item in items.iter_mut() {
        if let crate::CodexInputItem::ProviderReplay(replay) = item {
            if replay.is_gemini()
                && !(replay.matches(&model) && accepts.binds(Kind::Gemini, replay))
            {
                if let Some(text) = replay.gemini_rendered_text() {
                    *item = crate::CodexInputItem::Message(crate::InputMessage {
                        role: crate::MessageRole::Assistant,
                        content: text.to_owned(),
                    });
                    continue;
                }
            }
            let crate::CodexInputItem::ProviderReplay(replay) = item else {
                continue;
            };
            if replay.is_anthropic_text() && !bound(replay, Kind::Anthropic) {
                if let Some(text) = replay.anthropic_rendered_text() {
                    *item = crate::CodexInputItem::Message(crate::InputMessage {
                        role: crate::MessageRole::Assistant,
                        content: text.to_owned(),
                    });
                    continue;
                }
            }
            let crate::CodexInputItem::ProviderReplay(replay) = item else {
                continue;
            };
            if let ReplayPayload::NativeSearch(search) = &replay.payload {
                if !replay.matches(&model) || !accepts.binds(Kind::Search, replay) {
                    *item = crate::CodexInputItem::Message(crate::InputMessage {
                        role: crate::MessageRole::Assistant,
                        content: search.fallback_text(),
                    });
                }
            }
        }
    }
    let mut position = 0;
    let mut removed = Vec::new();
    items.retain(|item| {
        let keep = sendable(item, &model, accepts, &bound);
        if !keep {
            removed.push(position);
        }
        position += 1;
        keep
    });
    removed
}

fn sendable(
    item: &crate::CodexInputItem,
    model: &grapt_types::ModelRef,
    accepts: &Accepts,
    bound: &impl Fn(&ProviderReplay, Kind) -> bool,
) -> bool {
    match item {
        crate::CodexInputItem::ProviderReplay(replay) if replay.is_gemini() => {
            replay.matches(model) && accepts.binds(Kind::Gemini, replay)
        }
        crate::CodexInputItem::ProviderReplay(replay) if replay.is_responses_reasoning() => {
            bound(replay, Kind::Responses)
        }
        // A thinking signature is valid only for the model, endpoint, route,
        // account and credential version that produced it.
        crate::CodexInputItem::ProviderReplay(replay) if replay.is_anthropic_thinking() => {
            bound(replay, Kind::Anthropic)
        }
        crate::CodexInputItem::ProviderReplay(replay) if replay.is_bedrock_thinking() => {
            bound(replay, Kind::Bedrock)
        }
        _ => true,
    }
}
