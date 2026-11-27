//! The model list carries a REAL provider catalog.
//!
//! `MAX_CATALOG_ROWS` (256) sits below a real account's catalog, and the failure
//! it produced was not a truncated list but a total REFUSAL — measured against
//! the actual product with the campaign's configured accounts, `GET /api/model`
//! answered `{"_tag":"InvalidRequestError","message":"collection exceeds its
//! item bound"}` and no client could list models at all.
//!
//! These witnesses pin what makes the larger bound trustworthy: a real-sized
//! catalog decodes AND is decodable by a client, the boundary is exactly where
//! it is declared, the smaller catalogs are demonstrably untouched, and a
//! row-valid list too large to send REFUSES rather than truncating.

use grapt_api::u3a::{
    ModelListResponseDto, ProviderListResponseDto, MAX_CATALOG_ROWS, MAX_MODEL_ROWS,
};
use grapt_api::{decode_page, decode_request, encode_page, U3aLimits, U3A_PRODUCTION_LIMITS};
use serde_json::{json, Value};

/// The measured size of the real configured catalog: 7 OpenAI rows plus 359
/// OpenRouter rows.
const MEASURED_REAL_CATALOG_ROWS: usize = 366;

/// The measured serialized size of that catalog, in bytes.
const MEASURED_REAL_CATALOG_BYTES: usize = 134_604;

fn must<T, E: core::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn limits() -> U3aLimits {
    must(
        U3aLimits::try_new(U3A_PRODUCTION_LIMITS),
        "production limits are admissible",
    )
}

/// A row mirroring the shape the REAL product emits, taken from the measured
/// catalog rather than invented — including `family`, which an invented row
/// omitted and which the DTO requires.
fn model_row(index: usize) -> Value {
    json!({
        "id": format!("model-{index}"),
        "modelID": format!("model-{index}"),
        "providerID": "openrouter",
        "family": "gpt-astra",
        "name": format!("Model {index}"),
        "capabilities": {"tools": true, "input": ["text", "image", "pdf"], "output": ["text"]},
        "variants": [{"id": "high", "body": {"reasoning": {"effort": "high"}}}],
        "time": {"released": 1788480000000.0},
        "cost": [],
        "status": "active",
        "enabled": true,
        "limit": {"context": 258400, "input": 258400, "output": 128000}
    })
}

fn list_of(rows: usize) -> Value {
    let data: Vec<Value> = (0..rows).map(model_row).collect();
    json!({
        "location": {
            "directory": "E:/temp/j01-service",
            "project": {"id": "global", "directory": "E:/"}
        },
        "data": data
    })
}

/// Built the way the PRODUCT builds it — from a `Value`, which is where the row
/// bound is actually enforced.
///
/// Deliberately NOT through `decode_request`: that carries the 65,536-byte
/// REQUEST bound, and a response is not a request. Measuring the row bound
/// through the request door would have reported a byte refusal and hidden the
/// thing under test.
fn decodes(rows: usize) -> bool {
    serde_json::from_value::<ModelListResponseDto>(list_of(rows)).is_ok()
}

/// The catalog that REFUSED before this bound existed now decodes.
///
/// This is the defect itself, pinned: at 256 a 366-row catalog could not be
/// carried, so the route answered nothing at all.
/// The real catalog MUST exceed the shared bound, or the defect never existed.
/// Asserted at COMPILE time: it is a relationship between constants, and a
/// runtime check of it could only ever be decoration.
const _: () = assert!(MEASURED_REAL_CATALOG_ROWS > MAX_CATALOG_ROWS);

#[test]
fn the_real_configured_catalog_is_carried() {
    assert!(
        decodes(MEASURED_REAL_CATALOG_ROWS),
        "the measured real catalog must decode"
    );
}

/// The boundary is EXACTLY where it is declared: the limit decodes and one more
/// is refused. A bound that admitted its own limit-plus-one would not be a
/// bound, and one that refused its own limit would be off by one in the
/// direction that breaks a real catalog.
#[test]
fn the_bound_admits_its_limit_and_refuses_one_more() {
    assert!(decodes(MAX_MODEL_ROWS), "the exact limit must decode");
    assert!(
        !decodes(MAX_MODEL_ROWS + 1),
        "one past the limit must be refused"
    );
}

/// A full list at the MEASURED row size fits the page bound.
///
/// Stated narrowly on purpose: this does NOT show that every row-valid list fits,
/// which is false and is asserted false below. Rows have no individual size
/// limit, so the row bound and the byte bounds are independent and both apply.
#[test]
fn a_full_list_encodes_inside_the_response_byte_bound() {
    let data: Vec<Value> = (0..MAX_MODEL_ROWS).map(model_row).collect();
    let document = json!({
        "location": {
            "directory": "E:/temp/j01-service",
            "project": {"id": "global", "directory": "E:/"}
        },
        "data": data
    });
    let decoded: ModelListResponseDto = must(
        serde_json::from_value(document),
        "a full-sized list decodes",
    );
    let encoded = must(encode_page(&decoded, limits()), "a full-sized list encodes");
    assert!(
        encoded.len() < U3A_PRODUCTION_LIMITS.page_bytes,
        "a full list is {} bytes against a {}-byte page bound; the row bound must \
         remain the binding constraint",
        encoded.len(),
        U3A_PRODUCTION_LIMITS.page_bytes
    );
}

