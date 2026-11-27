use super::*;
use grapt_types::CompactionTrigger;

fn every_capacity() -> [(Bound, Unit, &'static str); 7] {
    [
        (
            Bound::Conversation,
            Unit::Bytes,
            "conversation exceeds bounded request context; compaction required",
        ),
        (
            Bound::Conversation,
            Unit::Items,
            "conversation exceeds bounded request context; compaction required",
        ),
        (
            Bound::ResponseGroups,
            Unit::Items,
            "response groups exceed the request item bound",
        ),
        (
            Bound::SummaryAndRetained,
            Unit::Bytes,
            "summary and retained history exceed the request bound",
        ),
        (
            Bound::SummaryAndRetained,
            Unit::Items,
            "summary and retained history exceed the request bound",
        ),
        (
            Bound::CapturedMail,
            Unit::Items,
            "conversation exceeds bounded request context; compaction required",
        ),
        (
            Bound::CapturedMail,
            Unit::Bytes,
            "conversation exceeds bounded request context; compaction required",
        ),
    ]
}

#[test]
fn every_capacity_reads_exactly_as_its_old_refusal_and_asks_for_structural_compaction() {
    for (bound, unit, text) in every_capacity() {
        let error = ContextError::capacity(bound, unit, 513, 512);
        assert_eq!(error.to_string(), text, "{bound:?}/{unit:?}");
        assert_eq!(String::from(error.clone()), text, "{bound:?}/{unit:?}");
        assert_eq!(
            error.compaction_trigger(),
            Some(CompactionTrigger::StructuralBound),
            "{bound:?}/{unit:?}"
        );
        assert_eq!(
            error.capacity_bound(),
            Some(Capacity {
                bound,
                unit,
                observed: 513,
                limit: 512
            })
        );
    }
}

#[test]
fn a_failure_never_compacts_even_when_it_spells_a_capacity_refusal() {
    for (_, _, text) in every_capacity() {
        for error in [
            ContextError::from(text),
            ContextError::from(text.to_owned()),
        ] {
            assert_eq!(error, ContextError::Failed(text.to_owned()));
            assert_eq!(error.compaction_trigger(), None, "{text}");
            assert_eq!(error.capacity_bound(), None, "{text}");
        }
    }
    let unreadable = ContextError::from("conversation user history is unreadable: disk I/O error");
    assert_eq!(unreadable.compaction_trigger(), None);
    assert_eq!(
        String::from(unreadable),
        "conversation user history is unreadable: disk I/O error"
    );
}

#[test]
fn captured_mail_keeps_its_unit_and_its_failures() {
    use grapt_inbox::operation_mail::PresentedRowsError;
    let rows = ContextError::from(PresentedRowsError::Rows { limit: 128 });
    assert_eq!(
        rows.capacity_bound(),
        Some(Capacity {
            bound: Bound::CapturedMail,
            unit: Unit::Items,
            observed: 129,
            limit: 128
        })
    );
    let bytes = ContextError::from(PresentedRowsError::SourceBytes {
        charge: 700,
        remaining: 600,
    });
    assert_eq!(
        bytes.capacity_bound(),
        Some(Capacity {
            bound: Bound::CapturedMail,
            unit: Unit::Bytes,
            observed: 700,
            limit: 600
        })
    );
    for error in [
        PresentedRowsError::Rows { limit: 128 },
        PresentedRowsError::SourceBytes {
            charge: 700,
            remaining: 600,
        },
    ] {
        assert_eq!(
            error.to_string(),
            "conversation exceeds bounded request context; compaction required"
        );
    }
    let failed = ContextError::from(PresentedRowsError::Failed(
        "unknown agent mail capture version".to_owned(),
    ));
    assert_eq!(failed.compaction_trigger(), None);
    assert_eq!(failed.to_string(), "unknown agent mail capture version");
}
