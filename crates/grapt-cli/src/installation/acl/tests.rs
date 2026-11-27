//! Rows for the installation ACL judge. The fixtures are the SDDL
//! measured on the Windows 11 host (N7, /home/example/evidence/install-acl/n7).

use super::*;

const USER: &str = "S-1-5-21-610173083-3917101583-3076177489-1001";
const OTHER: &str = "S-1-5-21-610173083-3917101583-3076177489-1005";
const SHOWN: &str = r"C:\Users\example\AppData\Local\grapt\versions";

/// `Get-Acl .Sddl` of a fresh directory under LOCALAPPDATA (owner part).
const PREFIX_OWNER: &str = "O:S-1-5-21-610173083-3917101583-3076177489-1001G:S-1-5-21-610173083-3917101583-3076177489-513D:(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;S-1-5-21-610173083-3917101583-3076177489-1001)";
/// `icacls /save` of the same directory: DACL, no label part.
const PREFIX_ACL: &str = "D:(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;S-1-5-21-610173083-3917101583-3076177489-1001)";
/// The same after `icacls /setintegritylevel low`.
const PREFIX_ACL_LOW: &str = "D:AI(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;S-1-5-21-610173083-3917101583-3076177489-1001)S:AI(ML;OICI;NW;;;LW)";
/// System32 and powershell.exe as Windows ships them.
const SYSTEM32: &str = "O:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464G:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464D:PAI(A;OICIIO;GA;;;CO)(A;OICIIO;GA;;;SY)(A;;0x1301bf;;;SY)(A;OICIIO;GA;;;BA)(A;;0x1301bf;;;BA)(A;OICIIO;GXGR;;;BU)(A;;0x1200a9;;;BU)(A;CIIO;GA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;;0x1200a9;;;AC)(A;OICIIO;GXGR;;;AC)(A;;0x1200a9;;;S-1-15-2-2)(A;OICIIO;GXGR;;;S-1-15-2-2)";
const POWERSHELL_ACL: &str = "D:P(A;;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;;0x1200a9;;;BA)(A;;0x1200a9;;;SY)(A;;0x1200a9;;;BU)(A;;0x1200a9;;;AC)(A;;0x1200a9;;;S-1-15-2-2)S:";

fn installation() -> Rule {
    Rule::Installation {
        user: USER.to_owned(),
    }
}

/// The installation reader's combination: DACL and label from `icacls`,
/// owner from PowerShell.
fn read(acl: &str, owner: &str) -> Descriptor {
    let combined =
        parse(acl).and_then(|acl| parse(owner).and_then(|owner| acl.with_owner_of(&owner)));
    match combined {
        Ok(descriptor) => descriptor,
        Err(reason) => panic!("the fixture must parse: {reason}"),
    }
}

fn with(extra: &str) -> Descriptor {
    read(&format!("{PREFIX_ACL}{extra}"), PREFIX_OWNER)
}

fn unsafe_message(verdict: Result<(), Refusal>) -> String {
    match verdict {
        Err(Refusal::Unsafe(message)) => message,
        other => panic!("expected an unsafe refusal, got {other:?}"),
    }
}

#[test]
fn u1_a_fresh_localappdata_directory_is_accepted() {
    assert_eq!(
        judge(&installation(), SHOWN, &read(PREFIX_ACL, PREFIX_OWNER)),
        Ok(())
    );
}

#[test]
fn u2_a_world_writable_entry_is_refused_and_named() {
    let message = unsafe_message(judge(&installation(), SHOWN, &with("(A;OICI;FW;;;WD)")));
    assert!(
        message.contains("grants Everyone (S-1-1-0) write access"),
        "{message}"
    );
    assert!(
        message.contains(
            r"icacls 'C:\Users\example\AppData\Local\grapt\versions' /remove:g '*S-1-1-0'"
        ),
        "{message}"
    );
}

#[test]
fn u3_an_inherit_only_grant_counts_because_children_receive_it() {
    let message = unsafe_message(judge(&installation(), SHOWN, &with("(A;OICIIO;GA;;;BU)")));
    assert!(message.contains("Users (S-1-5-32-545)"), "{message}");
}

