#[test]
fn auto_refuses_removal_and_skip_approves_classified_ordinary_requests() {
    use crate::AutoReply;
    let policy = serde_json::json!({"policy":{"ownerFloor":false,"removesFiles":false}});
    for action in ["fs.modify", "fs.create", "fs.overwrite", "shell", "mcp_x"] {
        assert_eq!(
            super::auto_reply_for(action, AutoReply::Auto, Some(&policy)),
            "once",
            "{action}"
        );
    }
    for action in ["fs.delete", "fs.move"] {
        assert_eq!(
            super::auto_reply_for(action, AutoReply::Auto, Some(&policy)),
            "reject",
            "{action}"
        );
        assert_eq!(
            super::auto_reply_for(action, AutoReply::Everything, Some(&policy)),
            "once",
            "{action}"
        );
    }
}

#[test]
fn auto_uses_the_authoritative_patch_and_owner_classification() {
    use crate::AutoReply;
    for action in ["fs.modify", "patch", "edit"] {
        let protected = serde_json::json!({"policy":{"ownerFloor":true,"removesFiles":false}});
        for mode in [AutoReply::Auto, AutoReply::Everything] {
            assert_eq!(
                super::auto_reply_for(action, mode, Some(&protected)),
                "reject"
            );
        }
    }
    let removal = serde_json::json!({"policy":{"ownerFloor":false,"removesFiles":true}});
    assert_eq!(
        super::auto_reply_for("patch", AutoReply::Auto, Some(&removal)),
        "reject"
    );
    assert_eq!(
        super::auto_reply_for("patch", AutoReply::Everything, Some(&removal)),
        "once"
    );
    let ordinary = serde_json::json!({"policy":{"ownerFloor":false,"removesFiles":false}});
    assert_eq!(
        super::auto_reply_for("patch", AutoReply::Auto, Some(&ordinary)),
        "once"
    );
    for action in ["patch", "fs.modify", "shell"] {
        for mode in [AutoReply::Auto, AutoReply::Everything] {
            assert_eq!(super::auto_reply_for(action, mode, None), "reject");
            assert_eq!(
                super::auto_reply_for(
                    action,
                    mode,
                    Some(&serde_json::json!({"policy":{"ownerFloor":false}}))
                ),
                "reject"
            );
        }
    }
}

use super::{ProgressBound, RunError, MAX_SSE_RECONNECTS, MAX_TERMINAL_POLLS};

#[test]
fn reconnects_count_only_without_durable_progress_between_them() {
    let mut bound = ProgressBound::default();
    for _routine_end in 0..1_000 {
        bound.observe(true);
        assert!(
            bound.reconnect().is_ok(),
            "CLT101 a progressing turn exhausted its reconnect bound"
        );
    }
    // The last reconnect above already counts as the first without progress.
    for _stalled in 1..MAX_SSE_RECONNECTS {
        assert!(bound.reconnect().is_ok());
    }
    assert!(matches!(
        bound.reconnect(),
        Err(RunError::SseReconnectBound { maximum }) if maximum == MAX_SSE_RECONNECTS
    ));
}

#[test]
fn quiet_polls_count_only_since_the_last_durable_progress() {
    let mut bound = ProgressBound::default();
    for _progressing_quiet_period in 0..10 {
        for _quiet in 1..MAX_TERMINAL_POLLS {
            assert!(
                !bound.quiet_poll_exhausted(),
                "CLT103 quiet polls accumulated across durable progress"
            );
        }
        bound.observe(true);
    }
    for _quiet in 1..MAX_TERMINAL_POLLS {
        assert!(!bound.quiet_poll_exhausted());
    }
    assert!(bound.quiet_poll_exhausted());
}

#[test]
fn an_unchanged_relist_is_not_progress() {
    let mut bound = ProgressBound::default();
    for _unchanged in 0..MAX_SSE_RECONNECTS {
        bound.observe(false);
        assert!(bound.reconnect().is_ok());
    }
    bound.observe(false);
    assert!(bound.reconnect().is_err());
}
