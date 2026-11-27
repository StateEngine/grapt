use super::*;
use crate::auth::{AuthenticationPolicyVersion, SeatTrustBoundary, SubjectId};
use crate::directory::{MaxPrincipalMappings, SeatDirectoryEntry, SeatPrincipalDirectory};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, SeatRecipientId, Timestamp,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
#[derive(Debug)]
struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1)
    }
}
#[derive(Debug)]
struct Ids(&'static str);
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new(self.0).unwrap_or_else(|| panic!("fixture client"))
    }
}

fn principal(
    subject: &str,
    client: &'static str,
) -> Result<SealedPrincipal, Box<dyn std::error::Error>> {
    let id = SubjectId::try_new(subject)?;
    let directory = SeatPrincipalDirectory::configure(
        vec![SeatDirectoryEntry::new(
            id.clone(),
            SeatRecipientId::try_new("seat-approval")?,
        )],
        MaxPrincipalMappings::try_new(1)?,
        &DeterministicAuthorities::new(&Clock, &Ids(client)),
    )?;
    let boundary = SeatTrustBoundary::new(
        AuthenticationPolicyVersion::try_new("approval-policy")?,
        directory,
    );
    Ok(SealedPrincipal::seal(boundary.authenticate_fixture(id)?))
}

fn hub(
    pending: usize,
    subscribers: usize,
) -> Result<Arc<DogfoodEventHub>, Box<dyn std::error::Error>> {
    Ok(Arc::new(DogfoodEventHub::try_new(
        SseLimits::try_new(SseLimitValues {
            subscribers,
            pending_events: pending,
            event_bytes: MAX_SSE_EVENT_BYTES,
            frame_bytes: MAX_SSE_FRAME_BYTES,
        })?,
        ServerConnectedEvent::new(
            super::super::super::WireId::parse("evt_approval_connected".into()).ok_or("event")?,
        ),
    )?))
}

fn approver(principal: &SealedPrincipal, session: Option<&SessionId>) -> Approver {
    Approver {
        principal: principal.clone(),
        session: session.cloned(),
    }
}

#[test]
fn startup_global_forms_require_a_live_location_wide_approver() -> TestResult {
    let hub = hub(8, 8)?;
    let location = LocationRef::new("/work/owned", None)?;
    let owner = principal("approval-owner", "approval-client")?;
    let _observer = hub.subscribe_at(query::EventLocationQuery::Absent)?;
    let session = "ses_approval_owned".parse()?;
    let _session = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, Some(&session))),
    )?;
    assert!(!hub.has_global_form_recipient(&location)?);
    let ui = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    assert!(hub.has_global_form_recipient(&location)?);
    drop(ui);
    assert!(!hub.has_global_form_recipient(&location)?);
    Ok(())
}

#[tokio::test]
async fn seat01_observers_foreign_owners_clients_sessions_and_locations_do_not_approve(
) -> TestResult {
    let hub = hub(8, 8)?;
    let owner = principal("approval-owner", "approval-client")?;
    let foreign = principal("other-owner", "approval-client")?;
    let other_client = principal("approval-owner", "other-client")?;
    let session: SessionId = "ses_approval_owned".parse()?;
    let permission: PermissionId = "per_approval_owned".parse()?;
    let other_session: SessionId = "ses_approval_other".parse()?;
    let location = LocationRef::new("/work/owned", None)?;
    let _observer = hub.subscribe_at(query::EventLocationQuery::Absent)?;
    let _foreign = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&foreign, None)),
    )?;
    let _client = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&other_client, None)),
    )?;
    let _session = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, Some(&other_session))),
    )?;
    let _location = hub.subscribe_approver_at(
        query::event_location(Some("location%5Bdirectory%5D=%2Fwork%2Fforeign"))
            .map_err(|e| format!("{e:?}"))?,
        Some(approver(&owner, None)),
    )?;
    let watch = hub.watch_approvals(&owner, &session, &permission, location.clone())?;
    assert!(
        !watch.available()?,
        "an observer or foreign attachment gained an approver role"
    );
    let first = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, Some(&session))),
    )?;
    let last = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    assert!(
        !watch.available()?,
        "attachment resurrected a prior headless observation"
    );
    let mut watch = hub.watch_approvals(&owner, &session, &permission, location)?;
    assert!(watch.available()?);
    drop(first);
    assert!(
        watch.available()?,
        "dropping one of two approvers lost the other"
    );
    drop(last);
    let _reconnected = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    tokio::time::timeout(std::time::Duration::from_millis(100), watch.unavailable()).await??;
    assert!(
        !watch.available()?,
        "last approver drop was missed before awaiting the edge"
    );
    Ok(())
}