#[test]
fn u4_read_and_execute_for_other_accounts_is_harmless() {
    for extra in ["(A;;FR;;;AU)", "(A;;0x1200a9;;;BU)", "(A;OICIIO;GXGR;;;WD)"] {
        assert_eq!(
            judge(&installation(), SHOWN, &with(extra)),
            Ok(()),
            "{extra}"
        );
    }
}

#[test]
fn u5_another_users_numeric_add_subdirectory_is_refused() {
    let extra = format!("(A;;0x00000004;;;{OTHER})");
    let message = unsafe_message(judge(&installation(), SHOWN, &with(&extra)));
    assert!(message.contains(OTHER), "{message}");
}

#[test]
fn u5b_delete_child_alone_is_write_access() {
    unsafe_message(judge(&installation(), SHOWN, &with("(A;;DT;;;BU)")));
    unsafe_message(judge(&installation(), SHOWN, &with("(A;;0x40;;;IU)")));
}

#[test]
fn u6_a_deny_entry_never_makes_an_object_unsafe() {
    assert_eq!(judge(&installation(), SHOWN, &with("(D;;FA;;;WD)")), Ok(()));
}

#[test]
fn u7_only_the_user_system_or_administrators_may_own_it() {
    let foreign = PREFIX_OWNER.replacen(USER, OTHER, 1);
    let message = unsafe_message(judge(&installation(), SHOWN, &read(PREFIX_ACL, &foreign)));
    assert!(message.contains("is owned by"), "{message}");
    assert!(message.contains("takeown /f"), "{message}");
    for owner in ["O:BAG:SYD:", "O:SYG:SYD:"] {
        assert_eq!(
            judge(&installation(), SHOWN, &read(PREFIX_ACL, owner)),
            Ok(()),
            "{owner}"
        );
    }
}

#[test]
fn u8_a_null_or_missing_access_list_is_refused() {
    let message = unsafe_message(judge(
        &installation(),
        SHOWN,
        &read("D:NO_ACCESS_CONTROL", PREFIX_OWNER),
    ));
    assert!(message.contains("/reset"), "{message}");
    assert!(matches!(
        judge(&installation(), SHOWN, &read("S:", PREFIX_OWNER)),
        Err(Refusal::Unverifiable(_))
    ));
}

#[test]
fn u9_a_label_below_medium_or_without_no_write_up_is_refused() {
    let message = unsafe_message(judge(
        &installation(),
        SHOWN,
        &read(PREFIX_ACL_LOW, PREFIX_OWNER),
    ));
    assert!(message.contains("/setintegritylevel medium"), "{message}");
    let medium = format!("{PREFIX_ACL}S:(ML;OICI;NW;;;ME)");
    assert_eq!(
        judge(&installation(), SHOWN, &read(&medium, PREFIX_OWNER)),
        Ok(())
    );
    let without_no_write_up = format!("{PREFIX_ACL}S:(ML;OICI;NR;;;ME)");
    unsafe_message(judge(
        &installation(),
        SHOWN,
        &read(&without_no_write_up, PREFIX_OWNER),
    ));
    // An empty label part, as icacls writes for powershell.exe, is the default.
    let empty = format!("{PREFIX_ACL}S:");
    assert_eq!(
        judge(&installation(), SHOWN, &read(&empty, PREFIX_OWNER)),
        Ok(())
    );
}