/// The other catalogs are NOT widened by this change, witnessed on an ACTUAL
/// other catalog rather than on the model list.
///
/// The earlier version of this test decoded `MAX_CATALOG_ROWS` rows through
/// `ModelListResponseDto`, which only showed that the model DTO accepts a
/// smaller list — it said nothing about the surfaces it was supposed to protect.
/// `ProviderListResponseDto` still carries the shared bound, so it must admit
/// exactly 256 and refuse 257.
#[test]
fn a_different_catalog_still_carries_the_smaller_shared_bound() {
    fn provider_list(rows: usize) -> Value {
        let data: Vec<Value> = (0..rows)
            .map(|index| {
                // The shape the real `GET /api/provider` emits.
                json!({
                    "id": format!("provider-{index}"),
                    "name": format!("Provider {index}"),
                    "package": "aisdk:@ai-sdk/openai"
                })
            })
            .collect();
        json!({
            "location": {
                "directory": "E:/temp/j01-service",
                "project": {"id": "global", "directory": "E:/"}
            },
            "data": data
        })
    }
    assert!(
        serde_json::from_value::<ProviderListResponseDto>(provider_list(MAX_CATALOG_ROWS)).is_ok(),
        "a different catalog admits the shared bound"
    );
    assert!(
        serde_json::from_value::<ProviderListResponseDto>(provider_list(MAX_CATALOG_ROWS + 1))
            .is_err(),
        "a different catalog still REFUSES past the shared bound; the model          bound did not widen it"
    );
}

/// A row-valid list can still be too big to SEND, and that refuses rather than
/// truncates.
///
/// This is the independence the sizing note claims, asserted rather than
/// asserted-in-prose: every row here is individually valid and the count is
/// inside `MAX_MODEL_ROWS`, but the rows are large enough that the page exceeds
/// the byte bound. `encode_page` must REFUSE — a truncated page would be a
/// silently short catalog, which is the failure mode the whole model-list defect
/// was about.
#[test]
fn a_row_valid_list_that_exceeds_the_page_bound_is_refused_not_truncated() {
    // A name near the DTO string bound makes each row large; 64 of them clear
    // the 8 MiB page bound while staying far inside the 2,048-row bound.
    let large_name = "n".repeat(200_000);
    let data: Vec<Value> = (0..64)
        .map(|index| {
            let mut row = model_row(index);
            row["name"] = Value::String(large_name.clone());
            row
        })
        .collect();
    let document = json!({
        "location": {
            "directory": "E:/temp/j01-service",
            "project": {"id": "global", "directory": "E:/"}
        },
        "data": data
    });
    let decoded: ModelListResponseDto = must(
        serde_json::from_value(document),
        "every row is individually valid and the count is inside the row bound",
    );
    match encode_page(&decoded, limits()) {
        Ok(page) => panic!(
            "a page over the byte bound must be refused, not emitted at {} bytes",
            page.len()
        ),
        Err(error) => {
            // The refusal names the byte bound, so an operator is not left
            // guessing which limit stopped it.
            assert!(
                format!("{error:?}").contains("PageBytes"),
                "the refusal must name the page-byte bound, got {error:?}"
            );
        }
    }
}

/// A CLIENT CAN DECODE WHAT THE SERVER ENCODES.
///
/// This is the witness that was missing when the row bound was raised, and its
/// absence is what made "the refusal does not move downstream" only half true:
/// encoding was proven and decoding was never checked. The surface bounds
/// requests at 65,536 bytes and pages at 8,388,608, and it offered only
/// `decode_request` — so a real model list encoded fine and no client could read
/// it. Raising the row bound alone would have turned a 400 into a 200 that every
/// client refused at the codec, which from a user's seat is the same broken
/// picker with a different cause.
///
/// Caught by review against an independent client before this landed.
#[test]
fn a_real_catalog_page_is_decodable_by_a_client_not_only_encodable() {
    let list = list_of(MEASURED_REAL_CATALOG_ROWS);
    let decoded: ModelListResponseDto = must(serde_json::from_value(list), "fixture decodes");
    let page = must(encode_page(&decoded, limits()), "the real catalog encodes");
    assert!(
        page.len() > U3A_PRODUCTION_LIMITS.request_dto_bytes,
        "this witness is only meaningful while the real page EXCEEDS the request          bound; it is {} bytes against {}",
        page.len(),
        U3A_PRODUCTION_LIMITS.request_dto_bytes
    );

    // The request decoder refuses it — that is the defect, pinned.
    assert!(
        decode_request::<ModelListResponseDto>(&page, limits()).is_err(),
        "the request decoder must still refuse a page-sized response"
    );

    // The page decoder reads it, which is what a client needs.
    let round_tripped: ModelListResponseDto =
        must(decode_page(&page, limits()), "a client decodes the page");
    let reencoded = must(encode_page(&round_tripped, limits()), "re-encodes");
    assert_eq!(
        reencoded, page,
        "a page must survive an encode/decode round trip byte-for-byte"
    );
}

