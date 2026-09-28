//! One path, every spelling Windows admits, and one decision — map line 2455,
//! specification `docs/sandbox.md`.
//!
//! `sandbox-grants.md` §1 invariant 2 is *"**`deny` beats `allow`, at every
//! specificity.** A path matched by any `deny` pattern is refused even when a
//! longer, more specific `allow` names it exactly"*, and invariant 4 is *"**A
//! request outside the grant is refused inside the program.**"* Neither says
//! "when the path is spelled the way the pattern was", and Windows spells one
//! file many ways: `\` for `/`, either case, a `\\?\` or `\\.\` prefix, an 8.3
//! short name, trailing dots and spaces Win32 discards before it opens
//! anything, and an alternate data stream after a colon. Every one of these
//! reached a real file past a rule that named it — measured on macOS with the
//! literal fixtures below, and re-run on Windows ARM64.
//!
//! **Literal strings and no filesystem, deliberately.** A Windows path is a
//! value here, so every claim is decided on every host rather than only on
//! the one that can produce the spelling. `crates/sterna/tests/sandbox_profile.rs`
//! holds the filesystem-backed halves — `a_short_name_and_its_long_form_decide_
//! identically` and `a_verbatim_and_a_plain_spelling_of_one_path_decide_
//! identically` — and this file does not repeat them.

use std::path::Path;
use sterna::sandbox::profile::{Access, PermissionDenied, Profile};

/// A project root spelled the way Windows spells one, on every host.
const ROOT: &str = "C:/sterna-fixture/proj";

/// `deny` names a subtree and a single file; `allow` names the whole
/// project, so invariant 2's "even when a longer, more specific `allow` names
/// it" is what every refusal below has to beat.
const SETTINGS: &str = r#"{"permissions":{
    "allow":["Read(C:/sterna-fixture/proj/**)"],
    "deny":["Read(C:/sterna-fixture/proj/secrets/**)","Read(C:/sterna-fixture/proj/token.env)"]
}}"#;

fn profile() -> Profile {
    let profile = Profile::compile(Path::new(ROOT), Some(SETTINGS));
    assert!(
        profile.diagnostics().is_empty(),
        "the fixture document must compile its rules rather than be discarded: {:?}",
        profile.diagnostics()
    );
    profile
}

fn refusal(profile: &Profile, spelling: &str) -> PermissionDenied {
    match profile.check("Read", Access::Read, Path::new(spelling)) {
        Err(denied) => denied,
        Ok(granted) => panic!(
            "`{spelling}` was granted as {}; it is one spelling of a path a `deny` names",
            granted.display()
        ),
    }
}

/// Invariant 2, for every spelling of one denied subtree and one denied file.
///
/// Each row is a way Windows names the same object. The trailing dot and the
/// trailing space are the ones that are not obviously spellings at all: Win32
/// strips both from a component before it opens anything, so
/// `proj\secrets.\a.txt` *is* `proj\secrets\a.txt` — and the profile compared
/// the written form and let it past.
#[test]
fn every_windows_spelling_of_a_denied_path_is_refused() {
    let profile = profile();
    let denied_subtree = "`Read(C:/sterna-fixture/proj/secrets/**)` in permissions.deny";
    for spelling in [
        r"C:\sterna-fixture\proj\secrets\a.txt",
        r"C:/sterna-fixture/proj/secrets/a.txt",
        r"C:/sterna-fixture/proj/./secrets/a.txt",
        r"C:\sterna-fixture\proj\SECRETS\a.txt",
        r"c:\sterna-fixture\proj\secrets\a.txt",
        r"C:\sterna-fixture\proj\secrets.\a.txt",
        r"C:\sterna-fixture\proj\secrets \a.txt",
        r"C:\sterna-fixture\proj\secrets...\a.txt",
        r"C:\sterna-fixture\proj\secrets. . \a.txt",
        r"\\?\C:\sterna-fixture\proj\secrets\a.txt",
        r"\\.\C:\sterna-fixture\proj\secrets\a.txt",
        // A reserved device name is still inside the denied subtree, which is
        // what decides it; nothing here claims the device itself is refused.
        r"C:\sterna-fixture\proj\secrets\NUL",
    ] {
        assert_eq!(
            refusal(&profile, spelling).rule,
            denied_subtree,
            "{spelling} must be refused by the rule that names its subtree"
        );
    }

    let denied_file = "`Read(C:/sterna-fixture/proj/token.env)` in permissions.deny";
    for spelling in [
        r"C:\sterna-fixture\proj\token.env",
        r"C:\sterna-fixture\proj\TOKEN.ENV",
        r"C:\sterna-fixture\proj\token.env.",
        r"C:\sterna-fixture\proj\token.env ",
        // An alternate data stream is the same file object under another
        // name, and reading it reads the file.
        r"C:\sterna-fixture\proj\token.env:hidden",
        r"C:\sterna-fixture\proj\token.env::$DATA",
        r"\\?\C:\sterna-fixture\proj\token.env",
        r"\\.\C:\sterna-fixture\proj\token.env",
    ] {
        assert_eq!(
            refusal(&profile, spelling).rule,
            denied_file,
            "{spelling} must be refused by the rule that names the file"
        );
    }
}

