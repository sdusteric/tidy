//! Who signed a program, and whether Apple notarized it, through Security.framework's static
//! code API: `SecStaticCodeCreateWithPath`, `SecStaticCodeCheckValidity` (against no
//! requirement, `anchor apple` and `notarized`) and `SecCodeCopySigningInformation` (the team
//! id, the flags that mark an ad-hoc signature, and the leaf certificate for the team name).
//!
//! Read-only and offline: nothing runs the program, resources are not hashed
//! (`kSecCSDoNotValidateResources`) and no check may go online (`kSecCSNoNetworkAccess`), so a
//! check takes milliseconds. Notarization is asked only for programs with a team signature;
//! Apple's own programs and unsigned ones do not need it. Offline, a notarization ticket is
//! found only when it is stapled or macOS has it cached, so a team-signed program whose ticket
//! is not found counts as "could not check" (None), not as "not notarized".

use std::path::Path;

use super::cf::{
    CFDictionaryRef, CFStringRef, CFTypeRef, Owned, array_items, cf_file_url, cf_string, dict_get,
    number_value, string_value,
};
use crate::model::Signer;

type OSStatus = i32;
type SecCSFlags = u32;
type SecStaticCodeRef = CFTypeRef;
type SecRequirementRef = CFTypeRef;

/// `kSecCSDefaultFlags`.
const DEFAULT_FLAGS: SecCSFlags = 0;
/// `kSecCSSigningInformation`: include the certificates in the signing information.
const SIGNING_INFORMATION: SecCSFlags = 1 << 1;
/// `kSecCSDoNotValidateResources`: check the executable and its signature, not every file in
/// the bundle.
const DO_NOT_VALIDATE_RESOURCES: SecCSFlags = 1 << 2;
/// `kSecCSNoNetworkAccess`: never fetch a ticket or a revocation list.
const NO_NETWORK_ACCESS: SecCSFlags = 1 << 29;
/// The flags of every validity check.
const CHECK_FLAGS: SecCSFlags = DO_NOT_VALIDATE_RESOURCES | NO_NETWORK_ACCESS;
/// `errSecCSUnsigned`.
const ERR_UNSIGNED: OSStatus = -67062;
/// `errSecCSReqFailed`: the code does not satisfy the requirement.
const ERR_REQ_FAILED: OSStatus = -67050;
/// `kSecCodeSignatureAdhoc` in the `flags` of the signing information.
const FLAG_ADHOC: i64 = 0x0002;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecStaticCodeCreateWithPath(
        path: CFTypeRef,
        flags: SecCSFlags,
        code: *mut SecStaticCodeRef,
    ) -> OSStatus;
    fn SecStaticCodeCheckValidity(
        code: SecStaticCodeRef,
        flags: SecCSFlags,
        requirement: SecRequirementRef,
    ) -> OSStatus;
    fn SecRequirementCreateWithString(
        text: CFStringRef,
        flags: SecCSFlags,
        requirement: *mut SecRequirementRef,
    ) -> OSStatus;
    fn SecCodeCopySigningInformation(
        code: SecStaticCodeRef,
        flags: SecCSFlags,
        information: *mut CFDictionaryRef,
    ) -> OSStatus;
    fn SecCertificateCopySubjectSummary(certificate: CFTypeRef) -> CFStringRef;

    static kSecCodeInfoTeamIdentifier: CFStringRef;
    static kSecCodeInfoFlags: CFStringRef;
    static kSecCodeInfoCertificates: CFStringRef;
}

/// What a signature check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub signer: Signer,
    /// Some(true) when a notarization ticket was found (stapled or cached by macOS),
    /// Some(false) when it cannot be notarized (unsigned, ad hoc or a signature that does not
    /// check out), None when it does not apply (Apple's own programs) or no ticket was found
    /// offline.
    pub notarized: Option<bool>,
    /// The team id of a team signature (`EQHXZ8M8AV`), for matching an app's shared
    /// containers. None for Apple's own, unsigned and ad-hoc programs.
    pub team_id: Option<String>,
}

impl Signature {
    /// A program that is missing or could not be checked.
    pub fn unknown() -> Signature {
        Signature {
            signer: Signer::Unknown,
            notarized: None,
            team_id: None,
        }
    }
}

/// The signature checker the Security run uses. The production one is [`CodeSign`]; a test
/// can pass a fake that answers by path.
pub trait Signatures: Send + Sync {
    fn check(&self, program: &Path) -> Signature;
}

/// Checks signatures with Security.framework.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodeSign;

impl Signatures for CodeSign {
    fn check(&self, program: &Path) -> Signature {
        check(program)
    }
}

