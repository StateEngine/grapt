//! Identity-only launcher boots bind the first resume principal, not an ACK.
use super::E;

pub(super) struct Identity {
    agent: String,
    project: String,
    fingerprint: String,
}
impl Identity {
    pub(super) fn parse(text: &str) -> Result<Self, E> {
        let mut lines = text.lines();
        let agent = identity_line(lines.next(), "agent_id: ")?;
        let project = identity_line(lines.next(), "project_id: ")?;
        let fingerprint = lines
            .next()
            .and_then(|line| line.strip_prefix("identity_fingerprint: "))
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(E::PacketInvalid)?;
        if lines.next() != Some("Call stateengine_resume({}) before work.")
            || lines.next().is_some()
        {
            return Err(E::PacketInvalid);
        }
        Ok(Self {
            agent: agent.to_owned(),
            project: project.to_owned(),
            fingerprint: fingerprint.to_owned(),
        })
    }
    pub(super) fn fingerprint(&self, project: &str, agent: &str) -> Result<&str, E> {
        if self.project != project || self.agent != agent {
            return Err(E::Principal);
        }
        Ok(&self.fingerprint)
    }
}
fn identity_line<'a>(line: Option<&'a str>, prefix: &str) -> Result<&'a str, E> {
    line.and_then(|line| line.strip_prefix(prefix))
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 256
                && value.trim() == *value
                && !value.chars().any(char::is_control)
        })
        .ok_or(E::PacketInvalid)
}