/// The page decoder is bounded too — a LARGER bound, not the absence of one —
/// and the boundary is exactly where it is declared.
///
/// At the limit the input is admitted by the BYTE bound and then refused as
/// malformed JSON, which is the correct order: the size gate passes and the
/// parser rejects. One byte past, the size gate itself refuses. Asserting only
/// the oversized case would not distinguish "bounded at N" from "bounded at
/// anything less than N".
#[test]
fn the_page_decoder_enforces_the_page_bound_exactly() {
    let at_limit = vec![b'a'; U3A_PRODUCTION_LIMITS.page_bytes];
    let past_limit = vec![b'a'; U3A_PRODUCTION_LIMITS.page_bytes + 1];

    let at = decode_page::<ModelListResponseDto>(&at_limit, limits());
    let past = decode_page::<ModelListResponseDto>(&past_limit, limits());
    let at = match at {
        Ok(_) => panic!("filler is not a valid document"),
        Err(error) => format!("{error:?}"),
    };
    let past = match past {
        Ok(_) => panic!("a payload past the page bound must be refused"),
        Err(error) => format!("{error:?}"),
    };
    assert_ne!(
        at, past,
        "at the limit the refusal must be the PARSER, past it the SIZE bound;          identical errors would mean the byte bound is not where it is declared"
    );
}

/// The page decoder keeps every other refusal `decode_request` has. A larger
/// byte bound must not become a weaker decoder.
#[test]
fn the_page_decoder_still_refuses_malformed_and_invalid_documents() {
    let cases: &[(&str, &[u8])] = &[
        ("truncated", b"{\"location\":"),
        ("trailing data", b"{} trailing"),
        ("not an object", b"[]"),
        ("empty", b""),
        ("wrong field types", b"{\"location\":5,\"data\":5}"),
        (
            "unknown field",
            b"{\"location\":{},\"data\":[],\"extra\":1}",
        ),
    ];
    for (label, bytes) in cases {
        assert!(
            decode_page::<ModelListResponseDto>(bytes, limits()).is_err(),
            "{label} must still be refused by the page decoder"
        );
    }
}

/// The ACTUAL captured catalog — the bytes the real product emitted, not a
/// generated stand-in — round-trips through the page codec.
///
/// A synthetic fixture proves the bound arithmetic; only the captured payload
/// proves the real thing decodes, because it carries the field shapes, unicode
/// and variant structures a generator does not think to produce.
#[test]
fn the_captured_real_catalog_round_trips_through_the_page_codec() {
    const CAPTURED: &str = include_str!("j01_real_model_catalog.json");
    assert!(
        CAPTURED.len() > U3A_PRODUCTION_LIMITS.request_dto_bytes,
        "the captured catalog must exceed the request bound to be meaningful"
    );

    // The request decoder refuses the real response. That is the defect.
    assert!(
        decode_request::<ModelListResponseDto>(CAPTURED.as_bytes(), limits()).is_err(),
        "the request decoder refuses the real captured response"
    );

    // The page decoder reads it, and re-encoding reproduces a page that decodes
    // to the same document.
    let decoded: ModelListResponseDto = must(
        decode_page(CAPTURED.as_bytes(), limits()),
        "the captured catalog decodes as a page",
    );
    let reencoded = must(
        encode_page(&decoded, limits()),
        "the captured catalog re-encodes",
    );
    let again: ModelListResponseDto = must(
        decode_page(&reencoded, limits()),
        "the re-encoded page decodes",
    );
    assert_eq!(
        must(encode_page(&again, limits()), "stable"),
        reencoded,
        "the captured catalog must round-trip stably"
    );
}

/// The bound relationships, asserted at COMPILE time.
///
/// The model bound is the larger one; it leaves at least fivefold headroom over
/// the measured catalog rather than being fitted to today's number; and the
/// measurement that sized it — 367 bytes per row — is recorded here rather than
/// living only in a commit message.
const _: () = assert!(MAX_MODEL_ROWS > MAX_CATALOG_ROWS);
const _: () = assert!(MAX_MODEL_ROWS >= MEASURED_REAL_CATALOG_ROWS * 5);
const _: () = assert!(MEASURED_REAL_CATALOG_BYTES / MEASURED_REAL_CATALOG_ROWS == 367);
const _: () = assert!(MAX_CATALOG_ROWS == 256);