#[tokio::test]
async fn seat01_overflow_invalidation_and_capacity_never_leave_an_approver_alive() -> TestResult {
    let hub = hub(1, 1)?;
    let owner = principal("approval-owner", "approval-client")?;
    let session: SessionId = "ses_approval_owned".parse()?;
    let permission: PermissionId = "per_approval_owned".parse()?;
    let location = LocationRef::new("/work", None)?;
    let body = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    assert!(hub
        .subscribe_approver_at(
            query::EventLocationQuery::Absent,
            Some(approver(&owner, None))
        )
        .is_err());
    let mut watch = hub.watch_approvals(&owner, &session, &permission, location.clone())?;
    assert!(watch.available()?);
    hub.publish(&DogfoodEvent::ServerConnected(ServerConnectedEvent::new(
        super::super::super::WireId::parse("evt_approval_overflow".into()).ok_or("event")?,
    )))?;
    tokio::time::timeout(std::time::Duration::from_millis(100), watch.unavailable()).await??;
    assert!(!watch.available()?, "overflowed approver survived");
    drop(body);
    let _reconnected = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    assert!(
        !watch.available()?,
        "reconnect resurrected a disconnected observation"
    );
    let mut watch = hub.watch_approvals(&owner, &session, &permission, location)?;
    assert!(watch.available()?);
    hub.invalidate();
    tokio::time::timeout(std::time::Duration::from_millis(100), watch.unavailable()).await??;
    assert!(!watch.available()?, "invalidated approver survived");
    Ok(())
}

#[test]
fn seat01_approver_role_requires_one_valid_explicit_authenticated_header() -> TestResult {
    let owner = principal("approval-owner", "approval-client")?;
    let mut request = Request::new(Body::empty());
    request.extensions_mut().insert(owner);
    assert!(Approver::from_request(&request)
        .map_err(|()| "attachment parse")?
        .is_none());
    for value in ["", "true", "ses_has space"] {
        request
            .headers_mut()
            .insert(APPROVER_HEADER, HeaderValue::from_str(value)?);
        assert!(
            Approver::from_request(&request).is_err(),
            "accepted invalid role {value}"
        );
    }
    request.headers_mut().insert(
        APPROVER_HEADER,
        HeaderValue::from_static("ses_approval_owned"),
    );
    assert!(Approver::from_request(&request)
        .map_err(|()| "attachment parse")?
        .is_some());
    request
        .headers_mut()
        .append(APPROVER_HEADER, HeaderValue::from_static("*"));
    assert!(Approver::from_request(&request).is_err());
    request
        .headers_mut()
        .insert(APPROVER_HEADER, HeaderValue::from_static("*"));
    request.extensions_mut().remove::<SealedPrincipal>();
    assert!(Approver::from_request(&request).is_err());
    Ok(())
}

