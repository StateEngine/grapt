//! The coherent session snapshot. Every durable section is read on one pinned
//! read lease, with the session's committed position P read first, so rows,
//! permissions, forms and execution state all describe the store at P. The
//! open live text is captured under the hub lock once the lease has finished,
//! never while it is held; the client reconciles it by block identity.
//!
//! The read owns its path: inside the lease it touches only the lease's own
//! store and immutable artifacts, never a writer-owned lock, and every section
//! stays typed from its producer to the response; nothing is re-decoded.
//!
//! Without a read pool (an in-memory store) no lease can pin the read, so the
//! route answers unavailable and the client keeps its legacy reads.
use super::*;
use grapt_api::u3a::{
    FormInfoDto, MessageListQueryDto, MessageListView, PermissionListResponseDto,
    SessionFormListResponseDto, SessionMessageInfoDto, SessionMessagesApplicationError as Error,
    SessionMessagesResponseDtoCursor, SessionSnapshotExecutionDto, SessionSnapshotProvisionalDto,
    SessionSnapshotProvisionalKindDto, SessionSnapshotResponseDto, SessionSnapshotRevisionDto,
};
use grapt_schema::Required;

/// The window a client installs on attach: the newest page, newest first.
const WINDOW: &str = r#"{"limit":"200","order":"desc"}"#;

/// The durable sections at P, typed.
struct Sections {
    seq: u64,
    rows: Vec<SessionMessageInfoDto>,
    cursor: SessionMessagesResponseDtoCursor,
    permissions: PermissionListResponseDto,
    forms: SessionFormListResponseDto,
    execution: SessionSnapshotExecutionDto,
}

impl grapt_api::u3a::SessionSnapshotApplicationPort for DogfoodApplication {
    fn session_snapshot(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
    ) -> ApplicationFuture<SessionSnapshotResponseDto, Error> {
        if self.readers.is_none() {
            return ready(Err(Error::Unavailable));
        }
        let session = session_id.into_inner();
        let pinned = session.clone();
        let read = self.read_projection(
            move |application| sections(application, &principal, &pinned),
            |_| Error::Unavailable,
        );
        let hub = Arc::clone(&self.hub);
        Box::pin(async move {
            let sections = read.await?;
            let blocks = hub.provisional(&session).map_err(|_| Error::Unavailable)?;
            let provisional = blocks
                .into_iter()
                .map(provisional_block)
                .collect::<Result<Vec<_>, Error>>()?;
            assemble(&session, sections, provisional)
        })
    }
}

/// Every durable section, on the pinned lease, at the position read first.
fn sections(
    application: &DogfoodApplication,
    principal: &SealedPrincipal,
    session: &SessionId,
) -> Result<Sections, Error> {
    // The lease's own inbox, never a writer's: read P first.
    let seq = revision(application.lock_inbox().store().connection(), session)?;
    if application.get_record(session).is_err() {
        return Err(Error::SessionNotFound(not_found(session)));
    }
    let query: MessageListQueryDto =
        serde_json::from_str(WINDOW).map_err(|error| Error::Unknown(unknown(error)))?;
    let page = history::MessagePage::parse(query)?;
    let (rows, cursor) = application
        .history_typed(session, &page, MessageListView::Durable)
        .map_err(history::PageRefusal::into_application)?;
    let inbox = application.lock_inbox();
    let store = inbox.store();
    let failed = |error: String| Error::Unknown(unknown(error));
    let permissions = permission::pinned_rows(store, principal, session).map_err(failed)?;
    let forms = pending_forms(store, session).map_err(failed)?;
    let fields = application
        .executor
        .session_state_fields(store.connection(), session);
    let execution = decode(Value::Object(fields)).map_err(failed)?;
    Ok(Sections {
        seq,
        rows,
        cursor,
        permissions,
        forms,
        execution,
    })
}

/// P: the session's committed position, the lease's first read.
fn revision(connection: &rusqlite::Connection, session: &SessionId) -> Result<u64, Error> {
    let seq: Option<i64> = connection
        .query_row(
            "SELECT max(seq) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| Error::Unavailable)?;
    seq.and_then(|seq| u64::try_from(seq).ok())
        .ok_or_else(|| Error::SessionNotFound(not_found(session)))
}

/// The session's pending forms on the lease store, typed as the form route types them.
fn pending_forms(
    store: &grapt_store::EventStore,
    session: &SessionId,
) -> Result<SessionFormListResponseDto, String> {
    let forms = grapt_form::catalog::pending_forms_for(store, Some(session))
        .map_err(|error| error.to_string())?;
    decode(json!({"data": forms.into_iter().map(|form| form.info).collect::<Vec<_>>()}))
}

/// One live block, typed straight from the registry.
fn provisional_block(
    held: grapt_api::ProvisionalText,
) -> Result<SessionSnapshotProvisionalDto, Error> {
    // The registry reports a text-less block as incomplete before this.
    let text = held.text.ok_or(Error::Unavailable)?;
    let bytes = held.bytes;
    let text = grapt_api::u3a::BoundedString::try_new(text)
        .map_err(|error| Error::Unknown(unknown(error)))?;
    let message = WireId::parse(held.block.message.as_str().to_owned())
        .ok_or_else(|| Error::Unknown(unknown("live block message identity")))?;
    Ok(SessionSnapshotProvisionalDto::new(
        Required::new(message),
        Required::new(match held.block.kind {
            grapt_api::ProvisionalKind::Reasoning => SessionSnapshotProvisionalKindDto::Reasoning,
            grapt_api::ProvisionalKind::Text => SessionSnapshotProvisionalKindDto::Text,
        }),
        Required::new(held.block.ordinal.into()),
        Required::new(text),
        Required::new(bytes.into()),
    ))
}

/// The typed response, refused if its encoded bytes exceed the page bound.
fn assemble(
    session: &SessionId,
    sections: Sections,
    provisional: Vec<SessionSnapshotProvisionalDto>,
) -> Result<SessionSnapshotResponseDto, Error> {
    let bounded = |error: &str| Error::Unknown(unknown(error));
    let forms: Vec<FormInfoDto> = sections.forms.data().get().as_slice().to_vec();
    let aggregate =
        WireId::parse(session.as_str().to_owned()).ok_or_else(|| bounded("session identity"))?;
    let value = SessionSnapshotResponseDto::new(
        Required::new(SessionSnapshotRevisionDto::new(
            Required::new(aggregate),
            Required::new(sections.seq.into()),
        )),
        Required::new(grapt_api::u3a::BoundedVec::try_new(sections.rows).map_err(bounded)?),
        Required::new(sections.cursor),
        Required::new(sections.permissions.data().get().clone()),
        Required::new(grapt_api::u3a::BoundedVec::try_new(forms).map_err(bounded)?),
        Required::new(sections.execution),
        Required::new(grapt_api::u3a::BoundedVec::try_new(provisional).map_err(bounded)?),
    );
    let limits =
        grapt_api::u3a::U3aLimits::production().map_err(|error| Error::Unknown(unknown(error)))?;
    grapt_api::u3a::encode_page(&value, limits).map_err(|error| Error::Unknown(unknown(error)))?;
    Ok(value)
}
