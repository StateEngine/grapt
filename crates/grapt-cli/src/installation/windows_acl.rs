//! Read the installation's security descriptors on Windows and judge them.
//!
//! The workspace forbids `unsafe`, so no Win32 security call is made here.
//! Three programs Windows ships do the reading:
//! `whoami /user` names the current user's SID, `icacls /save` writes each
//! object's DACL and mandatory label, and PowerShell's `Get-Acl` reports the
//! owner. Each runs from `%SystemRoot%\System32` by absolute path, bounded in
//! time and output, into a private scratch directory, and its output is
//! parsed strictly; anything else is "could not verify", never a pass.
//!
//! Trust in those programs: only TrustedInstaller, SYSTEM and Administrators
//! can write `System32` as Windows ships it, and `%SystemRoot%` comes from
//! this process's own environment, which another account cannot set. An
//! account that can replace them is already an administrator. Their own
//! descriptors are judged under the system rule as well, which catches a
//! loosened System32; a malicious administrator is out of scope.
//!
//! Residual window (stated in the user documentation): the check runs at
//! update, revert and first install, not at every start.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::acl::{self, Descriptor, Refusal, Rule};
use super::io;
use super::windows_reader::{command_path, run, run_at, Scratch};

/// The most any reading program may print or save.
const OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// Test-only: make the reader fail, as a blocked or missing tool would.
    pub(super) static READER_FAULT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// The programs the check runs, by absolute path.
struct Tools {
    whoami: PathBuf,
    icacls: PathBuf,
    powershell: PathBuf,
    /// The directories and programs judged under the system rule.
    trusted: Vec<PathBuf>,
}

fn tools() -> Result<Tools, Refusal> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
        .ok_or_else(|| Refusal::Unverifiable("SystemRoot is not an absolute path".to_owned()))?;
    let system32 = root.join("System32");
    let shell = system32.join(r"WindowsPowerShell\v1.0");
    let tools = Tools {
        whoami: system32.join("whoami.exe"),
        icacls: system32.join("icacls.exe"),
        powershell: shell.join("powershell.exe"),
        trusted: Vec::new(),
    };
    Ok(Tools {
        trusted: vec![
            system32.clone(),
            tools.whoami.clone(),
            tools.icacls.clone(),
            shell,
            tools.powershell.clone(),
        ],
        ..tools
    })
}

/// The current user's SID from `whoami /user /fo csv /nh`: one line,
/// `"name","SID"`. The name is in the console code page; the SID column is
/// ASCII and is all this reads.
fn user(tools: &Tools, scratch: &Path) -> Result<String, Refusal> {
    let arguments = ["/user", "/fo", "csv", "/nh"].map(OsString::from);
    let output = run(&tools.whoami, &arguments, scratch, "whoami.out", &[])?;
    let malformed = || Refusal::Unverifiable("whoami reported an unexpected user line".to_owned());
    let line = output
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let [line] = line.as_slice() else {
        return Err(malformed());
    };
    let start = line
        .windows(3)
        .rposition(|window| window == b"\",\"")
        .ok_or_else(malformed)?;
    let sid = line[start + 3..]
        .strip_suffix(b"\"")
        .and_then(|sid| std::str::from_utf8(sid).ok())
        .ok_or_else(malformed)?;
    acl::account(sid).map_err(|_| malformed())
}