#[test]
fn u10_unrecognised_entries_refuse_rather_than_pass() {
    for text in [
        "D:(XA;;FA;;;WD;(@User.x == 1))",
        "D:(OA;;FA;bf967aba-0de6-11d0-a285-00aa003049e2;;WD)",
        "D:(A;;ZZ;;;BU)",
        "D:(A;;FA;;;QQ)",
        "D:(A;XX;FA;;;BU)",
        "D:(A;;FA;;;BU",
        "D:(A;;FA;;;BU)junk",
        "D:XYZ(A;;FA;;;BU)",
        "Q:BA",
        "D:(A;;FA;;;BU)O:BA",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}

#[test]
fn u11_creator_owner_inherit_only_is_a_placeholder_not_an_account() {
    assert_eq!(
        judge(&installation(), SHOWN, &with("(A;OICIIO;GA;;;CO)")),
        Ok(())
    );
    unsafe_message(judge(&installation(), SHOWN, &with("(A;OICI;GA;;;CO)")));
}

#[test]
fn u12_audit_entries_grant_nothing() {
    let audited = format!("{PREFIX_ACL}S:(AU;SA;FA;;;WD)");
    assert_eq!(
        judge(&installation(), SHOWN, &read(&audited, PREFIX_OWNER)),
        Ok(())
    );
}

#[test]
fn u13_windows_ships_system32_and_powershell_inside_the_system_rule() {
    let system32 = match parse(SYSTEM32) {
        Ok(descriptor) => descriptor,
        Err(reason) => panic!("System32 parses: {reason}"),
    };
    assert_eq!(
        judge(&Rule::System, r"C:\WINDOWS\System32", &system32),
        Ok(())
    );
    assert_eq!(
        judge(
            &Rule::System,
            r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe",
            &read(POWERSHELL_ACL, SYSTEM32)
        ),
        Ok(())
    );
    // The installation rule would refuse it: TrustedInstaller is not the user.
    assert!(judge(&installation(), "x", &system32).is_err());
}

#[test]
fn u14_a_loosened_system_program_cannot_be_verified_not_repaired() {
    let loosened = read(&POWERSHELL_ACL.replace("S:", "(A;;FW;;;BU)S:"), SYSTEM32);
    match judge(&Rule::System, "powershell.exe", &loosened) {
        Err(Refusal::Unverifiable(reason)) => {
            assert!(
                reason.contains("is not protected the way Windows ships it"),
                "{reason}"
            );
        }
        other => panic!("expected unverifiable, got {other:?}"),
    }
    let foreign = read(
        POWERSHELL_ACL,
        &SYSTEM32.replacen(TRUSTED_INSTALLER, USER, 1),
    );
    assert!(matches!(
        judge(&Rule::System, "powershell.exe", &foreign),
        Err(Refusal::Unverifiable(_))
    ));
}

#[test]
fn the_two_refusals_read_differently() {
    assert_eq!(
        Refusal::Unverifiable("PowerShell language mode ConstrainedLanguage".to_owned()).message(),
        "could not verify install permissions (PowerShell language mode ConstrainedLanguage); installation remains unchanged"
    );
    assert_eq!(
        Refusal::Unsafe("x grants y write access".to_owned()).message(),
        "x grants y write access; installation remains unchanged"
    );
}

#[test]
fn the_encoded_command_is_base64_of_utf16le() {
    // [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("a'Ω"))
    assert_eq!(encoded_command("a'Ω"), "YQAnAKkD");
    assert_eq!(encoded_command("ab"), "YQBiAA==");
    assert_eq!(encoded_command(""), "");
}

/// The exact `icacls /save` lines captured on stock Windows 11
/// (/home/example/evidence/install-acl-native-r1/icacls-system-dirs.txt).
/// 13fb56e0 refused the v1.0 line: synthetic fixtures had never carried a
/// flagged NULL SACL.
const NATIVE_SYSTEM32: &str = "D:PAI(A;;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;CIIO;GA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;;0x1301bf;;;SY)(A;OICIIO;GA;;;SY)(A;;0x1301bf;;;BA)(A;OICIIO;GA;;;BA)(A;;0x1200a9;;;BU)(A;OICIIO;GXGR;;;BU)(A;OICIIO;GA;;;CO)(A;;0x1200a9;;;AC)(A;OICIIO;GXGR;;;AC)(A;;0x1200a9;;;S-1-15-2-2)(A;OICIIO;GXGR;;;S-1-15-2-2)";
const NATIVE_V1: &str = "D:PAI(A;;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;CIIO;GA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;;0x1301bf;;;SY)(A;OICIIO;GA;;;SY)(A;;0x1301bf;;;BA)(A;OICIIO;GA;;;BA)(A;;0x1200a9;;;BU)(A;OICIIO;GXGR;;;BU)(A;OICIIO;GA;;;CO)(A;;0x1200a9;;;AC)(A;OICIIO;GXGR;;;AC)(A;;0x1200a9;;;S-1-15-2-2)(A;OICIIO;GXGR;;;S-1-15-2-2)S:AINO_ACCESS_CONTROL";
const NATIVE_POWERSHELL: &str = "D:P(A;;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;;0x1200a9;;;BA)(A;;0x1200a9;;;SY)(A;;0x1200a9;;;BU)(A;;0x1200a9;;;AC)(A;;0x1200a9;;;S-1-15-2-2)S:";
const NATIVE_LOCALAPPDATA: &str = "D:(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;S-1-5-21-610173083-3917101583-3076177489-1001)";

#[test]
fn native_icacls_lines_of_all_four_objects_judge_as_windows_ships_them() {
    for (shown, line) in [
        (r"C:\WINDOWS\System32", NATIVE_SYSTEM32),
        (r"C:\WINDOWS\System32\WindowsPowerShell\v1.0", NATIVE_V1),
        (
            r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe",
            NATIVE_POWERSHELL,
        ),
    ] {
        assert_eq!(
            judge(&Rule::System, shown, &read(line, SYSTEM32)),
            Ok(()),
            "{shown}"
        );
    }
    assert_eq!(
        judge(
            &installation(),
            r"C:\Users\example\AppData\Local",
            &read(NATIVE_LOCALAPPDATA, PREFIX_OWNER)
        ),
        Ok(())
    );
}

#[test]
fn a_null_sacl_in_any_flag_form_carries_no_label() {
    for sacl in [
        "S:NO_ACCESS_CONTROL",
        "S:PNO_ACCESS_CONTROL",
        "S:AINO_ACCESS_CONTROL",
        "S:PAINO_ACCESS_CONTROL",
        "S:ARNO_ACCESS_CONTROL",
        "S:",
        "",
    ] {
        let line = format!("{PREFIX_ACL}{sacl}");
        assert_eq!(
            judge(&installation(), SHOWN, &read(&line, PREFIX_OWNER)),
            Ok(()),
            "{sacl}"
        );
    }
}

#[test]
fn a_null_dacl_in_any_flag_form_stays_refused() {
    for dacl in [
        "D:NO_ACCESS_CONTROL",
        "D:PNO_ACCESS_CONTROL",
        "D:AINO_ACCESS_CONTROL",
        "D:PAINO_ACCESS_CONTROL",
    ] {
        let message = unsafe_message(judge(&installation(), SHOWN, &read(dacl, PREFIX_OWNER)));
        assert!(message.contains("has no access list"), "{dacl}: {message}");
    }
    assert!(parse("D:XNO_ACCESS_CONTROL").is_err());
    assert!(parse("S:NO_ACCESS_CONTROLX").is_err());
}

/// The child stderr captured when grapt.exe ran from PowerShell 7
/// (/home/example/evidence/install-acl-target-file-r1/err-pwsh.txt).
const PWSH_PARENT_STDERR: &str = concat!(
    "#< CLIXML\r\n<Objs Version=\"1.1.0.1\" xmlns=\"http://schemas.microsoft.com/powershell/2004/04\">",
    "<Obj S=\"progress\" RefId=\"0\"><TN RefId=\"0\"><T>System.Management.Automation.PSCustomObject</T><T>System.Object</T></TN>",
    "<MS><I64 N=\"SourceId\">1</I64><PR N=\"Record\"><AV>Preparing modules for first use.</AV><AI>0</AI><Nil /><PI>-1</PI><PC>-1</PC><T>Completed</T><SR>-1</SR><SD> </SD></PR></MS></Obj>",
    "<S S=\"Error\">Get-Acl : The 'Get-Acl' command was found in the module 'Microsoft.PowerShell.Security', but the module could not be _x000D__x000A_</S>",
    "<S S=\"Error\">loaded. For more information, run 'Import-Module Microsoft.PowerShell.Security'._x000D__x000A_</S>",
    "<S S=\"Error\">At line:5 char:60_x000D__x000A_</S>",
    "<S S=\"Error\">+ ... stomobject]@{ kind = 'owner'; index = $index; sddl = (Get-Acl -Litera ..._x000D__x000A_</S>",
    "<S S=\"Error\">    + FullyQualifiedErrorId : CouldNotAutoloadMatchingModule_x000D__x000A_</S>",
    "<S S=\"Error\"> _x000D__x000A_</S></Objs>",
);

#[test]
fn a_powershell_error_stream_reads_as_its_message() {
    assert_eq!(
        powershell_errors(PWSH_PARENT_STDERR),
        "Get-Acl : The 'Get-Acl' command was found in the module 'Microsoft.PowerShell.Security', but the module could not be loaded. For more information, run 'Import-Module Microsoft.PowerShell.Security'."
    );
    assert_eq!(powershell_errors("#< CLIXML\r\n<Objs></Objs>"), "");
}
