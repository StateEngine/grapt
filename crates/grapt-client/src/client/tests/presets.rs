use super::*;
use crate::preset::Preset;
use crate::transport::WireResponse;
use std::time::Duration;

#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct PresetPeer {
    inner: ContractDouble,
    held: Option<WireRequest>,
    release: bool,
    refuse: bool,
    unreadable: bool,
    effective: Preset,
    lose_selection_reply: bool,
    connected: bool,
    canceled: usize,
    requests: Vec<WireRequest>,
    selections: BTreeMap<SessionWireId, Preset>,
}
impl PresetPeer {
    fn answer(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let raw = request
            .path()
            .strip_prefix("/api/session/")
            .and_then(|p| p.strip_suffix("/preset"));
        let Some(raw) = raw else {
            return self.inner.call(request);
        };
        let id = crate::wire::session_id(raw).map_err(|error| TransportError::Io {
            reason: error.to_string(),
        })?;
        if request.operation() == Operation::SessionPresetSelect {
            if self.refuse {
                return Ok(WireResponse {
                    status: 403,
                    body: Vec::new(),
                });
            }
            let bytes = request.body().unwrap_or_default();
            let dto: grapt_api::u3a::SessionPresetSelectDto = grapt_api::u3a::decode_request(
                bytes,
                crate::wire::limits().map_err(|error| TransportError::Io {
                    reason: error.to_string(),
                })?,
            )
            .map_err(|error| TransportError::Io {
                reason: error.to_string(),
            })?;
            let preset = match dto.preset().get().as_str() {
                "inspect" => Preset::Inspect,
                "author" => Preset::Author,
                "bypass" => Preset::Bypass,
                _ => Preset::Maintainer,
            };
            self.selections.insert(id, preset);
            return Ok(WireResponse {
                status: 204,
                body: Vec::new(),
            });
        }
        if self.unreadable {
            return Ok(WireResponse {
                status: 200,
                body: b"{}".to_vec(),
            });
        }
        let selected = self.selections.get(&id).copied().map(crate::preset::name);
        Ok(WireResponse {
            status: 200,
            body: serde_json::json!({"data":{"selected":selected,"effective":crate::preset::name(self.effective)}})
                .to_string()
                .into_bytes(),
        })
    }
}
impl Transport for PresetPeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let response = self.answer(request)?;
        if request.operation() == Operation::SessionPresetSelect
            && std::mem::take(&mut self.lose_selection_reply)
        {
            return Err(TransportError::AfterSend {
                reason: "lost preset response".into(),
            });
        }
        Ok(response)
    }
}
impl Connection for PresetPeer {
    fn subscribed(&self) -> bool {
        self.connected
    }
    fn resubscribe(&mut self) {
        self.connected = true;
        self.inner.resubscribe();
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        assert!(self.held.is_none(), "one pending owner was overwritten");
        if matches!(
            request.operation(),
            Operation::SessionPresetGet | Operation::SessionPresetSelect
        ) {
            self.requests.push(request.clone());
            self.held = Some(request.clone());
            Ok(None)
        } else {
            self.answer(request).map(Some)
        }
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        if !self.release {
            return Ok(None);
        }
        let request = self.held.take().ok_or(TransportError::NotConnected)?;
        self.answer(&request).map(Some)
    }
    fn cancel_read(&mut self) {
        if self.held.take().is_some() {
            self.canceled += 1;
        }
    }
}
fn preset_client(launch: Option<Preset>) -> Result<Client<PresetPeer>, ClientError> {
    let mut client = Client::new(
        PresetPeer {
            inner: ContractDouble::new(),
            held: None,
            release: false,
            refuse: false,
            unreadable: false,
            effective: Preset::Inspect,
            lose_selection_reply: false,
            connected: true,
            canceled: 0,
            requests: Vec::new(),
            selections: BTreeMap::new(),
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    client.set_launch_preset(launch);
    Ok(client)
}
fn settle(client: &mut Client<PresetPeer>) -> Result<(), ClientError> {
    client.peer_mut().release = true;
    for _ in 0..4 {
        client.pump_at(Duration::ZERO)?;
    }
    Ok(())
}

#[test]
fn seat02_bypass_footer_is_explicit_and_wire_round_trips() -> Result<(), ClientError> {
    let mut client = preset_client(Some(Preset::Bypass))?;
    client.create_session()?;
    settle(&mut client)?;
    client.preset.snapshot = Some(crate::wire::PresetSnapshot {
        selected: Some(Preset::Bypass),
        effective: Preset::Bypass,
    });
    assert!(client
        .preset_line()
        .is_some_and(|line| line.contains("Bypass permissions on")));
    let encoded = crate::wire::encode_preset(Preset::Bypass)?;
    assert_eq!(
        crate::wire::decode_preset_selection(&encoded)?,
        Preset::Bypass
    );
    Ok(())
}
#[test]
fn preset_new_and_resumed_attach_apply_once_and_reconnect_only_reads() -> Result<(), ClientError> {
    let mut client = preset_client(Some(Preset::Author))?;
    let id = client.create_session()?;
    client.pump_at(Duration::ZERO)?;
    assert!(client.submit_prompt("kept").is_err());
    assert!(client.peer_mut().selections.is_empty());
    settle(&mut client)?;
    assert_eq!(
        client.preset_line().as_deref(),
        Some("Preset: author | effective: inspect")
    );
    client.peer_mut().connected = false;
    settle(&mut client)?;
    assert_eq!(
        client
            .peer_mut()
            .requests
            .iter()
            .filter(|r| r.operation() == Operation::SessionPresetSelect)
            .count(),
        1
    );
    client.select(&id)?;
    settle(&mut client)?;
    assert_eq!(
        client
            .peer_mut()
            .requests
            .iter()
            .filter(|r| r.operation() == Operation::SessionPresetSelect)
            .count(),
        2
    );
    Ok(())
}
#[test]
fn preset_default_and_read_failure_are_distinct() -> Result<(), ClientError> {
    let mut client = preset_client(None)?;
    let id = client.create_session()?;
    settle(&mut client)?;
    assert_eq!(
        client.preset_line().as_deref(),
        Some("Preset: default | effective: inspect")
    );
    client.peer_mut().unreadable = true;
    client.select(&id)?;
    settle(&mut client)?;
    assert!(client
        .preset_line()
        .is_some_and(|line| line.starts_with("Preset unavailable:")));
    assert!(client
        .peer_mut()
        .requests
        .iter()
        .all(|r| r.operation() == Operation::SessionPresetGet));
    Ok(())
}
#[test]
fn preset_failed_selection_never_appears_applied_or_submits_a_prompt() -> Result<(), ClientError> {
    let mut client = preset_client(Some(Preset::Maintainer))?;
    client.create_session()?;
    client.peer_mut().refuse = true;
    settle(&mut client)?;
    assert!(client
        .preset_line()
        .is_some_and(|line| line.contains("403")));
    assert!(client.submit_prompt("kept").is_err());
    assert!(client.peer_mut().selections.is_empty());
    Ok(())
}
#[test]
fn preset_switch_cancels_old_io_even_when_returning_to_same_session() -> Result<(), ClientError> {
    let mut client = preset_client(Some(Preset::Inspect))?;
    let old = client.create_session()?;
    client.pump_at(Duration::ZERO)?;
    let new = client.create_session()?;
    assert_eq!(client.peer_mut().canceled, 1);
    settle(&mut client)?;
    assert!(!client.peer_mut().selections.contains_key(&old));
    assert_eq!(
        client.peer_mut().selections.get(&new),
        Some(&Preset::Inspect)
    );
    client.select(&old)?;
    settle(&mut client)?;
    assert_eq!(
        client.peer_mut().selections.get(&old),
        Some(&Preset::Inspect)
    );
    assert_eq!(client.selected(), Some(&old));
    Ok(())
}
#[test]
fn preset_pending_selection_does_not_block_terminal_hydration() -> Result<(), ClientError> {
    let mut client = preset_client(Some(Preset::Inspect))?;
    client.create_session()?;
    client.pump_at(Duration::ZERO)?;
    client.request_history(); // A terminal arrives while the POST is pending.
    settle(&mut client)?;
    assert!(client.recovery.history.is_none());
    assert!(client.require_launch_preset().is_ok());
    Ok(())
}

#[test]
fn preset_busy_copy_does_not_claim_to_change_current_execution() -> Result<(), ClientError> {
    let mut client = preset_client(Some(Preset::Author))?;
    let session = client.create_session()?;
    settle(&mut client)?;
    let execution = crate::wire::ExecutionWireId::parse("exe_preset".to_owned())
        .unwrap_or_else(|| panic!("execution id refused"));
    let view = client
        .views
        .get_mut(&session)
        .unwrap_or_else(|| panic!("selected view absent"));
    view.execution = crate::projection::ExecutionState::Running {
        execution_id: execution,
    };
    assert!(client
        .preset_line()
        .is_some_and(|line| line.ends_with("applies from the next turn")));
    assert!(matches!(
        client.view().map(|view| &view.execution),
        Some(crate::projection::ExecutionState::Running { .. })
    ));
    Ok(())
}

#[test]
fn interactive_preset_selection_fences_an_older_pending_read() -> Result<(), ClientError> {
    let mut client = preset_client(None)?;
    let session = client.create_session()?;
    client.pump_at(Duration::ZERO)?;
    assert!(client.peer_mut().held.is_some());
    client.select_permission_preset(Preset::Maintainer)?;
    assert_eq!(client.peer_mut().canceled, 1);
    assert_eq!(
        client.peer_mut().selections.get(&session),
        Some(&Preset::Maintainer)
    );
    assert_eq!(
        client.preset_line().as_deref(),
        Some("Preset: maintainer | effective: inspect")
    );
    settle(&mut client)?;
    assert_eq!(
        client.peer_mut().selections.get(&session),
        Some(&Preset::Maintainer)
    );
    assert!(client.preset.job.is_none());
    assert_eq!(client.recovery_wait(), None);
    Ok(())
}

#[test]
fn permission_inspection_keeps_launch_intent_until_an_explicit_selection() -> Result<(), ClientError>
{
    let mut client = preset_client(Some(Preset::Author))?;
    let session = client.create_session()?;
    client.pump_at(Duration::ZERO)?;
    assert!(client.refresh_permission_preset().is_err());
    assert_eq!(client.peer_mut().canceled, 0);
    assert!(client.peer_mut().held.is_some());
    assert!(client.select_permission_preset(Preset::Inspect).is_err());
    assert_eq!(client.peer_mut().canceled, 0);
    settle(&mut client)?;
    client.select_permission_preset(Preset::Inspect)?;
    assert_eq!(
        client.peer_mut().selections.get(&session),
        Some(&Preset::Inspect)
    );
    assert!(client.require_launch_preset().is_ok());
    Ok(())
}

#[test]
fn refused_or_unreadable_permissions_never_display_a_selected_bypass() -> Result<(), ClientError> {
    let mut client = preset_client(None)?;
    let session = client.create_session()?;
    client.select_permission_preset(Preset::Author)?;
    client.peer_mut().refuse = true;
    assert!(client.select_permission_preset(Preset::Bypass).is_err());
    assert_eq!(
        client.peer_mut().selections.get(&session),
        Some(&Preset::Author)
    );
    assert!(client
        .preset_line()
        .is_some_and(|line| line.starts_with("Preset unavailable:") && line.contains("403")));
    client.peer_mut().refuse = false;
    client.refresh_permission_preset()?;
    assert_eq!(
        client.preset_line().as_deref(),
        Some("Preset: author | effective: inspect")
    );
    client.peer_mut().unreadable = true;
    assert!(client.refresh_permission_preset().is_err());
    assert!(client
        .preset_line()
        .is_some_and(|line| line.starts_with("Preset unavailable:")));
    Ok(())
}

#[test]
fn interactive_preset_lost_reply_blocks_overtaking_writes_and_prompts() -> Result<(), ClientError> {
    let mut client = preset_client(None)?;
    let session = client.create_session()?;
    settle(&mut client)?;
    client.peer_mut().lose_selection_reply = true;
    assert!(client.select_permission_preset(Preset::Inspect).is_err());
    assert_eq!(
        client.peer_mut().selections.get(&session),
        Some(&Preset::Inspect)
    );
    assert!(client.select_permission_preset(Preset::Bypass).is_err());
    assert!(client.submit_prompt("must remain blocked").is_err());
    client.refresh_permission_preset()?;
    assert!(client.select_permission_preset(Preset::Bypass).is_err());
    assert_eq!(
        client.peer_mut().selections.get(&session),
        Some(&Preset::Inspect)
    );
    assert!(client
        .preset_line()
        .is_some_and(|line| line.contains("unconfirmed")));
    Ok(())
}

#[test]
fn bypass_banner_follows_effective_policy_through_reconnect_and_narrowing(
) -> Result<(), ClientError> {
    let mut client = preset_client(None)?;
    client.create_session()?;
    settle(&mut client)?;
    client.peer_mut().effective = Preset::Bypass;
    client.select_permission_preset(Preset::Bypass)?;
    assert!(client
        .preset_line()
        .is_some_and(|line| line.starts_with("Bypass permissions on")));
    client.peer_mut().connected = false;
    settle(&mut client)?;
    assert!(client
        .preset_line()
        .is_some_and(|line| line.starts_with("Bypass permissions on")));
    client.peer_mut().unreadable = true;
    assert!(client.refresh_permission_preset().is_err());
    assert!(client.preset_line().is_some_and(
        |line| line.starts_with("Bypass permissions on (last confirmed) | Preset unavailable:")
    ));
    client.peer_mut().unreadable = false;
    client.peer_mut().effective = Preset::Inspect;
    client.refresh_permission_preset()?;
    assert_eq!(
        client.preset_line().as_deref(),
        Some("Preset: bypass | effective: inspect")
    );
    client.peer_mut().unreadable = true;
    assert!(client.refresh_permission_preset().is_err());
    assert!(client
        .preset_line()
        .is_some_and(|line| line.starts_with("Preset unavailable:")));
    Ok(())
}

#[test]
fn bypass_indicator_uses_effective_readback_and_survives_failed_refresh() -> Result<(), ClientError>
{
    let mut client = preset_client(None)?;
    client.create_session()?;
    settle(&mut client)?;
    client.select_permission_preset(Preset::Bypass)?;
    assert!(
        !client.bypass_permissions_on(),
        "selected bypass can be narrowed by the server"
    );
    client.peer_mut().effective = Preset::Bypass;
    client.refresh_permission_preset()?;
    assert!(client.bypass_permissions_on());
    assert!(client
        .preset_line()
        .is_some_and(|line| line.starts_with(crate::preset::BYPASS_INDICATOR)));
    client.peer_mut().unreadable = true;
    assert!(client.refresh_permission_preset().is_err());
    assert!(
        client.bypass_permissions_on(),
        "failed readback must retain the last confirmed warning"
    );
    client.peer_mut().unreadable = false;
    client.peer_mut().effective = Preset::Maintainer;
    client.refresh_permission_preset()?;
    assert!(!client.bypass_permissions_on());
    Ok(())
}
