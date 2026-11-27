//! Authenticated offline retirement of one captured service registration.

use std::io::{IsTerminal, Read};
use std::path::Path;

use base64::Engine;
use grapt_auth_basic::BasicAuthVerifier;
use grapt_service::{RegistrationRecord, ServiceController, StopOutcome};
use grapt_store::SystemClock;

use crate::{input, ready, startup};

const MAX_REGISTRATION_BYTES: usize = 8_512;

pub(crate) const HELP: &str = "usage: grapt-server retire-service --service <absolute-profile> --expected-registration <captured-record>\n\nRetire only the captured registration after its service has stopped. Supply the\nprofile's username:secret credential on stdin, never as an argument. A live\nservice, changed registration or foreign ready receipt refuses. No process is\nstarted or signaled, and no database is opened. An already-empty slot is an\nobservation, not proof that this invocation performed an earlier retirement.";

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn run(arguments: &[String]) -> Result<String, String> {
    if arguments.len() != 4
        || arguments[0] != "--service"
        || arguments[2] != "--expected-registration"
        || arguments.iter().any(|value| {
            value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
        })
    {
        return Err(HELP.into());
    }
    let root = Path::new(&arguments[1]);
    if !root.is_absolute() {
        return Err("--service must name an absolute existing profile directory".into());
    }
    let metadata = std::fs::symlink_metadata(root).map_err(detail)?;
    if !metadata.is_dir() || linked(&metadata) {
        return Err("--service must name a real profile directory".into());
    }
    let root = root.canonicalize().map_err(detail)?;
    let expected_bytes = read_regular(Path::new(&arguments[3]), MAX_REGISTRATION_BYTES)?;
    let expected = RegistrationRecord::decode(&expected_bytes).map_err(detail)?;

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err("pipe the profile username:secret credential to stdin; terminal entry is refused to avoid echoing it".into());
    }
    let mut presented = Vec::new();
    stdin
        .lock()
        .take(387)
        .read_to_end(&mut presented)
        .map_err(|_| {
            "credential stdin could not be read; supply one username:secret record".to_owned()
        })?;
    if presented.len() > 386 {
        return Err("credential stdin exceeds its bound".into());
    }
    let presented = credential_record(&presented)?;
    let header = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(presented)
    );
    // Authenticate before election creates its lock file, then revalidate
    // under election so a credential rotation cannot leave old authority live.
    authenticate(&root, &header)?;

    let authority = startup::acquire(&root).map_err(|error| {
        format!("retirement could not acquire the service election: {error}; stop the owning service before retrying")
    })?;
    authenticate(&root, &header)?;
    // All supported product owners retain this election for their lifetime.
    // Holding it proves absence independently of numeric PID reuse and keeps
    // a successor out until the exact-byte release and readback finish.
    let published = grapt_platform::PublishedConfig::load(&root).map_err(detail)?;
    let config = input::read_config(&root.join("service.conf"), published.build().as_str())
        .map_err(detail)?;
    let owner = expected.owner();
    if owner.slot().as_str() != config.slot || owner.environment().as_str() != config.environment {
        return Err("captured registration belongs to another profile slot or environment".into());
    }
    let controller = ServiceController::new(
        owner.slot().clone(),
        owner.environment().clone(),
        &authority,
        &SystemClock,
    );
    let registration = root.join("registration");
    // Refuse special files before the controller's ordinary file read, which
    // assumes a cooperating profile and must not block on a FIFO here.
    match std::fs::symlink_metadata(&registration) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(detail(error)),
        Ok(_) => {
            read_regular(&registration, MAX_REGISTRATION_BYTES)?;
        }
    }
    let discovered = controller.discover().map_err(detail)?;
    if discovered.bytes().is_none() {
        match std::fs::symlink_metadata(root.join(ready::READY_RECEIPT_FILE)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(detail(error)),
            Ok(_) => return Err("registration is empty but a ready receipt remains; its custody cannot be retired without its registration".into()),
        }
        return receipt("slot_already_empty", &expected, false, false, None);
    }
    if discovered.bytes() != Some(expected_bytes.as_slice()) {
        return Err("registration changed or is unreadable; capture the intended owner's registration before retrying".into());
    }
    let dead = startup::DeadPredecessor::observed_dead(owner.clone());
    match std::fs::symlink_metadata(root.join(ready::READY_RECEIPT_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(detail(error)),
        Ok(_) => {
            read_regular(&root.join(ready::READY_RECEIPT_FILE), 8192)?;
        }
    }
    // Retire ready custody first. If interrupted before the registration CAS,
    // a retry can finish using the same captured registration and absent ready
    // receipt; an empty slot can never retain this operation's old ready file.
    let ready_retired = ready::retire_dead_predecessor(&root, &dead).map_err(detail)?;
    match controller.stop(&discovered, owner).map_err(|error| {
        format!("ready receipt retired={ready_retired}; registration release failed: {error}; retry with the same captured registration")
    })? {
        StopOutcome::Released { unlock_warning, .. } => {
            let observed = controller.discover().map_err(|error| {
                format!("registration release committed but readback failed: {error}; retry to observe the slot")
            })?;
            if observed.bytes().is_some() {
                return Err("registration release committed but the slot is no longer empty; inspect the current owner".into());
            }
            receipt(
                "released",
                &expected,
                true,
                ready_retired,
                unlock_warning.map(|warning| {
                    format!(
                        "registration committed; unlock failed: {:?}",
                        warning.error_kind()
                    )
                }),
            )
        }
        StopOutcome::LostRace { .. } | StopOutcome::NotTheOwner { .. } => Err(
            "registration changed before release; the current registration was preserved".into(),
        ),
        StopOutcome::SlotAlreadyEmpty => {
            Err("registration disappeared before release; retry to observe the slot".into())
        }
    }
}

