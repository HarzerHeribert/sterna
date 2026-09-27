//! Sterna updates itself from GitHub releases, the way `install.sh` installed
//! it: a release's archive, verified against the release's own
//! `SHA256SUMS`, unpacked into a **fresh** `<root>/versions/<tag>`, the
//! subscription broker the release pins adopted, and `<root>/current`
//! repointed -- never a byte written over a binary that may be running. The
//! running session keeps its binary; the next start runs the new one, and
//! the person is told so once: *Sterna <tag> installed · restart to update*.
//!
//! **Only a release install updates itself.** The install is read from the
//! running binary's own path (`<root>/versions/<tag>/bin/sterna`); a binary
//! run from a build tree, or a version directory whose name is not exactly a
//! release tag (a developer's `v0.1.0-pre.1-1316-gf96f87f0`), is left alone
//! -- `sterna update` says why rather than guess what it is newer than.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use sha2::Digest as _;

/// The repository releases are published from.
pub const REPOSITORY: &str = "HarzerHeribert/sterna";
/// Set to anything to stop the background check (`sterna update` still works).
pub const DISABLE_ENV: &str = "STERNA_DISABLE_AUTOUPDATE";
/// [`DISABLE_ENV`]'s name before the rename. Retired: while it is set alone it
/// still stops the check, and each start says to rename it.
const RETIRED_DISABLE_ENV: &str = "PANE_DISABLE_AUTOUPDATE";
/// The background check asks GitHub on every terminal session's start, but
/// not twice within this: GitHub answers 60 unauthenticated requests an hour
/// per address, and several sessions opened together must not spend them.
const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);
/// The stamp the background check leaves in the install root.
const STAMP: &str = "update-check";
/// What the adopted broker's pin was, in the install root.
const BROKER_STAMP: &str = "broker-version";
const TIMEOUT: Duration = Duration::from_secs(120);

/// The notice the background check leaves for the session to show once.
static NOTICE: Mutex<Option<String>> = Mutex::new(None);

/// A release install: the root holding `versions/` and `current`, and the
/// tag of the version directory the running binary lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub root: PathBuf,
    pub tag: String,
}

impl Install {
    /// The install the running binary belongs to, or `None` when it runs
    /// from anywhere else.
    #[must_use]
    pub fn of_running() -> Option<Self> {
        let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
        Self::from_exe(&exe)
    }

    /// `<root>/versions/<tag>/bin/<sterna>` read backwards; `None` for any
    /// other shape.
    #[must_use]
    pub fn from_exe(exe: &Path) -> Option<Self> {
        let bin = exe.parent()?;
        let version = bin.parent()?;
        let versions = version.parent()?;
        if bin.file_name()? != "bin" || versions.file_name()? != "versions" {
            return None;
        }
        Some(Self {
            root: versions.parent()?.to_path_buf(),
            tag: version.file_name()?.to_str()?.to_string(),
        })
    }

    /// Whether this install's directory is exactly a release tag.
    #[must_use]
    pub fn is_release(&self) -> bool {
        parse_tag(&self.tag).is_some()
    }

    /// The tag `<root>/current` points at, when it points at a version.
    #[must_use]
    pub fn current_tag(&self) -> Option<String> {
        let target = fs::read_link(self.root.join("current")).ok()?;
        Some(target.file_name()?.to_str()?.to_string())
    }
}

/// `v<major>.<minor>.<patch>` with an optional `-pre.<n>`, and nothing else.
#[must_use]
pub fn parse_tag(tag: &str) -> Option<(u64, u64, u64, Option<u64>)> {
    let rest = tag.strip_prefix('v')?;
    let (core, pre) = match rest.split_once("-pre.") {
        Some((core, pre)) => (core, Some(pre.parse().ok()?)),
        None => (rest, None),
    };
    let mut parts = core.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts
        .next()
        .is_none()
        .then_some((version.0, version.1, version.2, pre))
}

/// Whether `candidate` is a later release than `than`. A release without a
/// pre-release number is later than every pre-release of the same version.
#[must_use]
pub fn newer(candidate: &str, than: &str) -> bool {
    let (Some(a), Some(b)) = (parse_tag(candidate), parse_tag(than)) else {
        return false;
    };
    let rank = |(major, minor, patch, pre): (u64, u64, u64, Option<u64>)| {
        (major, minor, patch, pre.map_or((1, 0), |n| (0, n)))
    };
    rank(a) > rank(b)
}

/// This build's release target, as the release archives name it.
#[must_use]
pub fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

/// Where releases are listed and downloaded from; each overridable for a
/// test, exactly as `install.sh`'s are.
#[derive(Debug, Clone)]
pub struct Source {
    pub releases_api: String,
    pub downloads: String,
    pub broker_downloads: Option<String>,
}