#[tokio::test]
async fn seat01_overlap_handoff_keeps_presence_but_a_gap_is_permanent_and_bounded() -> TestResult {
    let hub = hub(8, 2)?;
    let owner = principal("approval-owner", "approval-client")?;
    let session: SessionId = "ses_approval_owned".parse()?;
    let permission: PermissionId = "per_approval_owned".parse()?;
    let location = LocationRef::new("/work", None)?;
    let first = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    let mut watch = hub.watch_approvals(&owner, &session, &permission, location.clone())?;
    let replacement = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    drop(first);
    assert!(
        watch.available()?,
        "overlapping approver handoff created a false gap"
    );
    drop(replacement);
    let _reconnected = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    tokio::time::timeout(std::time::Duration::from_millis(100), watch.unavailable()).await??;
    assert!(
        !watch.available()?,
        "reconnect hid a disconnect before the owner was polled"
    );
    drop(watch);
    let mut watches = Vec::new();
    for _ in 0..MAX_SSE_SUBSCRIBERS {
        watches.push(hub.watch_approvals(&owner, &session, &permission, location.clone())?);
    }
    assert!(matches!(
        hub.watch_approvals(&owner, &session, &permission, location.clone()),
        Err(SseError::ApprovalWatchBound)
    ));
    let _ = watches.pop();
    assert!(
        hub.watch_approvals(&owner, &session, &permission, location)?
            .available()?,
        "dropped observation did not release its bounded slot"
    );
    Ok(())
}

#[test]
fn seat01_decision_fence_orders_disconnect_before_late_reply_and_holds_through_commit() -> TestResult
{
    let hub = hub(8, 2)?;
    let owner = principal("approval-owner", "approval-client")?;
    let session: SessionId = "ses_approval_owned".parse()?;
    let permission: PermissionId = "per_approval_owned".parse()?;
    let body = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    let watch = hub.watch_approvals(
        &owner,
        &session,
        &permission,
        LocationRef::new("/work", None)?,
    )?;
    hub.with_permission_approval(&owner, &session, &permission, |attached| {
        assert_eq!(attached, Some(true));
        assert!(
            matches!(
                hub.subscribers.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "decision commit was not fenced against subscription membership changes"
        );
    })?;
    drop(body);
    let _reattached = hub.subscribe_approver_at(
        query::EventLocationQuery::Absent,
        Some(approver(&owner, None)),
    )?;
    assert!(!watch.available()?);
    hub.with_permission_approval(&owner, &session, &permission, |attached| {
        assert_eq!(
            attached,
            Some(false),
            "late reply escaped a recorded disconnect before the waiter was polled"
        );
    })?;
    let unrelated: PermissionId = "per_http_only".parse()?;
    hub.with_permission_approval(&owner, &session, &unrelated, |attached| {
        assert_eq!(
            attached, None,
            "tool presence rules changed an HTTP-only permission request"
        );
    })?;
    Ok(())
}

#[test]
fn renamed_approver_headers_preserve_old_clients_and_refuse_disagreement() -> TestResult {
    let owner = principal("approval-owner", "approval-client")?;
    let current = "x-grapt-approver";
    let legacy = "x-secode-approver"; // grapt-rename: keep
    for value in ["*", "ses_approval_owned"] {
        for (headers, accepted) in [
            (vec![(legacy, value)], true),
            (vec![(current, value)], true),
            (vec![(current, value), (legacy, value)], true),
            (vec![(current, value), (legacy, "ses_other")], false),
        ] {
            let mut request = Request::new(Body::empty());
            request.extensions_mut().insert(owner.clone());
            for (name, value) in headers {
                request
                    .headers_mut()
                    .append(name, HeaderValue::from_str(value)?);
            }
            let parsed = Approver::from_request(&request);
            if accepted {
                let role = parsed
                    .map_err(|()| "valid approver refused")?
                    .ok_or("approver absent")?;
                assert!(role.matches(&owner, &"ses_approval_owned".parse()?));
                assert_eq!(role.matches(&owner, &"ses_other".parse()?), value == "*");
            } else {
                assert!(
                    parsed.is_err(),
                    "conflicting approver headers granted a role"
                );
            }
        }
    }
    Ok(())
}