fn authenticate(root: &Path, header: &str) -> Result<(), String> {
    let credential_bytes = read_regular(&root.join("basic.credential"), 256)?;
    let credential = credential_record(&credential_bytes)?;
    let (username, secret) = credential
        .split_once(':')
        .ok_or("profile Basic credential is malformed")?;
    BasicAuthVerifier::new(username, secret)
        .map_err(detail)?
        .verify(header.as_bytes())
        .map_err(detail)?;
    Ok(())
}

fn receipt(
    outcome: &str,
    expected: &RegistrationRecord,
    released: bool,
    ready_retired: bool,
    warning: Option<String>,
) -> Result<String, String> {
    let owner = expected.owner();
    serde_json::to_string(&serde_json::json!({
        "schema": "secode.product.service-retirement.v1", "outcome": outcome,
        "service_slot": owner.slot().as_str(), "service_instance": owner.instance().as_str(),
        "environment": owner.environment().as_str(), "generation": owner.generation().value(),
        "pid": owner.pid().value(), "endpoint": owner.endpoint().as_str(),
        "registration_released": released, "ready_receipt_retired": ready_retired,
        "unlock_warning": warning,
    }))
    .map_err(detail)
}

fn credential_record(bytes: &[u8]) -> Result<&str, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "credential must be UTF-8")?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err("credential must be one username:secret record".into());
    }
    Ok(text)
}

fn read_regular(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself and prohibit replacement while held.
        options.custom_flags(0x0020_0000).share_mode(1);
    }
    let file = options.open(path).map_err(detail)?;
    let metadata = file.metadata().map_err(detail)?;
    if !metadata.is_file() || linked(&metadata) || metadata.len() > maximum as u64 {
        return Err("retirement input must be a bounded regular file without links".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(detail)?;
    if bytes.len() > maximum {
        return Err("retirement input grew beyond its bound".into());
    }
    Ok(bytes)
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn detail(error: impl std::fmt::Display) -> String {
    error.to_string()
}
