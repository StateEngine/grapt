//! Judge a Windows security descriptor, spelled as SDDL, for the installation.
//!
//! Pure: no I/O and no Win32 calls (the workspace forbids `unsafe`), so it is
//! compiled and witnessed on every platform. The Windows reader
//! (`windows_acl`) supplies SDDL from `icacls /save` (DACL and mandatory
//! label) and PowerShell's `Get-Acl` (owner); this module decides.
//!
//! The rule: another account or a
//! lower-integrity program that can write the prefix, `bin\`, `versions\` or
//! a selection record can swap the programs the launcher runs. Parsing is
//! strict and every unrecognised token refuses: an entry this module cannot
//! read is never assumed harmless.

/// SDDL's two-letter SID aliases this module recognises.
const ALIASES: [(&str, &str); 15] = [
    ("SY", "S-1-5-18"),
    ("BA", "S-1-5-32-544"),
    ("BU", "S-1-5-32-545"),
    ("BG", "S-1-5-32-546"),
    ("AU", "S-1-5-11"),
    ("WD", "S-1-1-0"),
    ("IU", "S-1-5-4"),
    ("AN", "S-1-5-7"),
    ("LS", "S-1-5-19"),
    ("NS", "S-1-5-20"),
    ("AC", "S-1-15-2-1"),
    ("CO", "S-1-3-0"),
    ("CG", "S-1-3-1"),
    ("OW", "S-1-3-4"),
    ("PS", "S-1-5-10"),
];

/// Mandatory-label aliases: the integrity level is the SID's last part.
const LABELS: [(&str, &str); 5] = [
    ("UN", "S-1-16-0"),
    ("LW", "S-1-16-4096"),
    ("ME", "S-1-16-8192"),
    ("HI", "S-1-16-12288"),
    ("SI", "S-1-16-16384"),
];

pub(super) const SYSTEM: &str = "S-1-5-18";
pub(super) const ADMINISTRATORS: &str = "S-1-5-32-544";
/// NT SERVICE\TrustedInstaller, the owner of System32 and its programs.
pub(super) const TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const CREATOR_OWNER: &str = "S-1-3-0";
const MEDIUM: u32 = 0x2000;

/// Access-mask bits that let a holder change, add, remove or re-permission
/// an object, or that imply them. Everything else (read, execute, list,
/// synchronize, read permissions) is harmless.
const WRITE_ISH: u32 = 0x0000_0002 // FILE_WRITE_DATA / FILE_ADD_FILE
    | 0x0000_0004 // FILE_APPEND_DATA / FILE_ADD_SUBDIRECTORY
    | 0x0000_0010 // FILE_WRITE_EA
    | 0x0000_0040 // FILE_DELETE_CHILD
    | 0x0000_0100 // FILE_WRITE_ATTRIBUTES
    | 0x0001_0000 // DELETE
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000 // WRITE_OWNER
    | 0x0100_0000 // ACCESS_SYSTEM_SECURITY
    | 0x0200_0000 // MAXIMUM_ALLOWED
    | 0x1000_0000 // GENERIC_ALL
    | 0x4000_0000; // GENERIC_WRITE

/// SDDL access-right tokens, as bits. Directory-service spellings are bit
/// positions, so they resolve to the same file rights.
const RIGHTS: [(&str, u32); 21] = [
    ("GA", 0x1000_0000),
    ("GR", 0x8000_0000),
    ("GW", 0x4000_0000),
    ("GX", 0x2000_0000),
    ("RC", 0x0002_0000),
    ("SD", 0x0001_0000),
    ("WD", 0x0004_0000),
    ("WO", 0x0008_0000),
    ("CC", 0x0000_0001),
    ("DC", 0x0000_0002),
    ("LC", 0x0000_0004),
    ("SW", 0x0000_0008),
    ("RP", 0x0000_0010),
    ("WP", 0x0000_0020),
    ("DT", 0x0000_0040),
    ("LO", 0x0000_0080),
    ("CR", 0x0000_0100),
    ("FA", 0x001F_01FF),
    ("FR", 0x0012_0089),
    ("FW", 0x0012_0116),
    ("FX", 0x0012_00A0),
];

/// One access-control entry, with its SID resolved to numeric form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ace {
    kind: AceKind,
    inherit_only: bool,
    mask: u32,
    sid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AceKind {
    Allow,
    Deny,
    /// A mandatory label: `mask` holds the NW/NR/NX policy bits.
    Label,
}

