use grapt_release::{
    domain_fingerprint, inspect_status, path_identity, rehearse, restore_backup_rehearsal,
};
use grapt_store::SystemClock;
use grapt_types::{MigrationId, RecoveryDisposition};
use serde::Serialize;
use std::path::{Path, PathBuf};

const MAX_ARGUMENTS: usize = 32;
const CUTOVER_REFUSAL: &str =
    "cutover is owned by the unified R6 release authority; migration operator cannot commit it";

#[derive(Debug, Serialize)]
struct Refusal<'a> {
    schema: &'static str,
    outcome: &'static str,
    reason: &'a str,
}

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let result = run(&arguments);
    match result {
        Ok(rendered) => println!("{rendered}"),
        Err(reason) => {
            let refusal = Refusal {
                schema: "grapt.release.operator.v1",
                outcome: "refused",
                reason: &reason,
            };
            match serde_json::to_string(&refusal) {
                Ok(rendered) => println!("{rendered}"),
                Err(_) => {
                    println!("{{\"schema\":\"grapt.release.operator.v1\",\"outcome\":\"refused\"}}")
                }
            }
            std::process::exit(2);
        }
    }
}

fn run(arguments: &[String]) -> Result<String, String> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(usage().to_owned());
    };
    if command == "cutover" {
        return Err(CUTOVER_REFUSAL.to_owned());
    }
    if arguments.len() > MAX_ARGUMENTS {
        return Err(format!(
            "operator accepts at most {MAX_ARGUMENTS} arguments"
        ));
    }
    match command {
        "rehearse" => {
            let (source, root, run_id, engine_version) = rehearsal_arguments(arguments)?;
            let _ = rehearse(&source, &root, run_id, &engine_version, &SystemClock)
                .map_err(|error| error.to_string())?;
            let status = inspect_status(&root).map_err(|error| error.to_string())?;
            serde_json::to_string(&status).map_err(|error| error.to_string())
        }
        "handoff" => {
            let (source, root, run_id, engine_version) = rehearsal_arguments(arguments)?;
            let rehearsal = rehearse(&source, &root, run_id, &engine_version, &SystemClock)
                .map_err(|error| error.to_string())?;
            let receipt = rehearsal
                .eligibility_receipt()
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&receipt).map_err(|error| error.to_string())
        }
        "status" => {
            reject_unknown(arguments, &["--run-root"])?;
            let root = PathBuf::from(required(arguments, "--run-root")?);
            let status = inspect_status(&root).map_err(|error| error.to_string())?;
            serde_json::to_string(&status).map_err(|error| error.to_string())
        }
        // The executable rollback. It had no command-line surface before, which
        // meant the only recovery path from a failed activation was reachable
        // solely from a Rust checkout — and the operator this exists for has a
        // broken install and a backup, not a toolchain.
        //
        // It never overwrites: the restored database is a NEW artifact beside
        // the one in service, so an operator inspects both truths before
        // choosing either. That is why no confirmation flag guards it.
        "restore" => {
            reject_unknown(arguments, &["--source", "--backup", "--restored"])?;
            let source = PathBuf::from(required(arguments, "--source")?);
            let backup = PathBuf::from(required(arguments, "--backup")?);
            let restored = PathBuf::from(required(arguments, "--restored")?);
            let receipt = restore_backup_rehearsal(&source, &backup, &restored)
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&RestoreReport {
                schema: "grapt.release.rollback-restore.v1",
                outcome: "restored",
                restored_database: receipt.path().to_path_buf(),
                restored_fingerprint: receipt.file_fingerprint().as_str().to_owned(),
                restored_domain_fingerprint: receipt.domain_fingerprint().as_str().to_owned(),
            })
            .map_err(|error| error.to_string())
        }
        // The truthful bounded recovery disposition: what a rollback would and
        // would not resume, stated BEFORE anyone acts on it.
        //
        // It reads the DURABLE JOURNAL rather than re-running the rehearsal,
        // and that is forced by when the question gets asked. By the time
        // anyone wants a rollback the served database has legitimately changed,
        // and resuming a rehearsal over a changed candidate is precisely what
        // the resurrection guard refuses (`CandidateChanged`). The journal
        // still holds both proven domains, so it is the only source that
        // survives the situation this command exists for.
        //
        // A missing digest refuses loudly rather than defaulting: "we did not
        // look" must never render as "nothing was lost".
        "disposition" => {
            reject_unknown(arguments, &["--run-root", "--served"])?;
            let root = PathBuf::from(required(arguments, "--run-root")?);
            let served = PathBuf::from(required(arguments, "--served")?);
            let status = inspect_status(&root).map_err(|error| error.to_string())?;
            if !status.ready_for_handoff {
                return Err(
                    "the rehearsal never reached ReadyForHandoff, so there is no proven \
                     activation baseline to compare against"
                        .to_owned(),
                );
            }
            let activation_baseline = recorded(status.clone_domain_digest, "candidate")?;
            let restored_domain = recorded(status.rollback_domain_digest, "rollback")?;
            let observed = domain_fingerprint_of(&served)?;
            let disposition = RecoveryDisposition::classify(
                restored_domain,
                activation_baseline,
                &observed,
                path_identity_of(&served),
            );
            serde_json::to_string(&DispositionReport::render(&disposition, &served))
                .map_err(|error| error.to_string())
        }
        _ => Err(usage().to_owned()),
    }
}

