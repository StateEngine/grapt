    use super::*;
    use std::str::FromStr;

    fn envelope(epoch: u32, seq: u64) -> Result<EventEnvelopeV2, Box<dyn std::error::Error>> {
        Ok(EventEnvelopeV2::new(
            EventId::from_str("evt_v2fixture01")?,
            AggregateId::Session("ses_v2fixture01".parse()?),
            EventSeq::new(seq),
            WriterEpoch::new(epoch).ok_or("epoch")?,
            EnvelopeKind::Legacy(EventKind::SessionInputAdmittedV1),
            Timestamp::from_millis(1_700_000_000_000),
            b"payload-canonical-bytes".to_vec(),
        )?)
    }

    #[test]
    fn the_writer_epoch_is_inside_the_canonical_bytes() -> Result<(), Box<dyn std::error::Error>> {
        // W1: identical envelopes except for the epoch are different facts.
        let one = envelope(1, 0)?;
        let two = envelope(2, 0)?;
        assert_ne!(one.canonical_bytes(), two.canonical_bytes());
        assert_ne!(one.digest(), two.digest());
        // And the digest tracks the bytes, not the struct identity.
        assert_eq!(one.digest(), envelope(1, 0)?.digest());
        Ok(())
    }

    #[test]
    fn a_historical_kind_cannot_be_written_new() -> Result<(), Box<dyn std::error::Error>> {
        let refusal = EventEnvelopeV2::new(
            EventId::from_str("evt_v2fixture02")?,
            AggregateId::Session("ses_v2fixture01".parse()?),
            EventSeq::FIRST,
            WriterEpoch::FIRST,
            EnvelopeKind::Legacy(EventKind::SessionToolSuccessV1),
            Timestamp::from_millis(0),
            Vec::new(),
        );
        assert_eq!(
            refusal.err(),
            Some(EnvelopeError::HistoricalKind {
                kind: EventKind::SessionToolSuccessV1
            })
        );
        Ok(())
    }

    #[test]
    fn fencing_is_always_fenced_and_the_vocabulary_stays_closed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // W10: exhaustive match, no wildcard — adding a fencing variant
        // breaks this until someone decides what it means here.
        match envelope(1, 0)?.fencing() {
            EventFencing::Fenced { epoch } => assert_eq!(epoch, WriterEpoch::FIRST),
            EventFencing::LegacyUnfenced => {
                panic!("a constructed v2 envelope can never be unfenced")
            }
        }
        Ok(())
    }

    /// Restates the documented layout independently of the encoder, in
    /// literal bytes. A relative assertion would hold under a changed
    /// format; this one cannot.
    #[test]
    fn the_frozen_format_is_pinned_byte_for_byte() -> Result<(), Box<dyn std::error::Error>> {
        fn scalar(tag: u8, payload: &[u8]) -> Vec<u8> {
            let mut out = vec![tag];
            out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
            out.extend_from_slice(payload);
            out
        }
        let mut expected = Vec::new();
        expected.extend(scalar(0x02, b"secode-event-envelope"));
        expected.extend(scalar(0x03, &1u64.to_be_bytes()));
        expected.extend(scalar(0x01, b"evt_v2fixture01"));
        expected.extend(scalar(0x01, b"session"));
        expected.extend(scalar(0x01, b"ses_v2fixture01"));
        expected.extend(scalar(0x03, &0u64.to_be_bytes()));
        expected.extend(scalar(0x03, &1u64.to_be_bytes()));
        expected.extend(scalar(0x01, b"session.input.admitted.1"));
        expected.extend(scalar(0x04, &1_700_000_000_000i64.to_be_bytes()));
        expected.extend(scalar(0x02, b"payload-canonical-bytes"));
        assert_eq!(envelope(1, 0)?.canonical_bytes(), expected);
        Ok(())
    }

    /// Freezes one known envelope's digest. Any byte-layout change moves
    /// this constant and must bump FORMAT_VERSION with a new golden.
    #[test]
    fn the_golden_digest_is_pinned() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            envelope(1, 0)?.digest().to_string(),
            "secode-event-envelope/1:\
             e8d2471c3337e5804cb717b1823f54444e81678f3ad7e4e1cbdf78a51b13931f",
        );
        Ok(())
    }

    #[test]
    fn every_coordinate_moves_the_digest() -> Result<(), Box<dyn std::error::Error>> {
        // Per-coordinate sensitivity, one changed coordinate at a time.
        let base = envelope(1, 0)?;
        let base_digest = base.digest();

        let seq = envelope(1, 1)?;
        assert_ne!(seq.digest(), base_digest, "seq");

        let id = EventEnvelopeV2::new(
            EventId::from_str("evt_v2fixture99")?,
            base.aggregate().clone(),
            base.seq(),
            base.epoch(),
            base.kind(),
            base.created(),
            base.payload().to_vec(),
        )?;
        assert_ne!(id.digest(), base_digest, "id");

        let aggregate = EventEnvelopeV2::new(
            base.id().clone(),
            AggregateId::Session("ses_v2fixture99".parse()?),
            base.seq(),
            base.epoch(),
            base.kind(),
            base.created(),
            base.payload().to_vec(),
        )?;
        assert_ne!(aggregate.digest(), base_digest, "aggregate");

        let kind = EventEnvelopeV2::new(
            base.id().clone(),
            base.aggregate().clone(),
            base.seq(),
            base.epoch(),
            EnvelopeKind::Legacy(EventKind::SessionInputPromotedV1),
            base.created(),
            base.payload().to_vec(),
        )?;
        assert_ne!(kind.digest(), base_digest, "kind");

        let created = EventEnvelopeV2::new(
            base.id().clone(),
            base.aggregate().clone(),
            base.seq(),
            base.epoch(),
            base.kind(),
            Timestamp::from_millis(1),
            base.payload().to_vec(),
        )?;
        assert_ne!(created.digest(), base_digest, "created");

        let payload = EventEnvelopeV2::new(
            base.id().clone(),
            base.aggregate().clone(),
            base.seq(),
            base.epoch(),
            base.kind(),
            base.created(),
            b"other-bytes".to_vec(),
        )?;
        assert_ne!(payload.digest(), base_digest, "payload");
        Ok(())
    }

    // L2 note, stated rather than faked: in THIS stream no two adjacent
    // fields are both free variable-length values — every string field is
    // followed by a fixed-width scalar or a constant-set label, so a
    // removal-collision pair (equal unframed concatenations, different
    // framed bytes) is impossible to construct here by arithmetic: dropping
    // a byte from a string cannot be absorbed by a fixed 8-byte neighbour.
    /// A stored envelope keeps NO derived digest beside its bytes, so
    /// decoding must reproduce the envelope exactly — a decoder that dropped
    /// or reordered one coordinate would recompute a digest over bytes that no
    /// longer describe the stored fact.
    #[test]
    fn canonical_bytes_round_trip_through_decode() -> Result<(), Box<dyn std::error::Error>> {
        let session = EventEnvelopeV2::new(
            EventId::from_str("evt_rt2")?,
            AggregateId::Session("ses_rt".parse()?),
            EventSeq::new(7),
            WriterEpoch::FIRST,
            EnvelopeKind::Legacy(EventKind::parse("session.updated.1")?),
            Timestamp::from_millis(-1_234),
            b"payload-bytes".to_vec(),
        )?;
        let decoded = decode_canonical(&session.canonical_bytes())?;
        assert_eq!(decoded.canonical_bytes(), session.canonical_bytes());
        assert_eq!(decoded.digest(), session.digest());
        assert_eq!(decoded.payload(), session.payload());
        assert_eq!(decoded.seq(), session.seq());
        assert_eq!(decoded.epoch(), session.epoch());

        // A trailing byte is a different byte string and must not decode to
        // the same fact.
        let mut trailing = session.canonical_bytes();
        trailing.push(0);
        assert!(matches!(
            decode_canonical(&trailing),
            Err(EnvelopeDecodeError::TrailingBytes { remaining: 1 })
        ));

        // Every prefix is malformed rather than a shorter fact.
        let full = session.canonical_bytes();
        for cut in 0..full.len() {
            assert!(decode_canonical(&full[..cut]).is_err(), "prefix {cut}");
        }
        Ok(())
    }
