use crate::write::{FencedWriter, WriteError};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, EventId,
    PhysicalAttemptId, RequestSentMarker, ReservedMarker, SessionId, SettledMarker, StepNumber,
    Timestamp, WriterEpoch,
};
use std::num::NonZeroU32;
/// The canonical record format tag. A stored row that does not begin with
/// exactly these bytes is not a record this decoder will guess at.
///
/// `2` because the record shape changed when the Step's exact terminal
/// coordinates joined it (R2C5I ruling 2). The bump is deliberate even though no
/// durable `SEATR1` row has ever existed outside this lane's own history: a
/// format tag that survives a field change is a tag that no longer identifies a
/// format.
const RECORD_TAG: &[u8; 6] = b"SEATR2";

/// The maximum WHOLE-RECORD size, in bytes, a caller admits.
///
/// # Why a byte ceiling and not only a row count
///
/// A row count cannot bound memory while a row is unbounded: `rows × row` is
/// unbounded at any row count, because `PhysicalAttemptId` and `SessionId` have
/// no length bound of their own (`opaque_id!` refuses only the empty string;
/// `parse_prefixed` refuses only a wrong prefix and a non-alphanumeric byte).
/// This is the bound that actually bounds memory.
///
/// This paragraph used to name A0's `ReconciliationPage` and
/// `ReconciliationCeiling` as the row-bounding counterparts. Both names are
/// gone from this lane: R2C5I moved cumulative pass budgeting to B, which passes
/// a remaining-byte budget in per call, so A1 names neither type and this crate
/// no longer reasons about pages at all. The receipt said as much while the
/// prose here still cited them.
///
/// # It has no `Default`, no constant and no production value
///
/// No production `PhysicalAttemptId` mint or prefix is ruled, so this crate
/// chooses no number: the caller supplies a `NonZeroU32` and the ruling that
/// fixes it supplies the value. Measured encoded sizes are reported in the
/// lane's residuals; none of them is baked here.
///
/// # It is also the decoder's identity bound
///
/// A record that fits the ceiling contains identities that fit it too, since
/// every record is `56 + attempt + session` bytes. So the decoder reconstructs
/// A0's bounded identity wrappers under THIS maximum, and claims no separate
/// identity bound — there is only one number and it is this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptRecordCeiling(pub(super) NonZeroU32);

impl AttemptRecordCeiling {
    pub fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }

    pub(super) fn as_bytes(self) -> usize {
        self.0.get() as usize
    }
}

/// The exact durable Step terminal this attempt will settle into.
///
/// # Why this is persisted at RESERVE, before the provider is opened
///
/// R2b-3 mints one `StepSettlementContext` per logical Step and it is
/// PROCESS-LOCAL. If the receipt does not durably retain the exact terminal
/// identity, a fresh process after a crash cannot prove whether that terminal
/// committed, cannot reproduce the exact append, and cannot name the event a
/// reclaim must read back. R2C5I finding 3: the earlier design demanded the
/// caller supply an `EventId` that a recovering process had never seen.
///
/// Persisting it also removes a whole class of error: recovery never MINTS a
/// replacement terminal identity, so a restart cannot produce a second terminal
/// or different event bytes for one Step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCoordinates {
    pub(super) event: EventId,
    pub(super) created_ms: i64,
}

impl TerminalCoordinates {
    pub fn new(event: EventId, created_ms: i64) -> Self {
        Self { event, created_ms }
    }

    pub fn event(&self) -> &EventId {
        &self.event
    }

    pub fn created_ms(&self) -> i64 {
        self.created_ms
    }
}

/// The canonical record: the A0 receipt, the elected epoch it was reserved
/// under, and the exact Step terminal coordinates it will settle into.
///
/// The epoch and the terminal coordinates are part of the RECORD, not merely
/// columns, so the column/bytes agreement grid is total over every indexed
/// column. A store-owned column with no encoded counterpart would be the one
/// field corruption could move without contradicting anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptReceiptRecord {
    pub(super) receipt: DurableAttemptReceipt,
    pub(super) writer_epoch: WriterEpoch,
    pub(super) terminal: TerminalCoordinates,
}

