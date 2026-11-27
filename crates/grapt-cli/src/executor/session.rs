//! Rename through the established API and the ordinary service discovery owner.
use crate::{command::ConnectionChoice, CliLimits, ExitClass};
use grapt_types::SessionId;

pub(super) async fn rename(
    connection: Option<ConnectionChoice>,
    session: SessionId,
    title: String,
    limits: CliLimits,
) -> u8 {
    let result = tokio::select! {
        result = save(connection, &session, &title, limits) => result,
        _signal = super::termination_signal() => return ExitClass::Cancelled.code(),
    };
    let (class, message) = match result {
        Ok(()) => (ExitClass::Success, format!("Renamed {session}: {title}")),
        Err((class, message)) => (class, format!("grapt: {message}")),
    };
    let emitted = tokio::select! {
        result = super::emit(message, class != ExitClass::Success, limits) => result,
        _signal = super::termination_signal() => return ExitClass::Cancelled.code(),
    };
    if emitted.is_ok() {
        class.code()
    } else {
        ExitClass::Output.code()
    }
}

async fn save(
    connection: Option<ConnectionChoice>,
    session: &SessionId,
    title: &str,
    limits: CliLimits,
) -> Result<(), (ExitClass, String)> {
    let connection = match connection {
        Some(connection) => connection,
        None => ConnectionChoice::Discover {
            service_root: crate::command::terminal::user_profile(|name| std::env::var_os(name))
                .ok_or_else(|| {
                    (
                        ExitClass::Usage,
                        "default profile is unavailable; provide --service <root>".to_owned(),
                    )
                })?,
        },
    };
    let client = super::connect(connection, limits).await?;
    client
        .rename_session(session, title)
        .await
        .map_err(|error| {
            (
                error.exit_class(),
                format!("rename was not confirmed: {error}"),
            )
        })
}