/// What one object's security descriptor says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Descriptor {
    owner: Option<String>,
    /// `None` is a NULL DACL: every account has every right.
    dacl: Option<Vec<Ace>>,
    has_dacl: bool,
    labels: Vec<Ace>,
}

/// Why an installation is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The permissions were read and are unsafe.
    Unsafe(String),
    /// The permissions could not be read or judged; nothing is assumed.
    Unverifiable(String),
}

impl Refusal {
    pub(super) fn message(&self) -> String {
        match self {
            Self::Unsafe(message) => format!("{message}; installation remains unchanged"),
            Self::Unverifiable(reason) => format!(
                "could not verify install permissions ({reason}); installation remains unchanged"
            ),
        }
    }
}

/// Who may own and write an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Rule {
    /// The installation: the user, SYSTEM and Administrators.
    Installation { user: String },
    /// A system program the reader runs: TrustedInstaller, SYSTEM and
    /// Administrators.
    System,
}

impl Rule {
    fn trusted(&self, sid: &str) -> bool {
        match self {
            Self::Installation { user } => sid == user || sid == SYSTEM || sid == ADMINISTRATORS,
            Self::System => sid == TRUSTED_INSTALLER || sid == SYSTEM || sid == ADMINISTRATORS,
        }
    }
}

/// A SID in numeric form, from an SDDL alias or a numeric SID; anything
/// else refuses.
pub(super) fn account(token: &str) -> Result<String, String> {
    if let Some((_, sid)) = ALIASES
        .iter()
        .chain(LABELS.iter())
        .find(|(alias, _)| *alias == token)
    {
        return Ok((*sid).to_owned());
    }
    let mut parts = token.split('-');
    let well_formed = parts.next() == Some("S")
        && parts.next() == Some("1")
        && token.split('-').count() >= 3
        && parts.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if well_formed && token.len() <= 184 {
        Ok(token.to_owned())
    } else {
        Err(format!("unrecognised account `{token}`"))
    }
}

fn mask(token: &str) -> Result<u32, String> {
    if let Some(hex) = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).map_err(|_| format!("unrecognised rights `{token}`"));
    }
    if token.is_empty() || !token.len().is_multiple_of(2) || !token.is_ascii() {
        return Err(format!("unrecognised rights `{token}`"));
    }
    let mut bits = 0;
    for index in (0..token.len()).step_by(2) {
        let pair = &token[index..index + 2];
        let (_, value) = RIGHTS
            .iter()
            .find(|(name, _)| *name == pair)
            .ok_or_else(|| format!("unrecognised rights `{token}`"))?;
        bits |= value;
    }
    Ok(bits)
}

fn label_policy(token: &str) -> Result<u32, String> {
    let mut bits = 0;
    if !token.len().is_multiple_of(2) || !token.is_ascii() {
        return Err(format!("unrecognised label policy `{token}`"));
    }
    for index in (0..token.len()).step_by(2) {
        bits |= match &token[index..index + 2] {
            "NW" => 0x1,
            "NR" => 0x2,
            "NX" => 0x4,
            _ => return Err(format!("unrecognised label policy `{token}`")),
        };
    }
    Ok(bits)
}

fn ace(text: &str, in_sacl: bool) -> Result<Option<Ace>, String> {
    let fields: Vec<&str> = text.split(';').collect();
    let &[kind, flags, rights, object, inherited_object, trustee] = fields.as_slice() else {
        return Err(format!("unrecognised access entry `({text})`"));
    };
    let kind = match (kind, in_sacl) {
        ("A", false) => AceKind::Allow,
        ("D", false) => AceKind::Deny,
        ("ML", true) => AceKind::Label,
        // Audit and alarm entries record access; they grant nothing.
        ("AU" | "AL", true) => return Ok(None),
        _ => return Err(format!("unrecognised access entry `({text})`")),
    };
    if !object.is_empty() || !inherited_object.is_empty() {
        return Err(format!("unrecognised access entry `({text})`"));
    }
    let mut inherit_only = false;
    if !flags.len().is_multiple_of(2) || !flags.is_ascii() {
        return Err(format!("unrecognised access entry `({text})`"));
    }
    for index in (0..flags.len()).step_by(2) {
        match &flags[index..index + 2] {
            "IO" => inherit_only = true,
            "OI" | "CI" | "NP" | "ID" | "SA" | "FA" => {}
            _ => return Err(format!("unrecognised access entry `({text})`")),
        }
    }
    let mask = if kind == AceKind::Label {
        label_policy(rights)?
    } else {
        mask(rights)?
    };
    Ok(Some(Ace {
        kind,
        inherit_only,
        mask,
        sid: account(trustee)?,
    }))
}