/// One object's DACL and label from `icacls <path> /save <file>`: UTF-16,
/// the object's name on one line and its SDDL on the next.
fn saved(
    tools: &Tools,
    scratch: &Path,
    object: &Path,
    index: usize,
) -> Result<Descriptor, Refusal> {
    let shown = command_path(object)?;
    let file = scratch.join(format!("acl-{index}.txt"));
    let arguments = [
        OsString::from(&shown),
        OsString::from("/save"),
        file.clone().into_os_string(),
    ];
    run(
        &tools.icacls,
        &arguments,
        scratch,
        &format!("icacls-{index}.out"),
        &[],
    )?;
    let bytes = io::read(&file, OUTPUT_BYTES).map_err(Refusal::Unverifiable)?;
    let malformed =
        || Refusal::Unverifiable(format!("icacls saved an unexpected record for {shown}"));
    if !bytes.len().is_multiple_of(2) {
        return Err(malformed());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let units = units.strip_prefix(&[0xFEFF]).unwrap_or(&units);
    let text = String::from_utf16(units).map_err(|_| malformed())?;
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .collect();
    let [_name, sddl] = lines.as_slice() else {
        return Err(malformed());
    };
    if !sddl.starts_with("D:") {
        return Err(malformed());
    }
    acl::parse(sddl).map_err(|reason| Refusal::Unverifiable(format!("{shown}: {reason}")))
}

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Line {
    #[serde(rename = "mode")]
    Mode { mode: String },
    #[serde(rename = "owner")]
    Owner { index: usize, sddl: String },
}

/// Every object's owner from one PowerShell run. The paths travel in a
/// UTF-8 list in the scratch directory, not on the command line, so a large
/// build cannot overflow it. The script prints JSON lines this module
/// defines: the language mode first, then each object's SDDL by index
/// (paths are never echoed, so the console code page cannot garble them).
///
/// The child does not inherit `PSModulePath` (review ACL-H2): launched
/// from PowerShell 7, the parent's value names pwsh's module directories,
/// and Windows PowerShell 5.1 then cannot autoload `Get-Acl`. Without it,
/// 5.1 builds its own default, as Microsoft documents for this nesting.
fn owners(tools: &Tools, scratch: &Path, objects: &[PathBuf]) -> Result<Vec<Descriptor>, Refusal> {
    let list = scratch.join("objects.txt");
    let names: String = objects
        .iter()
        .map(|object| command_path(object).map(|name| name + "\n"))
        .collect::<Result<_, _>>()?;
    std::fs::write(&list, names).map_err(|error| {
        Refusal::Unverifiable(format!("object list cannot be written: {:?}", error.kind()))
    })?;
    let script = "$ErrorActionPreference = 'Stop'\n\
         [pscustomobject]@{ kind = 'mode'; mode = $ExecutionContext.SessionState.LanguageMode.ToString() } | ConvertTo-Json -Compress\n\
         $index = 0\n\
         foreach ($path in @(Get-Content -LiteralPath $env:GRAPT_SECURITY_PATH -Encoding UTF8)) {\n\
         [pscustomobject]@{ kind = 'owner'; index = $index; sddl = (Get-Acl -LiteralPath $path).Sddl } | ConvertTo-Json -Compress\n\
         $index++\n\
         }\n";
    let encoded = acl::encoded_command(script);
    let arguments = [
        "-NoProfile",
        "-NonInteractive",
        "-NoLogo",
        "-EncodedCommand",
        encoded.as_str(),
    ]
    .map(OsString::from);
    let output = run_at(
        &tools.powershell,
        &arguments,
        scratch,
        "owners.out",
        &["PSModulePath"],
        None,
        Some(&list),
    )?;
    let malformed =
        || Refusal::Unverifiable("PowerShell reported unexpected owner lines".to_owned());
    let text = std::str::from_utf8(&output).map_err(|_| malformed())?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    match lines.next().map(serde_json::from_str::<Line>) {
        Some(Ok(Line::Mode { mode })) if mode == "FullLanguage" => {}
        Some(Ok(Line::Mode { mode })) => {
            return Err(Refusal::Unverifiable(format!(
                "PowerShell language mode {mode}"
            )));
        }
        _ => return Err(malformed()),
    }
    let mut descriptors = Vec::with_capacity(objects.len());
    for (expected, line) in lines.enumerate() {
        match serde_json::from_str::<Line>(line) {
            Ok(Line::Owner { index, sddl }) if index == expected && index < objects.len() => {
                descriptors.push(acl::parse(&sddl).map_err(|reason| {
                    Refusal::Unverifiable(format!(
                        "{}: {reason}",
                        super::display_path(&objects[index])
                    ))
                })?);
            }
            _ => return Err(malformed()),
        }
    }
    if descriptors.len() == objects.len() {
        Ok(descriptors)
    } else {
        Err(malformed())
    }
}

/// One recursive save on exactly the already inspected immutable build.
/// Relative names are resolved from its real parent, then matched one-for-one.
fn saved_tree(
    tools: &Tools,
    scratch: &Path,
    root: &Path,
    objects: &[PathBuf],
) -> Result<Vec<Descriptor>, Refusal> {
    let parent = root
        .parent()
        .ok_or_else(|| Refusal::Unverifiable("build has no parent".into()))?;
    let name = root
        .file_name()
        .ok_or_else(|| Refusal::Unverifiable("build has no name".into()))?;
    let file = scratch.join("build-acls.txt");
    run_at(
        &tools.icacls,
        &[
            name.into(),
            "/save".into(),
            file.clone().into_os_string(),
            "/T".into(),
            "/L".into(),
            "/Q".into(),
        ],
        scratch,
        "build-acls.out",
        &[],
        Some(parent),
        None,
    )?;
    let expected = objects
        .iter()
        .map(|object| {
            object
                .strip_prefix(parent)
                .map_err(|_| Refusal::Unverifiable("inspected ACL object escaped its build".into()))
                .and_then(command_path)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let raw = io::read(&file, OUTPUT_BYTES).map_err(Refusal::Unverifiable)?;
    super::acl_batch::parse(&raw, &expected).map_err(Refusal::Unverifiable)
}

fn read_and_judge(prefix: &Path, target: &str) -> Result<(), Refusal> {
    #[cfg(test)]
    if let Some(reason) = READER_FAULT.with(std::cell::Cell::get) {
        return Err(Refusal::Unverifiable(reason.to_owned()));
    }
    let tools = tools()?;
    let scratch = Scratch::new()?;
    let user = user(&tools, &scratch.0)?;
    let objects = super::acl_objects::installation(prefix, target, super::acl_objects::MAX_TREE)?;
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| Refusal::Unverifiable("LOCALAPPDATA is unknown".into()))?;
    let local = std::fs::canonicalize(local).map_err(|error| {
        Refusal::Unverifiable(format!(
            "LOCALAPPDATA cannot be resolved: {:?}",
            error.kind()
        ))
    })?;
    let ancestors = super::acl_objects::user_ancestors(prefix, &local)?;
    let all: Vec<PathBuf> = tools
        .trusted
        .iter()
        .chain(&ancestors)
        .chain(&objects)
        .cloned()
        .collect();
    let owners = owners(&tools, &scratch.0, &all)?;
    let root = prefix.join("versions").join(target);
    let build: Vec<_> = all
        .iter()
        .filter(|object| object.starts_with(&root))
        .cloned()
        .collect();
    let mut batch = saved_tree(&tools, &scratch.0, &root, &build)?.into_iter();
    let rule = Rule::Installation { user };
    for (index, (object, owner)) in all.iter().zip(&owners).enumerate() {
        let saved = if object.starts_with(&root) {
            batch
                .next()
                .ok_or_else(|| Refusal::Unverifiable("build ACL batch ended early".into()))?
        } else {
            saved(&tools, &scratch.0, object, index)?
        };
        let descriptor = saved.with_owner_of(owner).map_err(Refusal::Unverifiable)?;
        let rule = if index < tools.trusted.len() {
            &Rule::System
        } else {
            &rule
        };
        acl::judge(rule, &super::display_path(object), &descriptor)?;
    }
    Ok(())
}

/// Refuse the activation unless the installation's permissions are safe.
pub(super) fn verify(prefix: &Path, target: &str) -> Result<(), String> {
    super::progress::during(true, "Checking installation permissions", |_reporter| {
        read_and_judge(prefix, target).map_err(|refusal| refusal.message())
    })
}