impl AttemptReceiptRecord {
    pub fn new(
        receipt: DurableAttemptReceipt,
        writer_epoch: WriterEpoch,
        terminal: TerminalCoordinates,
    ) -> Self {
        Self {
            receipt,
            writer_epoch,
            terminal,
        }
    }

    pub fn receipt(&self) -> &DurableAttemptReceipt {
        &self.receipt
    }

    pub fn writer_epoch(&self) -> WriterEpoch {
        self.writer_epoch
    }

    /// The exact terminal this attempt settles into. Recovery reuses these
    /// bytes; it never mints a replacement.
    pub fn terminal(&self) -> &TerminalCoordinates {
        &self.terminal
    }
}

/// One receipt offered for durable reservation.
#[derive(Debug)]
pub struct ReserveInput<'a> {
    /// The receipt in its reserved state. A receipt that already carries a
    /// marker is still admissible — an exact retry after a crash re-offers the
    /// state it had — and reconciles only when byte-identical.
    pub receipt: &'a DurableAttemptReceipt,
    /// The token this caller was ELECTED with for the receipt's session
    /// aggregate. A [`FencedWriter`] rather than a bare epoch, because an epoch
    /// a caller could name is not a fence.
    pub writer: FencedWriter,
    /// The EXACT Step terminal this attempt will settle into, from the one
    /// R2b-3 settlement context minted for this logical Step. Supplied here, at
    /// reserve, because a process that crashes after the provider opens must be
    /// able to prove or reproduce that terminal without having seen it.
    pub terminal: &'a TerminalCoordinates,
    /// The whole-record byte ceiling, decided ONCE here, before any I/O.
    pub ceiling: AttemptRecordCeiling,
}

/// How a reservation settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptReceiptReserved {
    /// The row was written.
    Reserved,
    /// A byte-identical canonical record already existed under this attempt id;
    /// nothing was written. Only byte-identical canonical data reconciles.
    Idempotent,
}

/// One row a bounded recovery pass must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryRow {
    /// A receipt the pass may settle or reclaim. Already decoded and already
    /// proven to agree with its own columns.
    Receipt(AttemptReceiptRecord),
    /// A row that could not be trusted. Reported, never skipped: a silently
    /// skipped row is indistinguishable from an absent one, and an absent
    /// receipt reads as "no external call was ever made". It carries NO
    /// `ReceiptDisposition`, so it can never be classified replay-safe, and the
    /// caller must durably quarantine it before advancing its cursor.
    Untrustworthy {
        attempt: PhysicalAttemptId,
        /// Read with `length()`, without fetching the blob.
        declared_bytes: usize,
        reason: QuarantineReason,
    },
}

/// What one step of a bounded recovery pass found.
///
/// Total over the three outcomes, so "nothing came back" can never be
/// ambiguous between an exhausted listing and a budget that ran out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryStep {
    /// One row to act on, and the exclusive cursor that follows it.
    Row {
        row: RecoveryRow,
        next_after: PhysicalAttemptId,
        /// Bytes this step actually MATERIALIZED. Zero for a row refused from
        /// its declared size alone.
        bytes_charged: usize,
    },
    /// The next row does not fit the remaining budget, so it was NOT fetched
    /// and the cursor did NOT move. The caller yields and starts a new pass.
    ///
    /// This is the check order the row-size addendum requires: read the next
    /// row's declared size, compare against the remaining budget, THEN refuse
    /// or materialize. A collect can only discover it has exceeded a budget
    /// after it already has.
    BudgetExhausted {
        declared_bytes: usize,
        remaining: usize,
    },
    /// No further rows for this session.
    Exhausted,
}