impl Source {
    #[must_use]
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            releases_api: var("STERNA_UPDATE_API").unwrap_or_else(|| {
                format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30")
            }),
            downloads: var("STERNA_UPDATE_DOWNLOADS")
                .unwrap_or_else(|| format!("https://github.com/{REPOSITORY}/releases/download")),
            broker_downloads: var("STERNA_UPDATE_BROKER_DOWNLOADS"),
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent("sterna-update")
        .build()
        .into()
}

fn get_text(url: &str) -> Result<String, String> {
    agent()
        .get(url)
        .header("accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let response = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let mut reader = response.into_body().into_reader();
    let mut file = fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("{url}: {e}"))?;
    file.flush().map_err(|e| format!("{}: {e}", to.display()))
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The newest release's tag, pre-releases included, chosen by version: the
/// list's own order puts `pre.9` above `pre.10`, so its first entry is not it.
pub fn latest_tag(source: &Source) -> Result<String, String> {
    let text = get_text(&source.releases_api)?;
    let releases: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("the release list did not parse: {e}"))?;
    releases
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|release| release["tag_name"].as_str())
        .filter(|tag| parse_tag(tag).is_some())
        .reduce(|best, tag| if newer(tag, best) { tag } else { best })
        .map(str::to_string)
        .ok_or_else(|| "the release list named no release".to_string())
}