/// The falsifying half, and it is what stops the assertions above from being
/// satisfied by a profile that refuses everything.
///
/// Two of these are the specific over-refusals the repair could have bought.
/// A colon is a legal character in a Unix filename, so cutting every
/// component at its first one would have made `2026-09-09T12:00:00.log` a
/// path called `2026-09-09T12` — one no `deny` on `*.log` matches. And an
/// `allow` still decides case-sensitively (§2 and [`match_segment`]'s rule),
/// because folding it is how one spelling reaches a path its author never
/// wrote.
#[test]
fn a_path_no_rule_names_is_still_granted_in_every_spelling() {
    let profile = profile();
    for spelling in [
        r"C:\sterna-fixture\proj\notes\a.md",
        r"C:/sterna-fixture/proj/notes/a.md",
        r"\\?\C:\sterna-fixture\proj\notes\a.md",
        r"\\.\C:\sterna-fixture\proj\notes\a.md",
        // A colon-bearing name is one file, not a file and a stream, wherever
        // the host allows it.
        r"C:\sterna-fixture\proj\notes\2026-09-09T12:00:00.log",
    ] {
        assert!(
            profile
                .check("Read", Access::Read, Path::new(spelling))
                .is_ok(),
            "{spelling} is inside the project and no rule names it"
        );
    }

    // The stream cut is an extra spelling to test, never a replacement: a
    // `deny` written against the colon-bearing name still refuses it.
    let logs = Profile::compile(
        Path::new(ROOT),
        Some(r#"{"permissions":{"deny":["Read(C:/sterna-fixture/proj/notes/*.log)"]}}"#),
    );
    assert!(
        logs.check(
            "Read",
            Access::Read,
            Path::new(r"C:\sterna-fixture\proj\notes\2026-09-09T12:00:00.log"),
        )
        .is_err(),
        "cutting a component at its first colon would have hidden this file from its own deny"
    );

    // An `allow` outside the root is exact, on every host. It is a `Write`
    // grant because reading is granted everywhere nothing refuses.
    let elsewhere = Profile::compile(
        Path::new(ROOT),
        Some(r#"{"permissions":{"allow":["Write(C:/sterna-fixture/other/notes/**)"]}}"#),
    );
    assert!(
        elsewhere
            .check(
                "Write",
                Access::Write,
                Path::new(r"C:\sterna-fixture\other\notes\a.md")
            )
            .is_ok()
    );
    assert!(
        elsewhere
            .check(
                "Write",
                Access::Write,
                Path::new(r"C:\sterna-fixture\other\NOTES\a.md")
            )
            .is_err(),
        "folding an `allow` lets one spelling reach a path its author never wrote"
    );
}

/// §1 invariant 5: `.claude/**` is never writable, *"so a profile recomputed
/// from disk mid-session would let a program widen its own sandbox by editing
/// the file it was derived from"*.
///
/// The whole of §4 is reached through the same comparison, and this is the
/// case where a missed refusal is a sandbox escape rather than a leak: on a
/// case-insensitive filesystem `.CLAUDE\settings.json` **is**
/// `.claude\settings.json`, and the write was granted by the implicit root
/// grant that covers everything else in the project.
#[test]
fn the_never_grantable_set_is_reached_by_no_spelling() {
    let profile = profile();
    let dot_claude = "`.claude/**` is never writable: a program that could edit it could widen \
                      the profile it was derived from (docs/sandbox.md, invariant 5)";
    for spelling in [
        r"C:\sterna-fixture\proj\.claude\settings.json",
        r"C:\sterna-fixture\proj\.CLAUDE\settings.json",
        r"C:\sterna-fixture\proj\.Claude\settings.json",
        r"C:\sterna-fixture\proj\.claude.\settings.json",
        r"C:\sterna-fixture\proj\.claude \settings.json",
        r"\\?\C:\sterna-fixture\proj\.claude\settings.json",
        r"\\.\C:\sterna-fixture\proj\.CLAUDE\settings.json",
    ] {
        let denied = match profile.check("Write", Access::Write, Path::new(spelling)) {
            Err(denied) => denied,
            Ok(granted) => panic!(
                "`{spelling}` was granted for writing as {}; it is one spelling of `.claude`",
                granted.display()
            ),
        };
        assert_eq!(denied.rule, dot_claude, "{spelling}");
    }

    // And the reading half is untouched: §1.5 keeps `.claude` readable,
    // because `settings.json` is read before the sandbox is entered.
    assert!(
        profile
            .check(
                "Read",
                Access::Read,
                Path::new(r"C:\sterna-fixture\proj\.CLAUDE\settings.json")
            )
            .is_ok(),
        "the rule is write-only, whichever spelling asks"
    );
}

/// A Windows device-namespace path that reduces to no ordinary rooted
/// spelling is refused rather than compared — §1 invariant 4, in the
/// direction a cage has to err.
///
/// `\\?\GLOBALROOT\Device\HarddiskVolume3\…` reaches the same files as
/// `C:\…` and no lexical rule turns one into the other; `\\.\PhysicalDrive0`
/// and `\\.\C:` (with no separator after the drive, which names the volume
/// rather than its root directory) are raw block devices. Compared as
/// ordinary components they met no rule at all, and the broad `allow` this
/// fixture writes then covered them.
///
/// **The spellings here are ones no filesystem can reconcile**, and that is
/// the measurement rather than a convenience: on Windows a device path that
/// *does* resolve is canonicalized to its ordinary spelling by
/// `canonical_prefix` before any rule sees it, and then gets the ordinary
/// answer. Asserting the refusal against a resolvable one asserted the
/// weaker of the two behaviours and failed on the VM for saying so — see
/// the invariant below, which covers both.
#[test]
fn an_unresolvable_device_path_is_refused_rather_than_compared() {
    let whole_disk = whole_disk();
    let refused = "a Windows device-namespace path is refused rather than compared: it names no \
                   ordinary file this profile can place inside or outside a grant \
                   (docs/sandbox.md, invariant 4)";
    for spelling in UNRESOLVABLE_DEVICE_PATHS {
        let denied = match whole_disk.check("Read", Access::Read, Path::new(spelling)) {
            Err(denied) => denied,
            Ok(granted) => panic!("`{spelling}` was granted as {}", granted.display()),
        };
        assert_eq!(denied.rule, refused, "{spelling}");
    }
}

/// The invariant underneath the refusal, and the one that decides whether a
/// caller can be handed a volume: **`check` never returns a path still in the
/// device namespace.**
///
/// A caller must open the `PathBuf` `check` returns rather than the string it
/// passed in, so this is what stands between a grant and a raw sector read.
/// Either the filesystem reconciled the spelling to an ordinary one — in
/// which case the returned path opens a file — or nothing was granted. It
/// holds for the spellings the host can resolve and the ones it cannot, which
/// is why it is asserted separately from the sentence above.
#[test]
fn no_granted_path_is_still_in_the_device_namespace() {
    let whole_disk = whole_disk();
    let mut spellings: Vec<&str> = UNRESOLVABLE_DEVICE_PATHS.to_vec();
    spellings.extend([
        // Resolvable on a real Windows host, and not on any other. Both
        // answers are correct; being handed one of these back is not.
        r"\\?\GLOBALROOT\Device\HarddiskVolume3\Windows\System32\config\SAM",
        r"\\.\C:",
        r"\\?\C:",
        r"\\.\PhysicalDrive0",
        r"\\.\pipe\sterna",
        r"\\?\C:\sterna-fixture\proj\notes\a.md",
        r"\\.\C:\sterna-fixture\proj\notes\a.md",
    ]);
    for spelling in spellings {
        if let Ok(granted) = whole_disk.check("Read", Access::Read, Path::new(spelling)) {
            let folded = granted.to_string_lossy().replace('\\', "/");
            let unreconciled = (folded.starts_with("//?/") || folded.starts_with("//./"))
                && !reduces_to_an_ordinary_path(&folded);
            assert!(
                !unreconciled,
                "`{spelling}` was granted as {}, which is still a device path: a caller opening \
                 the path this profile decided on would open the object rather than a file",
                granted.display()
            );
        }
    }
}

/// The falsifying half: the two device spellings that *do* reduce lexically
/// still decide as the ordinary path they name, rather than being swept up by
/// the refusal above.
#[test]
fn a_device_spelling_that_reduces_decides_as_the_path_it_names() {
    let whole_disk = whole_disk();
    for spelling in [
        r"\\?\C:\sterna-fixture\proj\notes\a.md",
        r"\\.\C:\sterna-fixture\proj\notes\a.md",
        r"\\?\UNC\server\share\notes\a.md",
    ] {
        assert!(
            whole_disk
                .check("Read", Access::Read, Path::new(spelling))
                .is_ok(),
            "{spelling} reduces to an ordinary path and this grant covers it"
        );
    }

    // And the denied subtree is still denied through both device spellings,
    // which is what stops the reduction from being a way in.
    let profile = profile();
    for spelling in [
        r"\\?\C:\sterna-fixture\proj\secrets\a.txt",
        r"\\.\C:\sterna-fixture\proj\secrets\a.txt",
    ] {
        assert_eq!(
            refusal(&profile, spelling).rule,
            "`Read(C:/sterna-fixture/proj/secrets/**)` in permissions.deny",
            "{spelling}"
        );
    }
}

/// Device-namespace spellings no host can reconcile to a file: a volume
/// number, a physical drive and a named pipe that do not exist, a null volume
/// GUID, and a verbatim path with no drive at all. Every one of them stays
/// exactly as written through `canonical_prefix` on Windows and on Unix, so
/// each test below decides the same way on every host.
const UNRESOLVABLE_DEVICE_PATHS: &[&str] = &[
    r"\\?\GLOBALROOT\Device\HarddiskVolume999\sterna-fixture\no-such-file",
    r"\\.\GLOBALROOT\Device\HarddiskVolume999\sterna-fixture\no-such-file",
    r"\\?\Volume{00000000-0000-0000-0000-000000000000}\sterna-fixture\no-such-file",
    r"\\.\PhysicalDrive999",
    r"\\.\pipe\sterna-fixture-no-such-pipe",
    r"\\?\sterna-fixture\proj\notes\a.md",
];

/// A settings document that grants the whole filesystem, so nothing below is
/// refused merely for want of a grant.
fn whole_disk() -> Profile {
    Profile::compile(
        Path::new(ROOT),
        Some(r#"{"permissions":{"allow":["Read(/**)","Write(/**)"]}}"#),
    )
}

/// `folded` reduces to an ordinary rooted path, in the one form
/// `sandbox/profile.rs` reduces it: a rooted drive, or the `UNC/` marker.
/// Mirrored here rather than exported, because a test that called the
/// production function would agree with it by construction.
fn reduces_to_an_ordinary_path(folded: &str) -> bool {
    let Some(rest) = folded
        .strip_prefix("//?/")
        .or_else(|| folded.strip_prefix("//./"))
    else {
        return false;
    };
    let drive = rest.as_bytes();
    let rooted_drive =
        drive.len() > 2 && drive[0].is_ascii_alphabetic() && drive[1] == b':' && drive[2] == b'/';
    rooted_drive
        || rest
            .get(..4)
            .is_some_and(|m| m.eq_ignore_ascii_case("unc/"))
}