/// Why a receipt row could not be trusted. Closed: a reconciler behaves
/// differently for the two, and an open vocabulary would let a third condition
/// arrive without an owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    /// The stored bytes did not decode through the canonical decoder.
    Undecodable,
    /// The stored record is larger than the injected whole-record ceiling. The
    /// bytes are never materialized to discover this — `length()` answers it.
    OverCeiling,
}

impl QuarantineReason {
    pub fn as_str(self) -> &'static str {
        match self {
            QuarantineReason::Undecodable => "undecodable",
            QuarantineReason::OverCeiling => "over-ceiling",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "undecodable" => Some(QuarantineReason::Undecodable),
            "over-ceiling" => Some(QuarantineReason::OverCeiling),
            _ => None,
        }
    }
}

/// How a quarantine settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptQuarantined {
    /// The row was moved into the quarantine and removed from the live table.
    Quarantined,
    /// An identical quarantine record already existed. Idempotent, so a pass
    /// that crashed after committing can re-run.
    AlreadyQuarantined,
}

// ---------------------------------------------------------------------------
// Canonical encoding
// ---------------------------------------------------------------------------

/// The fixed part of every encoded record, in bytes.
///
/// tag(6) + attempt len(4) + session len(4) + terminal event len(4) + step(4)
/// + ordinal(4) + writer epoch(4) + reserved(8) + terminal created(8)
/// + send permit(1+8) + settled(1+8).
pub(super) const FIXED_RECORD_BYTES: usize = 6 + 4 + 4 + 4 + 4 + 4 + 4 + 8 + 8 + 9 + 9;

/// Encodes one record into its exact canonical bytes.
///
/// Explicit and length-prefixed rather than derived: this crate owns the format,
/// nothing is inferred from field order at runtime, and `grapt-types` stays
/// serde-free (F1 ruling).
///
/// Both optional markers are FIXED WIDTH — see the module doc. The absent form
/// is a zero presence byte followed by eight ZERO bytes, and the decoder refuses
/// a non-zero payload behind an absent flag rather than ignoring it.
pub fn encode_record(record: &AttemptReceiptRecord) -> Vec<u8> {
    let receipt = &record.receipt;
    let terminal = &record.terminal;
    let mut out = Vec::with_capacity(
        FIXED_RECORD_BYTES + receipt.identity_bytes() + terminal.event.as_str().len(),
    );
    out.extend_from_slice(RECORD_TAG);
    push_field(&mut out, receipt.attempt().as_str().as_bytes());
    push_field(&mut out, receipt.session().as_str().as_bytes());
    push_field(&mut out, terminal.event.as_str().as_bytes());
    out.extend_from_slice(&receipt.step().value().to_be_bytes());
    out.extend_from_slice(&receipt.ordinal().value().to_be_bytes());
    out.extend_from_slice(&record.writer_epoch.value().to_be_bytes());
    out.extend_from_slice(&receipt.reserved_at().as_millis().to_be_bytes());
    out.extend_from_slice(&terminal.created_ms.to_be_bytes());
    push_marker(&mut out, receipt.request_sent_at());
    push_marker(&mut out, receipt.settled_at());
    out
}

