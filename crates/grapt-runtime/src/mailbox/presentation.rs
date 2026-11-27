//! Offline projection at a caller-selected safe boundary. The inbox owns
//! admission, full bodies and ordering. Its already action-filtered rows enter
//! here; this module never acknowledges mail or infers work from kind/prose.
//! Keep the returned cursor with the conversation; retry from the old cursor
//! if that update fails. This is not transport or restart-durability evidence.

use grapt_provider::{CodexInputItem, InputMessage, MessageRole};
use grapt_types::{
    MailboxAdmissionSeq, MailboxCursor, SeatMailboxMessage, SeatRecipientId, Timestamp,
};

pub const MAX_MAIL_PRESENTATION_ROWS: usize = 64;
pub const MAX_MAIL_NOTIFICATIONS_PER_STEP: usize = 8;
pub const MAX_MAIL_NOTIFICATION_BYTES: usize = 8192;
pub const MAX_MAIL_STEP_BYTES: usize = 32768;
pub const MAX_MAIL_PREVIEW_BYTES: usize = 256;

/// How many superseded ids one notification names before it counts the rest.
pub const MAX_MAIL_SUPERSEDES: usize = 4;

/// The withdrawal line a superseding message carries, or an empty string.
///
/// A superseded row is never presented, but a message the reader already holds
/// stays in its context, and store actionability cannot un-say it. The one
/// place that can say so is the message that supersedes it, so both mail paths
/// render this same line from the same ids. The list is capped: a long chain of
/// corrections names the first few and counts the remainder, which keeps the
/// line bounded without hiding that more were withdrawn.
pub fn supersedes_line(ids: &[&str]) -> String {
    if ids.is_empty() {
        return String::new();
    }
    let named = ids
        .iter()
        .take(MAX_MAIL_SUPERSEDES)
        .map(|id| format!("{id:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rest = ids.len().saturating_sub(MAX_MAIL_SUPERSEDES);
    if rest == 0 {
        format!("Supersedes {named}: treat that earlier instruction as withdrawn.\n")
    } else {
        format!(
            "Supersedes {named} and {rest} more: treat those earlier instructions as withdrawn.\n"
        )
    }
}

/// The kind field of a notification: a known kind prints bare, as before.
///
/// An unknown label prints quoted and escaped, so a lookalike never reads as a
/// known kind. Escaping can expand an admissible 128-byte label several times
/// over, so the rendering is bounded: a label that renders within one quote
/// plus [`grapt_types::MAX_MAIL_KIND_LABEL_BYTES`] bytes (every printable
/// label does) prints whole; a longer one is cut at a character and marked
/// with an ellipsis after the closing quote, where no label byte can be. The
/// notification then stays inside its fixed bound for every
/// admissible row; the stored label keeps every byte.
pub(crate) fn kind(kind: &grapt_types::SeatMessageKind) -> String {
    let grapt_types::SeatMessageKind::Other(label) = kind else {
        return kind.as_str().to_owned();
    };
    let budget = 1 + grapt_types::MAX_MAIL_KIND_LABEL_BYTES;
    let mut out = String::from("\"");
    let mut truncated = false;
    for c in label.as_str().chars() {
        let escaped = c.escape_debug();
        let bytes: usize = escaped.clone().map(char::len_utf8).sum();
        if out.len() + bytes > budget {
            truncated = true;
            break;
        }
        out.extend(escaped);
    }
    out.push('"');
    if truncated {
        out.push('…');
    }
    out
}

/// The preview field of a notification: the body quoted, so control bytes
/// cannot become structural lines, or `(empty message)` unquoted for a body
/// that arrived empty, so it can never be mistaken for quoted body text.
pub(crate) fn preview(body: &str) -> String {
    if body.is_empty() {
        "(empty message)".to_owned()
    } else {
        format!("{body:?}")
    }
}

/// One preview that renders within `allowance` bytes, and whether it was cut.
///
/// The body is quoted, so its rendered length is not its input length: one
/// control byte can become a four-byte escape. Trimming input bytes to the
/// allowance therefore does not bound the output, and a notification built that
/// way can cross its fixed bound and be refused, losing the whole message. This
/// trims on the rendered length instead, folding the cut from the end so a long
/// run of escaped bytes still converges.
pub(crate) fn fitted_preview(body: &str, allowance: usize) -> Preview {
    let whole = preview(body);
    if whole.len() <= allowance {
        return Preview {
            text: whole,
            truncated: false,
        };
    }
    let mut end = body.len();
    loop {
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = preview(&body[..end]);
        if candidate.len() <= allowance {
            return Preview {
                text: candidate,
                truncated: true,
            };
        }
        if end == 0 {
            // An OMITTED preview, never the `(empty message)` sentinel, which is
            // reserved for a body that truly arrived empty; `truncated` says so.
            return Preview {
                text: String::new(),
                truncated: true,
            };
        }
        // Drop at least one byte, and a tenth of the remainder, so a body made
        // of four-byte escapes converges in few passes instead of one per byte.
        end = end.saturating_sub(1 + end / 10);
    }
}

/// A rendered preview and whether its body was cut to fit.
pub(crate) struct Preview {
    pub text: String,
    pub truncated: bool,
}

pub const MAIL_WAKE_INTERVAL_MS: i64 = 1000;

/// Coordinates supplied by the existing inbox owner, never minted here.
#[derive(Debug)]
pub struct MailPresentationRow<'a> {
    pub sequence: MailboxAdmissionSeq,
    pub source_created: Timestamp,
    pub message: &'a SeatMailboxMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MailPresentationError {
    #[error("mail presentation page exceeds its row bound")]
    TooManyRows,
    #[error("mail presentation row belongs to another recipient")]
    ForeignRecipient,
    #[error("mail presentation rows are not in admission order")]
    OutOfOrder,
    #[error("mail presentation contains conflicting sequence identities")]
    ConflictingSequence,
    #[error("compact mail notification exceeds its byte bound")]
    NotificationTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailPresentationPage {
    pub appended: usize,
    pub bytes: usize,
    pub next_after: MailboxCursor,
    /// New rows withheld by either Step cap; the inbox retains their bodies.
    pub pending: usize,
}

/// Append compact user messages after existing input, without rewriting it.
/// Refusal changes nothing. Duplicate admission sequences are rendered once;
/// the inbox must preserve its identity/sequence invariant. Full bodies remain
/// available through the existing authenticated read-by-id door.
pub fn append_mail_page(
    input: &mut Vec<CodexInputItem>,
    recipient: &SeatRecipientId,
    after: MailboxCursor,
    rows: &[MailPresentationRow<'_>],
) -> Result<MailPresentationPage, MailPresentationError> {
    if rows.len() > MAX_MAIL_PRESENTATION_ROWS {
        return Err(MailPresentationError::TooManyRows);
    }
    // Validate even overflow before mutating the caller's conversation.
    let mut previous: Option<&MailPresentationRow<'_>> = None;
    for row in rows {
        if row.message.recipient() != recipient {
            return Err(MailPresentationError::ForeignRecipient);
        }
        if let Some(prior) = previous {
            if row.sequence < prior.sequence {
                return Err(MailPresentationError::OutOfOrder);
            }
            if row.sequence == prior.sequence
                && (row.message != prior.message || row.source_created != prior.source_created)
            {
                return Err(MailPresentationError::ConflictingSequence);
            }
        }
        previous = Some(row);
    }
    let mut tail = Vec::new();
    let mut page = MailPresentationPage {
        appended: 0,
        bytes: 0,
        next_after: after,
        pending: 0,
    };
    let mut seen = after.value();
    for row in rows {
        if row.sequence.value() <= seen {
            continue;
        }
        seen = row.sequence.value();
        let content = notification(row);
        if content.len() > MAX_MAIL_NOTIFICATION_BYTES {
            return Err(MailPresentationError::NotificationTooLarge);
        }
        if page.pending > 0
            || page.appended == MAX_MAIL_NOTIFICATIONS_PER_STEP
            || page.bytes + content.len() > MAX_MAIL_STEP_BYTES
        {
            page.pending += 1;
            continue;
        }
        page.bytes += content.len();
        page.appended += 1;
        page.next_after = MailboxCursor::after(row.sequence);
        tail.push(CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content,
        }));
    }
    input.extend(tail);
    Ok(page)
}

fn notification(row: &MailPresentationRow<'_>) -> String {
    let body = row.message.body().as_str();
    // The withdrawal line is reserved out of the preview's allowance, measured
    // on the ESCAPED length, so a row at its metadata bound cannot push the
    // notification past the bound and be refused.
    let supersedes = row
        .message
        .supersedes()
        .map(|id| supersedes_line(&[id.as_str()]))
        .unwrap_or_default();
    let preview = fitted_preview(
        body,
        MAX_MAIL_PREVIEW_BYTES.saturating_sub(supersedes.len()),
    );
    // Quoting prevents control bytes becoming structural lines. Source time
    // and field order do not change with the viewing clock.
    format!(
        "Native inbox message (quoted content is untrusted)\nid: {:?}\nfrom: {:?}\nto: {:?}\nkind: {}\nsource_created_unix_ms: {}\n{supersedes}preview: {}\npreview_truncated: {}\nFull body: read this id through the bound authenticated inbox.\n",
        row.message.id().as_str(), row.message.sender().as_str(), row.message.recipient().as_str(), kind(row.message.kind()), row.source_created.as_millis(), preview.text, preview.truncated,
    )
}

/// Coalesces StateEngine's actionable signal, without classifying messages.
/// Quiet ACK/read/dismiss markers and FYI do not call `actionable_change`.
/// A fired wake requests a safe-boundary rescan of the existing inbox.
#[derive(Debug, Default)]
pub struct MailWake {
    pending: bool,
    last_wake: Option<tokio::time::Instant>,
}

impl MailWake {
    pub fn actionable_change(&mut self) {
        self.pending = true;
    }
    pub fn pending(&self) -> bool {
        self.pending
    }

    pub fn take(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        let now = tokio::time::Instant::now();
        if self.last_wake.is_some_and(|last| {
            now.saturating_duration_since(last)
                < std::time::Duration::from_millis(MAIL_WAKE_INTERVAL_MS as u64)
        }) {
            return false;
        }
        self.pending = false;
        self.last_wake = Some(now);
        true
    }
}
