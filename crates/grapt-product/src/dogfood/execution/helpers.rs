fn promoted_text(
    inbox: &Arc<Mutex<Inbox>>,
    session: &SessionId,
    promotion: &Promotion,
) -> Result<String, String> {
    let guard = match inbox.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .promoted_input(session, promotion.input.message())
        .map(|input| input.text)
        .map_err(|error| format!("promoted input text is unavailable: {error}"))
}

pub(super) fn next_step(
    inbox: &Arc<Mutex<Inbox>>,
    session: &SessionId,
) -> Result<StepNumber, String> {
    let guard = match inbox.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let last: i64 = guard
        .store()
        .connection()
        .query_row(
            "SELECT COALESCE(MAX(CAST(json_extract(data, '$.step') AS INTEGER)), 0) \
             FROM event WHERE aggregate_id = ?1 \
               AND type IN ('session.step.ended.2', 'session.step.failed.2', 'session.step.ended.3', 'session.step.failed.3')",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("last Step unreadable: {error}"))?;
    last.checked_add(1)
        .and_then(|value| u32::try_from(value).ok())
        .and_then(StepNumber::new)
        .ok_or_else(|| "next Step identity was refused".to_owned())
}

fn dto_string(value: &str) -> Result<grapt_api::u3a::DtoString, ExecutionRefusal> {
    grapt_api::u3a::DtoString::try_new(value.to_owned())
        .map_err(|error| ExecutionRefusal::Run(error.to_owned()))
}

fn finite(value: i64) -> Result<FiniteF64, ExecutionRefusal> {
    FiniteF64::try_new(value as f64).map_err(|error| ExecutionRefusal::Run(error.to_owned()))
}

fn wire_event(event: &EventId) -> Result<WireId<EventId>, ExecutionRefusal> {
    WireId::parse(event.as_str().to_owned())
        .ok_or_else(|| ExecutionRefusal::Run("wire event identity was invalid".to_owned()))
}

fn wire_session(session: &SessionId) -> Result<WireId<SessionId>, ExecutionRefusal> {
    WireId::parse(session.as_str().to_owned())
        .ok_or_else(|| ExecutionRefusal::Run("wire session identity was invalid".to_owned()))
}

fn wire_message(message: &MessageId) -> Result<WireId<MessageId>, ExecutionRefusal> {
    WireId::parse(message.as_str().to_owned())
        .ok_or_else(|| ExecutionRefusal::Run("wire message identity was invalid".to_owned()))
}

fn durable_ref(session: &SessionId, seq: u64) -> Result<DurableEventRef, ExecutionRefusal> {
    let aggregate = AggregateIdDto::try_new(session.as_str().to_owned())
        .map_err(|error| ExecutionRefusal::Run(error.to_owned()))?;
    Ok(DurableEventRef::new(aggregate, seq.into()))
}