/// Checks the signature of the program or bundle at `path`. A path that does not exist, or
/// that Security cannot open, is [`Signer::Unknown`].
pub fn check(path: &Path) -> Signature {
    if !path.is_absolute() || std::fs::symlink_metadata(path).is_err() {
        return Signature::unknown();
    }
    let url = cf_file_url(path);
    if url.is_null() {
        return Signature::unknown();
    }
    let mut code: SecStaticCodeRef = std::ptr::null();
    // SAFETY: `url` is a valid CFURL; the out-parameter points at a local.
    let status = unsafe { SecStaticCodeCreateWithPath(url.0, DEFAULT_FLAGS, &mut code) };
    let code = Owned(code);
    if status != 0 || code.is_null() {
        return Signature::unknown();
    }
    let flags = CHECK_FLAGS;
    // SAFETY: `code` is a valid static code object owned above; a null requirement checks
    // only that the signature is intact.
    let valid = unsafe { SecStaticCodeCheckValidity(code.0, flags, std::ptr::null()) };
    if valid == ERR_UNSIGNED {
        return Signature {
            signer: Signer::Unsigned,
            notarized: Some(false),
            team_id: None,
        };
    }
    let info = signing_info(&code);
    if info.adhoc {
        return Signature {
            signer: Signer::Unsigned,
            notarized: Some(false),
            team_id: None,
        };
    }
    if valid == 0 && satisfies(&code, "anchor apple") == Some(true) {
        return Signature {
            signer: Signer::Apple,
            notarized: None,
            team_id: None,
        };
    }
    let team_id = info.team_id.clone();
    let signer = match (info.team_name, info.team_id) {
        (Some(name), _) => Signer::Team(name),
        (None, Some(id)) => Signer::Team(id),
        (None, None) if valid == 0 => Signer::Unknown,
        (None, None) => Signer::Unsigned,
    };
    let notarized = if valid != 0 {
        // A signature that does not check out cannot count as notarized.
        Some(false)
    } else {
        // Offline, a missing ticket does not prove the program is not notarized.
        satisfies(&code, "notarized").filter(|n| *n)
    };
    Signature {
        signer,
        notarized,
        team_id,
    }
}

/// Whether `code` satisfies the requirement `text`. None when the requirement cannot be built
/// or the check fails for another reason than "does not satisfy".
fn satisfies(code: &Owned, text: &str) -> Option<bool> {
    let text = cf_string(text);
    if text.is_null() {
        return None;
    }
    let mut req: SecRequirementRef = std::ptr::null();
    // SAFETY: `text` is a valid CFString; the out-parameter points at a local.
    let status = unsafe { SecRequirementCreateWithString(text.0, DEFAULT_FLAGS, &mut req) };
    let req = Owned(req);
    if status != 0 || req.is_null() {
        return None;
    }
    // SAFETY: `code` and `req` are valid objects owned by the caller and above.
    let status = unsafe { SecStaticCodeCheckValidity(code.0, CHECK_FLAGS, req.0) };
    match status {
        0 => Some(true),
        ERR_REQ_FAILED => Some(false),
        _ => None,
    }
}

/// The parts of the signing information the check uses.
#[derive(Debug, Default)]
struct Info {
    team_id: Option<String>,
    team_name: Option<String>,
    adhoc: bool,
}

fn signing_info(code: &Owned) -> Info {
    let mut dict: CFDictionaryRef = std::ptr::null();
    // SAFETY: `code` is a valid static code object; the out-parameter points at a local.
    let status = unsafe { SecCodeCopySigningInformation(code.0, SIGNING_INFORMATION, &mut dict) };
    let dict = Owned(dict);
    if status != 0 || dict.is_null() {
        return Info::default();
    }
    // SAFETY: `dict` is the dictionary the call created; the keys are Security's constant
    // CFStrings. The values read are owned by `dict`, which outlives this block.
    unsafe {
        let team_id = string_value(dict_get(dict.0, kSecCodeInfoTeamIdentifier));
        let flags = number_value(dict_get(dict.0, kSecCodeInfoFlags)).unwrap_or(0);
        let certs = array_items(dict_get(dict.0, kSecCodeInfoCertificates));
        let team_name = certs.first().and_then(|leaf| {
            let summary = Owned(SecCertificateCopySubjectSummary(*leaf));
            string_value(summary.0).and_then(|s| team_name_from_summary(&s))
        });
        Info {
            team_id,
            team_name,
            adhoc: flags & FLAG_ADHOC != 0,
        }
    }
}

/// The developer name in a leaf certificate's summary:
/// `Developer ID Application: Google LLC (EQHXZ8M8AV)` gives `Google LLC`. None for a
/// summary without a name part.
pub fn team_name_from_summary(summary: &str) -> Option<String> {
    let rest = summary
        .split_once(": ")
        .map(|(_, r)| r)
        .unwrap_or(summary)
        .trim();
    let name = match rest.rfind(" (") {
        Some(i) if rest.ends_with(')') => &rest[..i],
        _ => rest,
    };
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_names_come_from_the_leaf_summary() {
        assert_eq!(
            team_name_from_summary("Developer ID Application: Google LLC (EQHXZ8M8AV)").as_deref(),
            Some("Google LLC")
        );
        assert_eq!(
            team_name_from_summary("Apple Development: Eric Lee (ABCDE12345)").as_deref(),
            Some("Eric Lee")
        );
        assert_eq!(
            team_name_from_summary("Some Corp").as_deref(),
            Some("Some Corp")
        );
        assert_eq!(team_name_from_summary("Prefix: "), None);
    }

    #[test]
    fn missing_and_relative_paths_are_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(check(&tmp.path().join("missing")), Signature::unknown());
        assert_eq!(check(Path::new("ls")), Signature::unknown());
    }

    #[test]
    fn a_plain_text_file_is_not_signed() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("script.txt");
        std::fs::write(&f, "hello\n").unwrap();
        let sig = check(&f);
        assert!(
            matches!(sig.signer, Signer::Unsigned | Signer::Unknown),
            "{sig:?}"
        );
        assert_ne!(sig.notarized, Some(true));
    }
}