fn push_field(out: &mut Vec<u8>, bytes: &[u8]) {
    // A length that cannot be represented cannot be encoded. Every record that
    // reaches this function has already passed the whole-record ceiling, which
    // is a `NonZeroU32`, so no field here can exceed `u32::MAX`.
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Nine bytes, always: presence plus value. The absent form is deliberately the
/// same width as the present one, which is what keeps a record's encoded length
/// constant across its whole life.
fn push_marker(out: &mut Vec<u8>, at: Option<Timestamp>) {
    match at {
        Some(at) => {
            out.push(1);
            out.extend_from_slice(&at.as_millis().to_be_bytes());
        }
        None => {
            out.push(0);
            out.extend_from_slice(&0i64.to_be_bytes());
        }
    }
}

/// A strict reader over the canonical bytes.
///
/// Every failure is a REFUSAL with a reason, never a default value: a wrong tag,
/// a truncated field, trailing bytes, invalid UTF-8, an empty identity, a
/// non-boolean presence byte and a valued-but-absent marker all fail closed.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < RECORD_TAG.len() || &bytes[..RECORD_TAG.len()] != RECORD_TAG {
            return Err("record does not begin with the canonical attempt-receipt tag".to_owned());
        }
        Ok(Reader {
            bytes,
            at: RECORD_TAG.len(),
        })
    }

    fn take(&mut self, len: usize, name: &str) -> Result<&'a [u8], String> {
        let stop = self
            .at
            .checked_add(len)
            .filter(|stop| *stop <= self.bytes.len())
            .ok_or_else(|| format!("{name} is truncated"))?;
        let slice = &self.bytes[self.at..stop];
        self.at = stop;
        Ok(slice)
    }

    fn field(&mut self, name: &str) -> Result<String, String> {
        let raw_len = self.take(4, &format!("{name} length"))?;
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(raw_len);
        let len = u32::from_be_bytes(len_bytes) as usize;
        let raw = self.take(len, name)?;
        std::str::from_utf8(raw)
            .map(str::to_owned)
            .map_err(|error| format!("{name} is not UTF-8: {error}"))
    }

    fn u32(&mut self, name: &str) -> Result<u32, String> {
        let raw = self.take(4, name)?;
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(raw);
        Ok(u32::from_be_bytes(bytes))
    }

    fn i64(&mut self, name: &str) -> Result<i64, String> {
        let raw = self.take(8, name)?;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(raw);
        Ok(i64::from_be_bytes(bytes))
    }

    /// Presence plus value, always nine bytes.
    ///
    /// A non-zero value behind a zero presence byte is REFUSED rather than
    /// ignored: silently dropping it would let two different byte strings decode
    /// to one record, and exact-retry reconciliation compares bytes.
    fn marker(&mut self, name: &str) -> Result<Option<Timestamp>, String> {
        let present = self.take(1, &format!("{name} presence"))?[0];
        let value = self.i64(name)?;
        match present {
            0 if value == 0 => Ok(None),
            0 => Err(format!(
                "{name} is marked absent but carries the value {value}"
            )),
            1 => Ok(Some(Timestamp::from_millis(value))),
            other => Err(format!("{name} presence byte {other} is not 0 or 1")),
        }
    }

    /// Trailing bytes are a refusal: a record with something appended is not the
    /// record it claims to be.
    fn finish(self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(format!(
                "{} trailing byte(s) after the record",
                self.bytes.len() - self.at
            ))
        }
    }
}