/// An ACL body without its leading list flags (`P`, `AI`, `AR` in any
/// combination Windows writes).
fn unflagged(body: &str) -> Result<&str, String> {
    let mut rest = body;
    loop {
        match ["PAI", "PAR", "AI", "AR", "P"]
            .iter()
            .find_map(|flag| rest.strip_prefix(flag))
        {
            Some(after) => rest = after,
            None if rest.is_empty() || rest.starts_with('(') || rest == NULL_LIST => {
                return Ok(rest)
            }
            None => return Err(format!("unrecognised list flags `{body}`")),
        }
    }
}

/// A NULL list, after any flags. As a DACL it lets every account do
/// everything; as a SACL it carries no mandatory label (stock Windows 11
/// writes `S:AINO_ACCESS_CONTROL` for `System32\WindowsPowerShell\v1.0`).
const NULL_LIST: &str = "NO_ACCESS_CONTROL";

/// The entries of one ACL body: optional flags, then parenthesised entries.
fn acl(body: &str, in_sacl: bool) -> Result<Vec<Ace>, String> {
    let mut rest = unflagged(body)?;
    let mut entries = Vec::new();
    while let Some(after) = rest.strip_prefix('(') {
        let close = after
            .find(')')
            .ok_or_else(|| "an access entry is not closed".to_owned())?;
        if let Some(entry) = ace(&after[..close], in_sacl)? {
            entries.push(entry);
        }
        rest = &after[close + 1..];
    }
    if rest.is_empty() {
        Ok(entries)
    } else {
        Err(format!(
            "unrecognised text after the access entries `{rest}`"
        ))
    }
}

/// Parse SDDL strictly. Components may be absent but never repeated or out
/// of order; anything this parser does not know refuses.
pub(super) fn parse(sddl: &str) -> Result<Descriptor, String> {
    let mut descriptor = Descriptor {
        owner: None,
        dacl: None,
        has_dacl: false,
        labels: Vec::new(),
    };
    let mut rest = sddl.trim_end_matches(['\r', '\n']);
    let mut order = 0;
    while !rest.is_empty() {
        let (tag, body) = rest.split_at(rest.len().min(2));
        let position = ["O:", "G:", "D:", "S:"]
            .iter()
            .position(|known| *known == tag)
            .ok_or_else(|| format!("unrecognised descriptor text `{rest}`"))?;
        if position < order {
            return Err(format!(
                "descriptor part `{tag}` is repeated or out of order"
            ));
        }
        order = position + 1;
        // A part ends where the next part's tag begins outside an entry.
        let mut depth = 0usize;
        let mut end = body.len();
        for (index, character) in body.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ if depth == 0
                    && ["G:", "D:", "S:"]
                        .iter()
                        .any(|next| body[index..].starts_with(next))
                    && index > 0 =>
                {
                    end = index;
                    break;
                }
                _ => {}
            }
        }
        let (value, after) = body.split_at(end);
        match tag {
            "O:" => descriptor.owner = Some(account(value)?),
            "G:" => {
                account(value)?;
            }
            "D:" => {
                descriptor.has_dacl = true;
                descriptor.dacl = if unflagged(value)? == NULL_LIST {
                    None
                } else {
                    Some(acl(value, false)?)
                };
            }
            _ if unflagged(value)? == NULL_LIST => descriptor.labels = Vec::new(),
            _ => descriptor.labels = acl(value, true)?,
        }
        rest = after;
    }
    Ok(descriptor)
}

impl Descriptor {
    /// The owner from one reading and the DACL and label from another: the
    /// reader takes them from different tools.
    pub(super) fn with_owner_of(mut self, other: &Descriptor) -> Result<Self, String> {
        self.owner = Some(other.owner.clone().ok_or("the owner was not reported")?);
        Ok(self)
    }
}

