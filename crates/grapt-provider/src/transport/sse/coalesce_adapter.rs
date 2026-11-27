//! Exact read boundaries through the production adapter's framing/route path.
//! HTTP writes cannot promise reqwest read boundaries, so a real adapter open
//! supplies the stream and this module drives its shared socket-chunk handler.
use super::*;
use crate::fragment::{AdapterState, FinishReason};
use crate::request::ProtocolFamily;
use crate::transport::adapter::ProviderOpen;
use crate::transport::fixtures::{adapter, drain, input, responses_request, serve};

async fn fragments(bytes: &[u8], chunk_size: usize) -> Vec<AttemptFragment> {
    let fixture = serve(200, &[], Vec::new()).await;
    let provider = adapter(1);
    let open = provider
        .open(input(
            "sse-coalesce",
            &fixture.base,
            ProtocolFamily::OpenAiResponses,
            responses_request(),
        ))
        .await
        .unwrap_or_else(|error| panic!("adapter open: {error}"));
    let ProviderOpen::Streaming(mut stream) = open else {
        panic!("adapter did not open a stream")
    };
    for chunk in bytes.chunks(chunk_size) {
        stream.feed_chunk(chunk);
        if stream.finished {
            break;
        }
    }
    assert!(
        stream.finished,
        "test response must contain a terminal or a framing refusal"
    );
    assert_eq!(
        provider.active_len(),
        0,
        "terminal must release capacity before drop"
    );
    drain(ProviderOpen::Streaming(stream)).await
}

fn padded_compaction() -> Vec<u8> {
    // Same four ignored padding fields as product's sc-oversize-envelope.
    let mut bytes = String::new();
    for index in 0..4 {
        bytes.push_str(&format!("data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"compaction\",\"id\":\"cmp_sc_{index}\",\"encrypted_content\":\"SC_PROVIDER_COMPACTED\",\"padding\":\"{}\"}}}}\n\n", "p".repeat(100_000)));
    }
    bytes.push_str("data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":19000,\"output_tokens\":300,\"total_tokens\":19300}}}\n\n");
    bytes.into_bytes()
}

#[tokio::test]
async fn sc_oversize_envelope_reaches_product_with_identical_adapter_states() {
    let bytes = padded_compaction();
    let coalesced = fragments(&bytes, bytes.len()).await;
    assert!(
        coalesced.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderTerminal {
                finish: FinishReason::Stop,
                ..
            }
        )),
        "COALESCED_COMPACTION_MALFORMED: {coalesced:?}"
    );
    let states: Vec<&[u8]> = coalesced
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::ReasoningClosed { state, .. } => state.capture_bytes(),
            _ => None,
        })
        .collect();
    assert_eq!(states.len(), 4);
    assert!(states
        .iter()
        .all(|state| state.len() <= AdapterState::MAX_BYTES));
    assert!(states.iter().map(|state| state.len()).sum::<usize>() > 320 * 1024);
    for chunk_size in [64 * 1024, 100_000] {
        assert_eq!(fragments(&bytes, chunk_size).await, coalesced);
    }
}

#[tokio::test]
async fn an_oversized_frame_has_identical_adapter_failure_across_reads() {
    let bytes = format!("data: {}\n\n", "x".repeat(AdapterState::MAX_BYTES + 1)).into_bytes();
    let coalesced = fragments(&bytes, bytes.len()).await;
    assert!(coalesced.iter().any(|fragment| matches!(fragment,
        AttemptFragment::ProviderFailure { class: ProviderFailureClass::InvalidProviderOutput, message, .. }
        if message == "the provider stream exceeded the adapter capture bound")));
    for chunk_size in [64 * 1024, AdapterState::MAX_BYTES] {
        assert_eq!(fragments(&bytes, chunk_size).await, coalesced);
    }
}
