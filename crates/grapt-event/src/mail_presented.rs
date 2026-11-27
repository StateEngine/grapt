//! One bounded conversation capture, referencing the canonical mailbox body.
//! Its v1 presentation is fixed; source attention may change after capture.
use grapt_types::remote_mail::{
    MailboxMessageDigest, ProjectPair, RemoteMailboxMessageId, SourcePrincipalId, SourceProjectId,
    TargetProjectId,
};
use grapt_types::{
    MailboxAdmissionSeq, MailboxMessageId, PhysicalAttemptId, SeatRecipientId, StepNumber,
};
use serde_json::{json, Value};

pub const MAX_PRESENTED_MAIL: usize = 8;
// 29 bounded strings at their worst JSON escape width, plus keys/integers.
pub const MAX_PRESENTATION_BYTES: usize = 49_152;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedMail {
    pub local_message: MailboxMessageId,
    pub source_message: RemoteMailboxMessageId,
    pub local_admission: MailboxAdmissionSeq,
    pub digest: MailboxMessageDigest,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailPresented {
    pub attempt: PhysicalAttemptId,
    pub step: StepNumber,
    pub projects: ProjectPair,
    pub principal: SourcePrincipalId,
    pub recipient: SeatRecipientId,
    messages: Vec<PresentedMail>,
}
impl MailPresented {
    pub fn new(
        attempt: PhysicalAttemptId,
        step: StepNumber,
        projects: ProjectPair,
        principal: SourcePrincipalId,
        recipient: SeatRecipientId,
        messages: Vec<PresentedMail>,
    ) -> Result<Self, String> {
        if attempt.as_str().len() > grapt_types::remote_mail::MAX_TRANSPORT_IDENTITY_BYTES {
            return Err("mail presentation attempt identity exceeds its bound".into());
        }
        if messages.is_empty() || messages.len() > MAX_PRESENTED_MAIL {
            return Err("mail presentation requires 1..8 messages".into());
        }
        let mut local = std::collections::BTreeSet::new();
        let mut previous = 0;
        for message in &messages {
            if message.local_admission.value() <= previous || !local.insert(&message.local_message)
            {
                return Err("mail presentation repeats or reorders an admission".into());
            }
            previous = message.local_admission.value();
        }
        Ok(Self {
            attempt,
            step,
            projects,
            principal,
            recipient,
            messages,
        })
    }
    pub fn messages(&self) -> &[PresentedMail] {
        &self.messages
    }
    pub fn encode(&self) -> Value {
        let messages: Vec<Value> = self.messages.iter().map(|m| json!({
            "local_message":m.local_message.as_str(), "source_message":m.source_message.as_str(),
            "local_admission":m.local_admission.value(), "digest":m.digest.as_str()
        })).collect();
        json!({"attempt":self.attempt.as_str(),"step":self.step.value(),
            "projects":{"source":self.projects.source().as_str(),"target":self.projects.target().as_str()},
            "principal":self.principal.as_str(),"recipient":self.recipient.as_str(),"messages":messages})
    }
    pub fn decode(v: &Value) -> Result<Self, String> {
        exact(
            v,
            &[
                "attempt",
                "step",
                "projects",
                "principal",
                "recipient",
                "messages",
            ],
        )?;
        exact(&v["projects"], &["source", "target"])?;
        let rows = v["messages"]
            .as_array()
            .filter(|rows| !rows.is_empty() && rows.len() <= MAX_PRESENTED_MAIL)
            .ok_or("mail presentation message bound")?;
        let mut messages = Vec::with_capacity(rows.len());
        for row in rows {
            exact(
                row,
                &[
                    "local_message",
                    "source_message",
                    "local_admission",
                    "digest",
                ],
            )?;
            messages.push(PresentedMail {
                local_message: MailboxMessageId::try_new(text(row, "local_message")?)
                    .map_err(|e| e.to_string())?,
                source_message: RemoteMailboxMessageId::try_new(text(row, "source_message")?)
                    .map_err(|e| e.to_string())?,
                local_admission: MailboxAdmissionSeq::new(integer(row, "local_admission")?)
                    .ok_or("zero mail admission")?,
                digest: MailboxMessageDigest::try_new(text(row, "digest")?)
                    .map_err(|e| e.to_string())?,
            });
        }
        Self::new(
            PhysicalAttemptId::new(text(v, "attempt")?).ok_or("invalid mail attempt")?,
            u32::try_from(integer(v, "step")?)
                .ok()
                .and_then(StepNumber::new)
                .ok_or("invalid mail Step")?,
            ProjectPair::new(
                SourceProjectId::try_new(text(&v["projects"], "source")?)
                    .map_err(|e| e.to_string())?,
                TargetProjectId::try_new(text(&v["projects"], "target")?)
                    .map_err(|e| e.to_string())?,
            ),
            SourcePrincipalId::try_new(text(v, "principal")?).map_err(|e| e.to_string())?,
            SeatRecipientId::try_new(text(v, "recipient")?).map_err(|e| e.to_string())?,
            messages,
        )
    }
}
fn exact(v: &Value, fields: &[&str]) -> Result<(), String> {
    let object = v.as_object().ok_or("mail presentation must be an object")?;
    if object.len() != fields.len() || fields.iter().any(|key| !object.contains_key(*key)) {
        return Err("mail presentation has missing or unknown fields".into());
    }
    Ok(())
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| format!("invalid mail presentation {key}"))
}
fn integer(v: &Value, key: &str) -> Result<u64, String> {
    v[key]
        .as_u64()
        .ok_or_else(|| format!("invalid mail presentation {key}"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_remote_ids_keep_distinct_local_admissions_and_duplicate_guards() -> Result<(), String>
    {
        let value = json!({"attempt":"attempt-origins","step":1,
        "projects":{"source":"receiving-floor","target":"secode"},
        "principal":"coder","recipient":"coder","messages":[
            {"local_message":"origin-one-local","source_message":"same-remote","local_admission":1,"digest":"digest-one"},
            {"local_message":"origin-two-local","source_message":"same-remote","local_admission":2,"digest":"digest-two"}
        ]});
        assert_eq!(MailPresented::decode(&value)?.encode(), value);
        let mut duplicate = value.clone();
        duplicate["messages"][1]["local_message"] =
            duplicate["messages"][0]["local_message"].clone();
        assert!(MailPresented::decode(&duplicate).is_err());
        let mut repeated = value.clone();
        repeated["messages"][1]["local_admission"] = json!(1);
        assert!(MailPresented::decode(&repeated).is_err());
        let mut reversed = value.clone();
        reversed["messages"]
            .as_array_mut()
            .ok_or("messages")?
            .reverse();
        assert!(MailPresented::decode(&reversed).is_err());
        let mut extra = value;
        extra["messages"][0]["source_project"] = json!("origin-one");
        assert!(MailPresented::decode(&extra).is_err());
        Ok(())
    }
    #[test]
    fn mail_capture_codec_preserves_scope_order_and_refuses_unknown_or_missing_fields(
    ) -> Result<(), String> {
        let v = json!({"attempt":"attempt-mail","step":1,"projects":{"source":"source","target":"target"},"principal":"coder","recipient":"coder","messages":[{"local_message":"mail-one","source_message":"source-one","local_admission":1,"digest":"digest-one"}]});
        assert_eq!(MailPresented::decode(&v)?.encode(), v);
        for field in [
            "attempt",
            "step",
            "projects",
            "principal",
            "recipient",
            "messages",
        ] {
            let mut bad = v.clone();
            bad[field] = Value::Null;
            assert!(MailPresented::decode(&bad).is_err());
            let mut bad = v.clone();
            bad.as_object_mut().ok_or("object")?.remove(field);
            assert!(MailPresented::decode(&bad).is_err());
        }
        for pointer in ["/projects", "/messages/0"] {
            let mut bad = v.clone();
            bad.pointer_mut(pointer)
                .and_then(Value::as_object_mut)
                .ok_or("object")?
                .insert("extra".into(), json!(1));
            assert!(MailPresented::decode(&bad).is_err());
        }
        let mut bad = v.clone();
        bad["messages"] = json!([]);
        assert!(MailPresented::decode(&bad).is_err());
        let mut bad = v.clone();
        bad["messages"] = json!(vec![v["messages"][0].clone(); 9]);
        assert!(MailPresented::decode(&bad).is_err());
        let mut bad = v.clone();
        bad["messages"] = json!(vec![v["messages"][0].clone(); 2]);
        assert!(MailPresented::decode(&bad).is_err());
        Ok(())
    }
}