/// Well-known names for the accounts a message is most likely to name.
fn spelled(sid: &str) -> String {
    let name = match sid {
        "S-1-1-0" => "Everyone",
        "S-1-5-32-545" => "Users",
        "S-1-5-11" => "Authenticated Users",
        "S-1-5-4" => "INTERACTIVE",
        "S-1-5-32-546" => "Guests",
        "S-1-5-7" => "ANONYMOUS LOGON",
        "S-1-15-2-1" => "ALL APPLICATION PACKAGES",
        _ => return sid.to_owned(),
    };
    format!("{name} ({sid})")
}

/// Judge one object. `shown` is the path as messages show it. A system
/// program that fails its rule is not the user's to repair, so it reads as
/// "could not verify", never as advice to re-permission Windows.
pub(super) fn judge(rule: &Rule, shown: &str, descriptor: &Descriptor) -> Result<(), Refusal> {
    match (rule, findings(rule, shown, descriptor)) {
        (Rule::System, Err(Refusal::Unsafe(_))) => Err(Refusal::Unverifiable(format!(
            "{shown}, which this check runs, is not protected the way Windows ships it"
        ))),
        (_, verdict) => verdict,
    }
}

fn findings(rule: &Rule, shown: &str, descriptor: &Descriptor) -> Result<(), Refusal> {
    let quoted = shown.replace('\'', "''");
    let owner = descriptor
        .owner
        .as_deref()
        .ok_or_else(|| Refusal::Unverifiable(format!("the owner of {shown} was not reported")))?;
    if !rule.trusted(owner) {
        return Err(Refusal::Unsafe(format!(
            "{shown} is owned by {}, so that account can change its permissions; only you, SYSTEM or Administrators may own Grapt's installation. Take ownership, for example in PowerShell: takeown /f '{quoted}' , then retry",
            spelled(owner)
        )));
    }
    if !descriptor.has_dacl {
        return Err(Refusal::Unverifiable(format!(
            "no access list was reported for {shown}"
        )));
    }
    let Some(entries) = &descriptor.dacl else {
        return Err(Refusal::Unsafe(format!(
            "{shown} has no access list, so every account can write to it. Restore inherited permissions, for example in PowerShell: icacls '{quoted}' /reset , then retry"
        )));
    };
    // Inherit-only entries are what children receive, so they count too.
    for entry in entries.iter().filter(|entry| entry.kind == AceKind::Allow) {
        let placeholder = entry.sid == CREATOR_OWNER && entry.inherit_only;
        if entry.mask & WRITE_ISH != 0 && !rule.trusted(&entry.sid) && !placeholder {
            return Err(Refusal::Unsafe(format!(
                "{shown} grants {} write access (0x{:08x}); another account could replace Grapt's programs. Remove that access, for example in PowerShell: icacls '{quoted}' /remove:g '*{}' , then retry",
                spelled(&entry.sid),
                entry.mask & WRITE_ISH,
                entry.sid
            )));
        }
    }
    for label in &descriptor.labels {
        let level = label
            .sid
            .strip_prefix("S-1-16-")
            .and_then(|rid| rid.parse::<u32>().ok())
            .ok_or_else(|| {
                Refusal::Unverifiable(format!("unrecognised integrity label on {shown}"))
            })?;
        if level < MEDIUM || label.mask & 0x1 == 0 {
            return Err(Refusal::Unsafe(format!(
                "{shown} carries an integrity label that lets lower-integrity programs write to it. Restore it, for example in PowerShell: icacls '{quoted}' /setintegritylevel medium , then retry"
            )));
        }
    }
    Ok(())
}

/// The message of the first error in PowerShell's CLIXML error stream, on
/// one line. PowerShell wraps a message at the console width and splits it
/// across `<S S="Error">` elements, then adds its position and category
/// lines; the message is everything before `At line:`.
pub(super) fn powershell_errors(clixml: &str) -> String {
    const OPEN: &str = "<S S=\"Error\">";
    let mut text = String::new();
    let mut rest = clixml;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find("</S>") else {
            break;
        };
        text.push_str(&after[..end]);
        rest = &after[end + "</S>".len()..];
    }
    let text = text
        .replace("_x000D__x000A_", "\n")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    let message = text.split("\nAt line:").next().unwrap_or_default();
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// PowerShell's `-EncodedCommand` argument: base64 of the UTF-16LE script.
/// Paths travel inside the script, so no argument quoting is involved.
pub(super) fn encoded_command(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(char::from(
                    ALPHABET[(value >> (18 - 6 * index)) as usize & 63],
                ));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

#[cfg(test)]
#[path = "acl/tests.rs"]
mod tests;