/// The restore operator's result.
#[derive(Debug, Serialize)]
struct RestoreReport {
    schema: &'static str,
    outcome: &'static str,
    restored_database: PathBuf,
    restored_fingerprint: String,
    restored_domain_fingerprint: String,
}

/// A rollback's truthful bounded disposition, rendered for an operator.
///
/// `restores_everything` is stated as its own field rather than left to be
/// inferred from the presence of the discard fields, because the one reading
/// that must never be reached by accident is "this was clean".
#[derive(Debug, Serialize)]
struct DispositionReport {
    schema: &'static str,
    outcome: &'static str,
    restores_everything: bool,
    served_database: PathBuf,
    restored_domain_fingerprint: String,
    /// Present only when work happened after activation.
    #[serde(skip_serializing_if = "Option::is_none")]
    activation_baseline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_domain_fingerprint: Option<String>,
    /// Where the work a rollback does not resume continues to live.
    #[serde(skip_serializing_if = "Option::is_none")]
    not_resumed_preserved_at: Option<String>,
}

impl DispositionReport {
    fn render(disposition: &RecoveryDisposition, served: &Path) -> Self {
        match disposition {
            RecoveryDisposition::RestoresEverything { restored_domain } => Self {
                schema: "grapt.release.recovery-disposition.v1",
                outcome: "rollback-restores-everything",
                restores_everything: true,
                served_database: served.to_path_buf(),
                restored_domain_fingerprint: restored_domain.as_str().to_owned(),
                activation_baseline: None,
                observed_domain_fingerprint: None,
                not_resumed_preserved_at: None,
            },
            RecoveryDisposition::DiscardsPostActivationWork {
                restored_domain,
                activation_baseline,
                observed_domain,
                preserved_at,
            } => Self {
                schema: "grapt.release.recovery-disposition.v1",
                outcome: "rollback-discards-post-activation-work",
                restores_everything: false,
                served_database: served.to_path_buf(),
                restored_domain_fingerprint: restored_domain.as_str().to_owned(),
                activation_baseline: Some(activation_baseline.as_str().to_owned()),
                observed_domain_fingerprint: Some(observed_domain.as_str().to_owned()),
                not_resumed_preserved_at: Some(
                    preserved_at
                        .as_canonical()
                        .unwrap_or("unresolvable")
                        .to_owned(),
                ),
            },
        }
    }
}

/// A digest the journal must actually carry.
///
/// Absent is refused rather than defaulted. A disposition computed against a
/// missing baseline would be an opinion, and the one opinion this command must
/// never express is an unfounded "nothing was lost".
fn recorded(
    digest: Option<String>,
    which: &str,
) -> Result<grapt_types::DatabaseFingerprint, String> {
    let digest = digest.ok_or_else(|| format!("the journal records no {which} domain digest"))?;
    grapt_types::DatabaseFingerprint::new(digest)
        .ok_or_else(|| format!("the journal's {which} domain digest is empty"))
}

fn domain_fingerprint_of(database: &Path) -> Result<grapt_types::DatabaseFingerprint, String> {
    domain_fingerprint(database).map_err(|error| error.to_string())
}

fn path_identity_of(database: &Path) -> grapt_types::path::PathIdentity {
    path_identity(database)
}

fn rehearsal_arguments(
    arguments: &[String],
) -> Result<(PathBuf, PathBuf, MigrationId, String), String> {
    rehearsal_arguments_with(
        arguments,
        &["--source", "--run-root", "--run-id", "--engine-version"],
    )
}

/// The rehearsal's four arguments, validated against a caller-supplied allowed
/// set so a verb that accepts one MORE flag does not have to restate the four.
fn rehearsal_arguments_with(
    arguments: &[String],
    allowed: &[&str],
) -> Result<(PathBuf, PathBuf, MigrationId, String), String> {
    reject_unknown(arguments, allowed)?;
    let source = PathBuf::from(required(arguments, "--source")?);
    let root = PathBuf::from(required(arguments, "--run-root")?);
    let run_id = MigrationId::new(required(arguments, "--run-id")?)
        .ok_or_else(|| "--run-id must be non-empty".to_owned())?;
    let engine_version = required(arguments, "--engine-version")?;
    Ok((source, root, run_id, engine_version))
}

fn required(arguments: &[String], flag: &str) -> Result<String, String> {
    let mut found = None;
    let mut index = 1_usize;
    while index < arguments.len() {
        if arguments[index] == flag {
            if found.is_some() {
                return Err(format!("duplicate argument {flag}"));
            }
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| format!("{flag} requires a value"))?;
            if value.starts_with("--") {
                return Err(format!("{flag} requires a value"));
            }
            found = Some(value.clone());
        }
        index = index.saturating_add(2);
    }
    found.ok_or_else(|| format!("missing required argument {flag}"))
}

fn reject_unknown(arguments: &[String], allowed: &[&str]) -> Result<(), String> {
    if arguments.len().is_multiple_of(2) {
        return Err(usage().to_owned());
    }
    let mut index = 1_usize;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        if !allowed.contains(&flag) {
            return Err(format!("unknown argument {flag:?}"));
        }
        index = index.saturating_add(2);
    }
    Ok(())
}

fn usage() -> &'static str {
    "usage: grapt-migrate \
     rehearse --source <db> --run-root <dir> --run-id <id> --engine-version <version> | \
     handoff --source <db> --run-root <dir> --run-id <id> --engine-version <version> | \
     status --run-root <dir> | \
     restore --source <db> --backup <db> --restored <dest> | \
     disposition --run-root <dir> --served <db>"
}
