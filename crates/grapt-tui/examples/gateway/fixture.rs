//! The accepted read contract only; this fixture never issues a binding grant.
use super::gateway::{contract::*, Reply, Request};

fn revision() -> Result<Revision, Box<dyn std::error::Error>> {
    Ok(Revision(
        std::num::NonZeroU64::new(1).ok_or("fixture revision")?,
    ))
}
fn page<T>(items: Vec<T>) -> Read<Page<T>> {
    let returned = items.len() as u64;
    Read::Present {
        value: Page {
            items,
            next: None,
            window: Window {
                source: Source::Configuration {
                    location: "gateway fixture".into(),
                },
                start_cursor: None,
                end_cursor: None,
                returned,
                limit: 100,
                bytes: 1024,
                max_bytes: 65_536,
                omitted_before: Omission::None,
                omitted_after: Omission::None,
                unreadable: 0,
                stop_reason: StopReason::Exhausted,
                complete: true,
            },
        },
    }
}
pub(super) fn read(request: &Request) -> Result<Reply, Box<dyn std::error::Error>> {
    Ok(match request {
        Request::ListProjects(_) => Reply::Projects(page(vec![ProjectInfo {
            id: ProjectId("fixture-project".into()),
            name: "Example project".into(),
            root: "/workspace/example".into(),
            revision: revision()?,
        }])),
        Request::ListAgents(project, _) => Reply::Agents(page(vec![AgentInfo {
            project: project.clone(),
            id: AgentId("fixture-agent".into()),
            name: "Example seat".into(),
            role: "Coding".into(),
            revision: revision()?,
        }])),
        Request::ProjectActiveSessions(project, _) => Reply::Sessions(page(vec![ActiveSession {
            activity: SessionActivity::Idle,
            target: HostSessionTarget {
                epoch: HostEpoch {
                    owner: ProcessInstance {
                        machine: "fixture".into(),
                        boot: "fixture-boot".into(),
                        pid: 1,
                        started: StartIdentity::Linux { start_ticks: 1 },
                        executable: "/fixture/grapt".into(),
                        argv_sha256: Sha256([0; 32]),
                    },
                    generation: HostGeneration(1),
                },
                session: HostSessionId("fixture-host-session".into()),
                binding: SessionIdentity {
                    project: project.clone(),
                    agent: AgentId("fixture-agent".into()),
                    session: SessionId("fixture-session".into()),
                },
                binding_fingerprint: Sha256([0; 32]),
            },
        }])),
    })
}

#[cfg(test)]
mod tests {
    use super::super::gateway::{Availability, Gateway};
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn connected_flow_selects_an_identity_without_issuing_a_grant_and_disconnected_skips_it(
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert!(Gateway::open(Availability::Disconnected).is_none());
        let mut gateway = Gateway::open(Availability::Connected).ok_or("connected gateway")?;
        for _ in 0..3 {
            let (ticket, request) = gateway.pending().ok_or("pending read")?;
            let reply = read(request)?;
            gateway.accept(ticket, Ok(reply));
            gateway.key(KeyCode::Enter);
        }
        let selection = gateway.take_selection().ok_or("selected session")?;
        assert_eq!(
            selection.identity.project,
            ProjectId("fixture-project".into())
        );
        assert_eq!(selection.identity.agent, AgentId("fixture-agent".into()));
        assert_eq!(
            selection.identity.session,
            SessionId("fixture-session".into())
        );
        gateway.key(KeyCode::Esc);
        gateway.key(KeyCode::F(5));
        let (ticket, _) = gateway.pending().ok_or("reloaded agents")?;
        gateway.accept(ticket - 1, Ok(Reply::Projects(page(Vec::new()))));
        assert!(gateway.pending().is_some());
        gateway.accept(
            ticket,
            Ok(Reply::Agents(Read::Unknown {
                what: "project disappeared".into(),
            })),
        );
        assert!(gateway.take_selection().is_none());
        Ok(())
    }
}
