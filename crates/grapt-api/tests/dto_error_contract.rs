//! The decode bound and the refusal vocabulary, over the public surface.

use grapt_api::{
    decode, decode_with_recipient, ApiRefusal, AuthenticationPolicyVersion, MaxRequestDtoBytes,
    Redacted, SubjectId,
};

fn bound(value: usize) -> MaxRequestDtoBytes {
    match MaxRequestDtoBytes::try_new(value) {
        Ok(bound) => bound,
        Err(refusal) => panic!("fixture bound {value} was refused: {refusal}"),
    }
}

#[test]
fn an_oversized_body_is_refused_before_it_is_parsed() {
    // The payload is oversize AND invalid UTF-8. That combination is what makes
    // this an ORDERING witness rather than a size witness: if the decoder parsed
    // first and measured after, it would report the encoding problem. Reporting
    // the bound proves nothing was parsed.
    let mut hostile = vec![0xFF, 0xFE, 0xFD];
    hostile.extend(std::iter::repeat_n(b'a', 64));

    match decode(bound(8), &hostile) {
        Ok(_) => panic!("an oversized body must be refused"),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::DtoTooLarge { max: 8, .. }),
            "the bound is checked before the parse, so a hostile payload is \
             never fully materialized; got {refusal:?}"
        ),
    }

    // The same-file control. A decoder that refused everything would satisfy the
    // case above and decode nothing at all.
    match decode(bound(64), b"a small body") {
        Ok(envelope) => {
            assert_eq!(envelope.body(), "a small body");
            assert!(
                envelope.recipient().is_none(),
                "a route without a recipient field carries none"
            );
        }
        Err(refusal) => panic!("a body within the bound was refused: {refusal}"),
    }
}

#[test]
fn a_body_exactly_at_the_bound_is_admitted() {
    // The boundary case, stated explicitly: the bound is a maximum, not an
    // exclusive limit, and an off-by-one here would refuse legitimate traffic
    // while every other test in this file still passed.
    let body = vec![b'x'; 16];
    match decode(bound(16), &body) {
        Ok(envelope) => assert_eq!(envelope.body().len(), 16),
        Err(refusal) => panic!("a body exactly at the bound was refused: {refusal}"),
    }
    match decode(bound(15), &body) {
        Ok(_) => panic!("one byte past the bound must be refused"),
        Err(refusal) => assert!(matches!(refusal, ApiRefusal::DtoTooLarge { max: 15, .. })),
    }
}

#[test]
fn a_zero_decode_bound_is_refused_rather_than_defaulted() {
    match MaxRequestDtoBytes::try_new(0) {
        Ok(_) => panic!("a zero decode bound must be refused"),
        Err(refusal) => assert!(
            matches!(
                refusal,
                ApiRefusal::BoundNotPositive {
                    bound: "max_request_dto_bytes"
                }
            ),
            "the refusal names the decode bound; got {refusal:?}"
        ),
    }
}

#[test]
fn malformed_body_subject_and_policy_name_their_own_laws() {
    match decode(bound(8), &[0xFF]) {
        Ok(_) => panic!("a non-UTF-8 request body must be refused"),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::BodyNotUtf8),
            "a body encoding defect must not render as a recipient defect; got {refusal:?}"
        ),
    }

    match SubjectId::try_new("") {
        Ok(_) => panic!("an empty authentication subject must be refused"),
        Err(refusal) => assert!(
            matches!(
                refusal,
                ApiRefusal::SubjectMalformed { max, .. }
                    if max == grapt_api::MAX_SUBJECT_ID_BYTES
            ),
            "a subject defect must name the subject bound; got {refusal:?}"
        ),
    }

    match AuthenticationPolicyVersion::try_new("") {
        Ok(_) => panic!("an empty authentication policy version must be refused"),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::PolicyVersionMalformed),
            "a policy-version defect must name its own rule; got {refusal:?}"
        ),
    }
}

#[test]
fn a_malformed_recipient_field_is_refused_naming_the_bound() {
    // 257 bytes: one past `MAX_SEAT_RECIPIENT_ID_BYTES`. The bound belongs to
    // `SeatRecipientId::try_new` in `grapt-types`, and this crate propagates
    // that type's refusal rather than re-implementing the limit — one law, one
    // carrier.
    let oversize = "s".repeat(257);
    let refusal = match decode_with_recipient(bound(64), b"{}", &oversize) {
        Ok(_) => panic!("an oversize recipient must be refused"),
        Err(refusal) => refusal,
    };
    assert!(matches!(refusal, ApiRefusal::RecipientMalformed { .. }));

    let rendered = refusal.to_string();
    assert!(
        rendered.contains("256"),
        "the refusal names the BOUND so a caller can comply: {rendered}"
    );
    assert!(
        !rendered.contains("257"),
        "the refusal must never report the observed length — that tells a caller \
         exactly how much was accepted before the refusal: {rendered}"
    );

    // An empty recipient is refused by the same route, and reports the same
    // thing: which of "empty" and "too long" occurred is an observation about
    // the caller's input, and distinguishing them outward is an oracle.
    match decode_with_recipient(bound(64), b"{}", "") {
        Ok(_) => panic!("an empty recipient must be refused"),
        Err(refusal) => assert_eq!(refusal.to_string(), rendered),
    }

    // The same-file control.
    match decode_with_recipient(bound(64), b"{}", "seat-alpha") {
        Ok(envelope) => {
            let Some(recipient) = envelope.recipient() else {
                panic!("a valid recipient field must be carried");
            };
            assert_eq!(recipient.as_str(), "seat-alpha");
        }
        Err(refusal) => panic!("a valid recipient was refused: {refusal}"),
    }
}

#[test]
fn the_redaction_wrapper_renders_a_sentinel_in_both_formats() {
    // `Redacted` is the single carrier of the no-echo law, so it is asserted
    // directly rather than only through the refusals that use it. `Debug`
    // matters as much as `Display`: a structured log line or a panic message
    // formats with `{:?}`, and a derived `Debug` would print exactly what
    // `Display` was keeping quiet.
    let secret = Redacted::new("hunter2-super-secret".to_owned());
    assert_eq!(secret.to_string(), "<redacted>");
    assert_eq!(format!("{secret:?}"), "<redacted>");

    let length = Redacted::new(8_192_usize);
    assert_eq!(length.to_string(), "<redacted>");
    assert_eq!(format!("{length:?}"), "<redacted>");
}
