//! Bounded read-only admission coordinates through the Inbox owner.
use super::{AdmissionCoordinates, Inbox, InboxError, MessageId, Value};
use std::collections::BTreeMap;

const PAGE_LIMIT: usize = 200;

impl Inbox {
    /// Read one stable input's exact admission coordinates.
    pub fn admission_coordinates(
        &self,
        input: &MessageId,
    ) -> Result<AdmissionCoordinates, InboxError> {
        self.admission_coordinate_page(std::slice::from_ref(input))?
            .into_iter()
            .next()
            .ok_or_else(|| corrupt(input, "admission coordinates are absent"))
    }

    /// Read at most 200 identities with one event scan. Results follow caller
    /// order, including repeated identities. Every requested input needs exactly
    /// one admission; malformed admission JSON anywhere still refuses the read.
    pub fn admission_coordinate_page(
        &self,
        inputs: &[MessageId],
    ) -> Result<Vec<AdmissionCoordinates>, InboxError> {
        if inputs.len() > PAGE_LIMIT {
            return Err(InboxError::CoordinatePageTooLong { limit: PAGE_LIMIT });
        }
        let Some(first) = inputs.first() else {
            return Ok(Vec::new());
        };
        let mut coordinates: BTreeMap<&str, (&MessageId, Option<AdmissionCoordinates>)> = inputs
            .iter()
            .map(|input| (input.as_str(), (input, None)))
            .collect();
        let wanted = serde_json::json!(inputs.iter().map(MessageId::as_str).collect::<Vec<_>>());
        let mut statement = self.store.connection().prepare(
            "SELECT seq, created, data,
                    CASE WHEN json_valid(data) THEN json_extract(data, '$.inputID') END
             FROM event WHERE type = 'session.input.admitted.1'
               AND (json_valid(data) = 0 OR json_extract(data, '$.inputID')
                    IN (SELECT value FROM json_each(?1)))",
        )?;
        let mut rows = statement.query([wanted.to_string()])?;
        while let Some(row) = rows.next()? {
            let data: String = row.get(2)?;
            let value: Value = serde_json::from_str(&data).map_err(|error| {
                corrupt(
                    first,
                    format!("admitted event data is not valid JSON: {error}"),
                )
            })?;
            let queried: String = row.get(3)?;
            // SQLite reads the first duplicate JSON key; serde_json reads the
            // last. Keep the original decoder's identity corroboration.
            if value.get("inputID").and_then(Value::as_str) != Some(queried.as_str()) {
                return Err(corrupt(
                    first,
                    "admitted event identity disagrees with the requested input",
                ));
            }
            let (input, slot) = coordinates.get_mut(queried.as_str()).ok_or_else(|| {
                corrupt(
                    first,
                    "admitted event identity is outside the requested page",
                )
            })?;
            if slot.is_some() {
                return Err(corrupt(
                    input,
                    "expected exactly one admitted event for coordinates, found at least 2",
                ));
            }
            let sequence: i64 = row.get(0)?;
            let seq = u64::try_from(sequence).map_err(|_| {
                corrupt(
                    input,
                    format!("admitted event sequence {sequence} is negative"),
                )
            })?;
            *slot = Some(AdmissionCoordinates {
                seq,
                created_ms: row.get(1)?,
            });
        }
        inputs
            .iter()
            .map(|input| {
                coordinates
                    .get(input.as_str())
                    .and_then(|(_, coordinates)| *coordinates)
                    .ok_or_else(|| {
                        corrupt(
                            input,
                            "expected exactly one admitted event for coordinates, found 0",
                        )
                    })
            })
            .collect()
    }
}

fn corrupt(input: &MessageId, reason: impl Into<String>) -> InboxError {
    InboxError::Corruption {
        input: input.clone(),
        reason: reason.into(),
    }
}