/// Installs `tag` into `root` and points `current` at it; returns the
/// version directory. An already-present version is only repointed to.
pub fn install_release(
    root: &Path,
    tag: &str,
    target: &str,
    source: &Source,
) -> Result<PathBuf, String> {
    let dest = root.join("versions").join(tag);
    let exe = if cfg!(windows) {
        "sterna.exe"
    } else {
        "sterna"
    };
    if !dest.join("bin").join(exe).is_file() {
        let work = root.join(format!(".update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let placed = place(root, tag, target, source, &work, &dest);
        let _ = fs::remove_dir_all(&work);
        placed?;
    }
    adopt_broker(root, &dest, target, source)?;
    repoint(root, &dest)?;
    Ok(dest)
}

fn place(
    root: &Path,
    tag: &str,
    target: &str,
    source: &Source,
    work: &Path,
    dest: &Path,
) -> Result<(), String> {
    let version = tag.trim_start_matches('v');
    let extension = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    let archive = format!("sterna-{version}-{target}.{extension}");
    let base = format!("{}/{tag}", source.downloads);
    download(&format!("{base}/{archive}"), &work.join(&archive))?;
    download(&format!("{base}/SHA256SUMS"), &work.join("SHA256SUMS"))?;
    let sums = fs::read_to_string(work.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    let want = sums
        .lines()
        .find_map(|line| {
            let (sum, name) = line.split_once(char::is_whitespace)?;
            (name.trim() == archive).then(|| sum.to_string())
        })
        .ok_or_else(|| format!("{archive} is not listed in the release's SHA256SUMS"))?;
    if sha256_of(&work.join(&archive))? != want {
        return Err(format!("{archive} does not match its SHA-256; refusing it"));
    }
    unpack(&work.join(&archive), work)?;
    let stage = work.join(format!("sterna-{version}-{target}"));
    let stage = if stage.is_dir() {
        stage
    } else {
        work.to_path_buf()
    };
    let partial = root.join("versions").join(format!("{tag}.partial"));
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(partial.join("bin")).map_err(|e| e.to_string())?;
    for name in ["sterna", "inference-gateway"] {
        let file = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        if stage.join(&file).is_file() {
            fs::copy(stage.join(&file), partial.join("bin").join(&file))
                .map_err(|e| e.to_string())?;
        }
    }
    if stage.join("cliproxyapi.toml").is_file() {
        fs::copy(
            stage.join("cliproxyapi.toml"),
            partial.join("cliproxyapi.toml"),
        )
        .map_err(|e| e.to_string())?;
    }
    let exe = if cfg!(windows) {
        "sterna.exe"
    } else {
        "sterna"
    };
    if !partial.join("bin").join(exe).is_file() {
        let _ = fs::remove_dir_all(&partial);
        return Err(format!("{archive} carried no sterna binary"));
    }
    fs::rename(&partial, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// `tar` unpacks both archive forms: GNU and BSD tar on Unix, and the
/// `tar.exe` Windows has shipped since 10 1803 reads a zip.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let flags = if archive.extension().is_some_and(|e| e == "zip") {
        "-xf"
    } else {
        "-xzf"
    };
    let status = std::process::Command::new("tar")
        .arg(flags)
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("tar could not unpack {}", archive.display()))
}

/// The broker the version pins, adopted through its own gateway when the pin
/// differs from the one last adopted. No pin, nothing to do.
fn adopt_broker(root: &Path, dest: &Path, target: &str, source: &Source) -> Result<(), String> {
    let Ok(pin) = fs::read_to_string(dest.join("cliproxyapi.toml")) else {
        return Ok(());
    };
    let pin: toml::Table = toml::from_str(&pin).map_err(|e| format!("cliproxyapi.toml: {e}"))?;
    let text = |key: &str| pin.get(key).and_then(toml::Value::as_str);
    let (Some(repository), Some(version)) = (text("repository"), text("version")) else {
        return Err("cliproxyapi.toml names no repository or version".to_string());
    };
    let stamp = root.join(BROKER_STAMP);
    if fs::read_to_string(&stamp).is_ok_and(|adopted| adopted.trim() == version) {
        return Ok(());
    }
    let Some(asset) = pin.get("assets").and_then(|assets| assets.get(target)) else {
        return Ok(());
    };
    let (Some(name), Some(sum)) = (
        asset.get("name").and_then(toml::Value::as_str),
        asset.get("sha256").and_then(toml::Value::as_str),
    ) else {
        return Err(format!(
            "cliproxyapi.toml's {target} entry names no asset or SHA-256"
        ));
    };
    let work = root.join(format!(".broker-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let base = source
            .broker_downloads
            .clone()
            .unwrap_or_else(|| format!("https://github.com/{repository}/releases/download"));
        download(&format!("{base}/v{version}/{name}"), &work.join(name))?;
        if sha256_of(&work.join(name))? != sum {
            return Err(format!(
                "{name} does not match the SHA-256 the release pins; refusing it"
            ));
        }
        unpack(&work.join(name), &work)?;
        let binary = work.join(if cfg!(windows) {
            "cli-proxy-api.exe"
        } else {
            "cli-proxy-api"
        });
        let gateway = dest.join("bin").join(if cfg!(windows) {
            "inference-gateway.exe"
        } else {
            "inference-gateway"
        });
        let status = std::process::Command::new(gateway)
            .args(["subscriptions", "adopt-binary"])
            .arg(&binary)
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("inference-gateway: {e}"))?;
        if !status.success() {
            return Err(format!(
                "inference-gateway refused to adopt CLIProxyAPI {version}"
            ));
        }
        fs::write(&stamp, version).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

/// Points `<root>/current` at `dest` in one rename, so a start that reads it
/// mid-update sees the old version or the new one, never neither.
fn repoint(root: &Path, dest: &Path) -> Result<(), String> {
    let link = root.join("current");
    let fresh = root.join(format!(".current-{}", std::process::id()));
    let _ = fs::remove_file(&fresh);
    #[cfg(unix)]
    std::os::unix::fs::symlink(dest, &fresh).map_err(|e| format!("{}: {e}", fresh.display()))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(dest, &fresh)
        .map_err(|e| format!("{}: {e}", fresh.display()))?;
    fs::rename(&fresh, &link).map_err(|e| format!("{}: {e}", link.display()))
}

/// The notice a finished background update left, taken once.
#[must_use]
pub fn take_notice() -> Option<String> {
    NOTICE.lock().ok()?.take()
}

/// The start-of-session notice: a newer version is already installed and
/// this session is running an older one.
#[must_use]
pub fn installed_notice(install: &Install) -> Option<String> {
    let current = install.current_tag()?;
    newer(&current, &install.tag).then(|| format!("Sterna {current} installed · restart to update"))
}

/// Checks at each session start (at most every [`CHECK_EVERY`]), on a thread
/// of its own, and installs a newer
/// release; the session shows [`take_notice`] when it finishes. Never runs
/// for a non-release install, with [`DISABLE_ENV`] set, or on a platform
/// with no release.
pub fn check_in_background() {
    if opted_out(
        std::env::var_os(DISABLE_ENV).is_some(),
        std::env::var_os(RETIRED_DISABLE_ENV).is_some(),
    ) {
        return;
    }
    let Some(install) = Install::of_running().filter(Install::is_release) else {
        return;
    };
    let Some(target) = target() else {
        return;
    };
    let stamp = install.root.join(STAMP);
    let age = fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok());
    if !due(age) {
        return;
    }
    std::thread::spawn(move || {
        let _ = fs::write(&stamp, b"");
        let source = Source::from_env();
        let Ok(latest) = latest_tag(&source) else {
            return;
        };
        let installed = install.current_tag().unwrap_or_else(|| install.tag.clone());
        if !newer(&latest, &installed) {
            return;
        }
        if install_release(&install.root, &latest, target, &source).is_ok()
            && let Ok(mut notice) = NOTICE.lock()
        {
            *notice = Some(format!("Sterna {latest} installed · restart to update"));
        }
    });
}

/// Whether the background check is off: [`DISABLE_ENV`] is set, or the name it
/// had before the rename is -- a person who turned updates off is not updated
/// because a variable was renamed under them.
fn opted_out(current: bool, retired: bool) -> bool {
    current || retired
}

/// The line a start says while only the retired opt-out name is set.
#[must_use]
pub fn retired_opt_out_notice() -> Option<String> {
    retired_notice(
        std::env::var_os(DISABLE_ENV).is_some(),
        std::env::var_os(RETIRED_DISABLE_ENV).is_some(),
    )
}

fn retired_notice(current: bool, retired: bool) -> Option<String> {
    (retired && !current).then(|| {
        format!(
            "{RETIRED_DISABLE_ENV} is now {DISABLE_ENV}; rename it in your shell profile. \
             Updates stay off until then."
        )
    })
}

/// Whether a check that last ran `age` ago may ask again; never checked is due.
fn due(age: Option<Duration>) -> bool {
    age.is_none_or(|age| age >= CHECK_EVERY)
}

/// `sterna update [--check]`: install the newest release now, or only say
/// whether there is one. Exit 0 when up to date or updated.
pub fn command(args: &[String]) -> i32 {
    let check_only = args.iter().any(|a| a == "--check");
    let Some(install) = Install::of_running() else {
        eprintln!(
            "sterna update: this Sterna is not an installed release (it runs from a build tree); install one with install.sh"
        );
        return 2;
    };
    let Some(target) = target() else {
        eprintln!("sterna update: no release is built for this platform");
        return 2;
    };
    let source = Source::from_env();
    let latest = match latest_tag(&source) {
        Ok(tag) => tag,
        Err(error) => {
            eprintln!("sterna update: {error}");
            return 1;
        }
    };
    let installed = install.current_tag().unwrap_or_else(|| install.tag.clone());
    if parse_tag(&installed).is_some() && !newer(&latest, &installed) {
        println!("Sterna {installed} is the newest release");
        return 0;
    }
    if check_only {
        println!("Sterna {latest} is available (installed: {installed}); run `sterna update`");
        return 0;
    }
    match install_release(&install.root, &latest, target, &source) {
        Ok(dest) => {
            println!(
                "Sterna {latest} installed at {} · restart to update",
                dest.display()
            );
            0
        }
        Err(error) => {
            eprintln!("sterna update: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_release_tag_parses_and_a_release_outranks_its_pre_releases() {
        assert_eq!(parse_tag("v0.1.0-pre.2"), Some((0, 1, 0, Some(2))));
        assert_eq!(parse_tag("v1.2.3"), Some((1, 2, 3, None)));
        assert_eq!(parse_tag("v0.1.0-pre.1-1316-gf96f87f0-ui3"), None);
        assert_eq!(parse_tag("0.1.0"), None);
        assert!(newer("v0.1.0-pre.10", "v0.1.0-pre.9"));
        assert!(newer("v0.1.0", "v0.1.0-pre.9"));
        assert!(!newer("v0.1.0-pre.9", "v0.1.0"));
        assert!(!newer("v0.1.0-pre.2", "v0.1.0-pre.2"));
        assert!(!newer("v0.1.0-pre.2", "v0.1.0-pre.1-1316-gf96f87f0"));
    }

    #[test]
    fn the_opt_out_from_before_the_rename_still_stops_the_check_and_asks_to_be_renamed() {
        assert!(opted_out(true, false));
        assert!(
            opted_out(false, true),
            "PANE_DISABLE_AUTOUPDATE alone still turns updates off"
        );
        assert!(!opted_out(false, false));
        let notice = retired_notice(false, true).expect("the old name alone is named");
        assert!(
            notice.contains("PANE_DISABLE_AUTOUPDATE is now STERNA_DISABLE_AUTOUPDATE"),
            "{notice}"
        );
        assert_eq!(
            retired_notice(true, true),
            None,
            "renamed already: nothing to say"
        );
        assert_eq!(retired_notice(false, false), None);
    }

    #[test]
    fn a_session_opened_ten_minutes_after_the_last_check_checks_again() {
        assert!(due(None));
        assert!(due(Some(Duration::from_secs(10 * 60))));
        assert!(
            !due(Some(Duration::from_secs(60))),
            "sessions opened together ask once"
        );
    }

    #[test]
    fn an_install_is_read_from_the_binary_path_and_a_build_tree_is_none() {
        let install = Install::from_exe(Path::new(
            "/h/.local/lib/sterna/versions/v0.1.0-pre.2/bin/sterna",
        ))
        .unwrap();
        assert_eq!(install.root, PathBuf::from("/h/.local/lib/sterna"));
        assert_eq!(install.tag, "v0.1.0-pre.2");
        assert!(install.is_release());
        assert_eq!(
            Install::from_exe(Path::new("/repo/target/release/sterna")),
            None
        );
        let dev = Install::from_exe(Path::new(
            "/r/versions/v0.1.0-pre.1-1316-gf96f87f0/bin/sterna",
        ))
        .unwrap();
        assert!(!dev.is_release());
    }
}