/// Decodes canonical bytes under an injected whole-record ceiling.
///
/// The ceiling is also the identity bound — see [`AttemptRecordCeiling`]. It is
/// applied to the BYTES before anything is parsed, so an oversize record is
/// refused rather than decoded and then measured.
pub fn decode_record(
    bytes: &[u8],
    ceiling: AttemptRecordCeiling,
) -> Result<AttemptReceiptRecord, String> {
    if bytes.len() > ceiling.as_bytes() {
        return Err(format!(
            "record is {} bytes, above the {}-byte ceiling",
            bytes.len(),
            ceiling.get()
        ));
    }
    let mut reader = Reader::new(bytes)?;
    let attempt = reader.field("attempt id")?;
    let session = reader.field("session id")?;
    let terminal_event = reader.field("terminal event id")?;
    let step = reader.u32("step")?;
    let ordinal = reader.u32("ordinal")?;
    let writer_epoch = reader.u32("writer epoch")?;
    let reserved = reader.i64("reserved instant")?;
    let terminal_created = reader.i64("terminal created instant")?;
    let request_sent = reader.marker("send permit")?;
    let settled = reader.marker("settled")?;
    reader.finish()?;

    let attempt = PhysicalAttemptId::new(attempt).ok_or("attempt id is empty")?;
    let attempt = BoundedAttemptId::new(attempt, ceiling.0)
        .map_err(|error| format!("attempt id exceeds the record ceiling: {error:?}"))?;
    let session: SessionId = session
        .parse()
        .map_err(|_| "session id is not a well-formed session identity".to_owned())?;
    let session = BoundedSessionId::new(session, ceiling.0)
        .map_err(|error| format!("session id exceeds the record ceiling: {error:?}"))?;
    let step = StepNumber::new(step).ok_or("step is zero, which is not a step")?;
    let ordinal = AttemptOrdinal::new(ordinal).ok_or("ordinal is zero, which is not an ordinal")?;
    let writer_epoch =
        WriterEpoch::new(writer_epoch).ok_or("writer epoch is zero, which is not elected")?;
    let terminal_event: EventId = terminal_event
        .parse()
        .map_err(|_| "terminal event id is not a well-formed event identity".to_owned())?;

    let receipt = DurableAttemptReceipt::reserved(
        attempt,
        session,
        step,
        ordinal,
        ReservedMarker::new(Timestamp::from_millis(reserved)),
    );
    // Applied through A0's own consuming transitions, so a decoded receipt can
    // only reach a state the type itself admits. The outcomes are `Recorded` by
    // construction — the receipt was just minted with no markers — and are
    // discarded rather than compared, because a decode is not a marker write.
    let receipt = match request_sent {
        Some(at) => receipt.mark_request_sent(RequestSentMarker::new(at)).0,
        None => receipt,
    };
    let receipt = match settled {
        Some(at) => receipt.mark_settled(SettledMarker::new(at)).0,
        None => receipt,
    };
    Ok(AttemptReceiptRecord::new(
        receipt,
        writer_epoch,
        TerminalCoordinates::new(terminal_event, terminal_created),
    ))
}

/// The marker instants must not run backwards.
///
/// # Disclosed refinement of the preflight's phrasing
///
/// The preflight named this refusal as "`request_sent_ms` present while
/// `reserved_ms` absent". That state is UNREPRESENTABLE here twice over:
/// `reserved_ms` is `NOT NULL` in the table, and the canonical encoding carries
/// the reserved instant as a mandatory fixed field, so no row and no byte string
/// can express it. Implementing a refusal for it would be an arm nobody could
/// ever reach — the "unreachable fiction" class of defect.
///
/// The order law that IS violable by a foreign writer or by corruption is the
/// monotonicity of the three instants, and that is what this refusal carries.
pub(super) fn verify_marker_order(record: &AttemptReceiptRecord) -> Result<(), WriteError> {
    let receipt = &record.receipt;
    let attempt = receipt.attempt().as_str().to_owned();
    let reserved = receipt.reserved_at().as_millis();
    if let Some(sent) = receipt.request_sent_at() {
        if sent.as_millis() < reserved {
            return Err(WriteError::ReceiptMarkerOrder {
                attempt,
                detail: format!(
                    "request sent at {} precedes the reserve at {reserved}",
                    sent.as_millis()
                ),
            });
        }
    }
    if let Some(settled) = receipt.settled_at() {
        let settled_ms = settled.as_millis();
        if settled_ms < reserved {
            return Err(WriteError::ReceiptMarkerOrder {
                attempt,
                detail: format!("settled at {settled_ms} precedes the reserve at {reserved}"),
            });
        }
        if let Some(sent) = receipt.request_sent_at() {
            if settled_ms < sent.as_millis() {
                return Err(WriteError::ReceiptMarkerOrder {
                    attempt,
                    detail: format!(
                        "settled at {settled_ms} precedes the request sent at {}",
                        sent.as_millis()
                    ),
                });
            }
        }
    }
    Ok(())
}
