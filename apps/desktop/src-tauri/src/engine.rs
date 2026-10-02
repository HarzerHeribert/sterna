//! Which engine program `host_start` runs, and from where.
//!
//! Two can be on a machine: the command-line install
//! (`<install root>/current/bin/sterna`, which `sterna update` keeps
//! current) and the copy bundled with the app (`sterna-desktop-engine`
//! beside the app's executable, with `sterna-desktop-gateway`). The install
//! wins unless the bundled one is newer, so the app and a terminal share one
//! engine. The other is kept as a second choice, for an install too old to
//! run a host at all.
//!
//! A bundled engine on a mount that goes away (a DMG, an AppImage, a
//! translocated app) is copied into the app's data folder first: the host it
//! starts outlives the app, so it must not run from a mount.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use semver::Version;
use tauri::{AppHandle, Manager};

/// How long `--version` may take.
const VERSION_WITHIN: Duration = Duration::from_secs(5);

const EXE: &str = std::env::consts::EXE_SUFFIX;

/// Where things are on this machine, read once on the main side.
#[derive(Clone, Debug, Default)]
pub struct Places {
    /// The app's own executable.
    pub exe: Option<PathBuf>,
    /// `$STERNA_HOME`, else the platform's install root.
    pub install_root: Option<PathBuf>,
    /// Where a bundled engine is copied off a transient mount.
    pub stage_root: Option<PathBuf>,
    /// The engine's data folder, which holds `host.json`.
    pub data_folder: Option<PathBuf>,
    /// The app runs from a DMG, an AppImage or a translocated copy.
    pub transient: bool,
}

impl Places {
    pub fn of(app: &AppHandle) -> Self {
        let exe = std::env::current_exe().ok();
        let local = app.path().local_data_dir().ok();
        let home = named("HOME").or_else(|| named("USERPROFILE"));
        let install_root = named("STERNA_HOME").or_else(|| {
            if cfg!(windows) {
                local
                    .as_ref()
                    .map(|local| local.join("Programs").join("sterna"))
            } else {
                home.as_ref()
                    .map(|home| home.join(".local").join("lib").join("sterna"))
            }
        });
        let appdir = named("APPDIR");
        let appimage = named("APPIMAGE").is_some() || appdir.is_some();
        let transient = exe.as_deref().is_some_and(|exe| {
            on_transient_mount(std::env::consts::OS, exe, appimage, appdir.as_deref())
        });
        Self {
            data_folder: data_folder(
                named("XDG_DATA_HOME"),
                named("XDG_CONFIG_HOME"),
                home,
                local,
                cfg!(windows),
            ),
            stage_root: app
                .path()
                .app_local_data_dir()
                .ok()
                .map(|dir| dir.join("engine")),
            exe,
            install_root,
            transient,
        }
    }
}

/// An environment variable naming an absolute path.
fn named(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// The engine's data folder, by the engine's own rule
/// (`crates/sterna/src/engine/data.rs`): `$XDG_DATA_HOME/sterna`; else
/// `$XDG_CONFIG_HOME/sterna/data` when that is not the usual `~/.config`;
/// else the platform's (`<local data>/sterna`, `…\sterna\data` on Windows).
pub fn data_folder(
    data: Option<PathBuf>,
    config: Option<PathBuf>,
    home: Option<PathBuf>,
    local: Option<PathBuf>,
    windows: bool,
) -> Option<PathBuf> {
    if let Some(data) = data {
        return Some(data.join("sterna"));
    }
    if let Some(config) = config
        && home.map(|home| home.join(".config")).as_ref() != Some(&config)
    {
        return Some(config.join("sterna").join("data"));
    }
    let local = local?.join("sterna");
    Some(if windows { local.join("data") } else { local })
}

/// Whether `exe` runs from a mount that goes away when the app quits.
///
/// Linux: an AppImage (`$APPIMAGE` or `$APPDIR` set, and the app inside
/// `$APPDIR`), or any `/.mount_` path. macOS: a disk image under `/Volumes/`
/// or a copy Gatekeeper translocated. Windows has neither: the app a release
/// places, `%LOCALAPPDATA%\Programs\sterna\versions\<tag>\desktop\Sterna.exe`,
/// runs where it is, with its engine beside it as `sterna-desktop-engine.exe`.
pub fn on_transient_mount(os: &str, exe: &Path, appimage: bool, appdir: Option<&Path>) -> bool {
    let text = exe.to_string_lossy();
    match os {
        "linux" => {
            (appimage && appdir.is_some_and(|dir| exe.starts_with(dir)))
                || text.contains("/.mount_")
        }
        "macos" => exe.starts_with("/Volumes/") || text.contains("/AppTranslocation/"),
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `STERNA_BIN`, in a debug build: run as given, nothing compared.
    Override,
    /// The command-line install.
    Installed,
    /// The copy bundled with the app.
    Bundled,
}

/// One engine program.
#[derive(Clone, Debug)]
pub struct Engine {
    pub kind: Kind,
    pub program: PathBuf,
    /// Run under `node` (a mock engine).
    pub script: bool,
    /// The gateway shipped with it, given to the host as
    /// `INFERENCE_GATEWAY_BIN` ([`gateway_env`]).
    pub gateway: Option<PathBuf>,
    /// `None` until asked; then what `--version` said, if it said one.
    version: Option<Option<Version>>,
}

impl Engine {
    pub fn new(kind: Kind, program: PathBuf) -> Self {
        Self {
            kind,
            program,
            script: false,
            gateway: None,
            version: None,
        }
    }

    /// How an error names it.
    pub fn shown(&self) -> String {
        if self.script {
            format!("node {}", self.program.display())
        } else {
            self.program.display().to_string()
        }
    }

    /// The command that runs it, with no arguments yet.
    pub fn command(&self) -> Command {
        let mut command = if self.script {
            let mut command = Command::new("node");
            command.arg(&self.program);
            command
        } else {
            Command::new(&self.program)
        };
        let inherited = std::env::var_os("INFERENCE_GATEWAY_BIN").filter(|named| !named.is_empty());
        match gateway_env(
            self.gateway.as_deref(),
            inherited.is_some(),
            cfg!(debug_assertions),
        ) {
            GatewayEnv::Inherit => {}
            GatewayEnv::Set(gateway) => {
                command.env("INFERENCE_GATEWAY_BIN", gateway);
            }
            GatewayEnv::Remove => {
                command.env_remove("INFERENCE_GATEWAY_BIN");
            }
        }
        hide_console(&mut command);
        command
    }

    /// Its version, from `<program> --version` (`sterna X.Y.Z`), asked once.
    pub fn version(&mut self) -> Option<Version> {
        if self.version.is_none() {
            self.version = Some(self.ask_version());
        }
        self.version.clone().flatten()
    }

    fn ask_version(&self) -> Option<Version> {
        let mut command = self.command();
        command
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().ok()?;
        let mut stdout = child.stdout.take()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            let _ = tx.send(text);
        });
        let said = rx.recv_timeout(VERSION_WITHIN);
        if said.is_err() {
            // This app's own child, and it did not answer.
            let _ = child.kill();
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        parse_version(said.ok()?.lines().next()?)
    }
}

/// `sterna 0.1.0` (or a bare `0.1.0`, or `v0.1.0`) as a version.
pub fn parse_version(said: &str) -> Option<Version> {
    let word = said.split_whitespace().last()?;
    Version::parse(word.strip_prefix('v').unwrap_or(word)).ok()
}

/// The bundled engine goes first only when it is newer than the install,
/// or the install's version cannot be read while the bundled one's can.
pub fn bundled_first(installed: Option<&Version>, bundled: Option<&Version>) -> bool {
    match (installed, bundled) {
        (Some(installed), Some(bundled)) => bundled > installed,
        (None, Some(_)) => true,
        (_, None) => false,
    }
}

/// The engines to try, best first.
pub fn choose(places: &Places) -> Result<Vec<Engine>, String> {
    // A release build never reads STERNA_BIN. The one variable that moves
    // where an engine is looked for is STERNA_HOME, the install root the
    // installer reads too (see `Places::of`).
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os("STERNA_BIN").filter(|path| !path.is_empty())
    {
        let program = PathBuf::from(path);
        let mut engine = Engine::new(Kind::Override, program);
        engine.script = engine
            .program
            .extension()
            .is_some_and(|ext| ext == "mjs" || ext == "js");
        return Ok(vec![engine]);
    }
    let installed = places
        .install_root
        .as_ref()
        .map(|root| {
            root.join("current")
                .join("bin")
                .join(format!("sterna{EXE}"))
        })
        .filter(|path| path.is_file())
        .map(|path| Engine::new(Kind::Installed, path));
    let beside = places.exe.as_ref().and_then(|exe| exe.parent());
    let bundled = beside
        .map(|dir| dir.join(format!("sterna-desktop-engine{EXE}")))
        .filter(|path| path.is_file())
        .map(|path| {
            let mut engine = Engine::new(Kind::Bundled, path);
            engine.gateway = beside
                .map(|dir| dir.join(format!("sterna-desktop-gateway{EXE}")))
                .filter(|path| path.is_file());
            engine
        });
    match (installed, bundled) {
        (None, None) => Err(format!(
            "The engine is missing: there is no sterna-desktop-engine beside the app{}.",
            places
                .install_root
                .as_ref()
                .map_or(String::new(), |root| format!(
                    " and no sterna installed in {}",
                    root.display()
                ))
        )),
        (Some(one), None) | (None, Some(one)) => Ok(vec![one]),
        (Some(mut installed), Some(mut bundled)) => {
            if bundled_first(installed.version().as_ref(), bundled.version().as_ref()) {
                Ok(vec![bundled, installed])
            } else {
                Ok(vec![installed, bundled])
            }
        }
    }
}

/// Copies a bundled engine off a transient mount, if it is on one, and
/// points `engine` at the copy.
pub fn prepare(engine: &mut Engine, places: &Places) -> Result<(), String> {
    if engine.kind != Kind::Bundled || !places.transient {
        return Ok(());
    }
    let version = engine
        .version()
        .map_or_else(|| "unknown".to_string(), |version| version.to_string());
    let dir = places
        .stage_root
        .as_ref()
        .ok_or("The app has no data folder to copy its engine into.")?
        .join(version);
    engine.program = copy_in(&engine.program, &dir.join(format!("sterna{EXE}")))?;
    if let Some(gateway) = &engine.gateway {
        engine.gateway = Some(copy_in(
            gateway,
            &dir.join(format!("inference-gateway{EXE}")),
        )?);
    }
    Ok(())
}

/// `from` copied to `to`, unless a file of the same size is already there.
///
/// The copy is written beside `to` under a passing name and renamed into
/// place, so nothing ever runs a half-written file and a running copy is
/// replaced, not written over.
pub fn copy_in(from: &Path, to: &Path) -> Result<PathBuf, String> {
    let failed = |e: std::io::Error| {
        format!(
            "The engine could not be copied to {}: {e}",
            to.parent().unwrap_or(to).display()
        )
    };
    let size = std::fs::metadata(from).map_err(failed)?.len();
    if std::fs::metadata(to).is_ok_and(|there| there.is_file() && there.len() == size) {
        return Ok(to.to_path_buf());
    }
    let dir = to
        .parent()
        .ok_or_else(|| failed(std::io::ErrorKind::NotFound.into()))?;
    std::fs::create_dir_all(dir).map_err(failed)?;
    let name = to
        .file_name()
        .unwrap_or(OsStr::new("engine"))
        .to_string_lossy();
    let passing = dir.join(format!(".{name}.{}.partial", std::process::id()));
    std::fs::copy(from, &passing).map_err(failed)?;
    runnable(&passing).map_err(failed)?;
    std::fs::rename(&passing, to).map_err(|e| {
        let _ = std::fs::remove_file(&passing);
        failed(e)
    })?;
    Ok(to.to_path_buf())
}

#[cfg(unix)]
fn runnable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn runnable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// No console window flashes up for the engine on Windows.
#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// What the host's environment says about the gateway.
#[derive(Debug, PartialEq, Eq)]
enum GatewayEnv<'a> {
    /// The inherited value stands.
    Inherit,
    Set(&'a Path),
    /// None inherited reaches the host: the engine finds the gateway beside it.
    Remove,
}

/// The gateway the host is given. A release build never lets an inherited
/// `INFERENCE_GATEWAY_BIN` choose it, as it never lets `STERNA_BIN` choose
/// the engine: an engine shipped with its gateway gets that one, any other
/// finds its own beside it. A debug build lets an inherited value stand.
fn gateway_env(own: Option<&Path>, inherited: bool, debug: bool) -> GatewayEnv<'_> {
    match (own, inherited, debug) {
        (_, true, true) => GatewayEnv::Inherit,
        (Some(own), _, _) => GatewayEnv::Set(own),
        (None, true, false) => GatewayEnv::Remove,
        (None, false, _) => GatewayEnv::Inherit,
    }
}

/// Waits until a script a test has just written runs. Until then Linux can
/// refuse it ("text file busy"): a process another test thread forked a
/// moment ago may still hold it open for writing. `args` keep the run free
/// of anything the test watches for.
#[cfg(all(test, unix))]
pub(crate) fn wait_until_runnable(path: &Path, args: &[&str]) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scratch;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_release_build_gives_the_host_its_own_gateway_whatever_is_inherited() {
        let own = Path::new("/app/sterna-desktop-gateway");
        assert_eq!(gateway_env(Some(own), true, false), GatewayEnv::Set(own));
        assert_eq!(gateway_env(Some(own), false, false), GatewayEnv::Set(own));
        assert_eq!(gateway_env(None, true, false), GatewayEnv::Remove);
        assert_eq!(gateway_env(None, false, false), GatewayEnv::Inherit);
        // A debug build lets an inherited one stand, as it does STERNA_BIN.
        assert_eq!(gateway_env(Some(own), true, true), GatewayEnv::Inherit);
        assert_eq!(gateway_env(Some(own), false, true), GatewayEnv::Set(own));
    }

    #[test]
    fn versions_are_read_from_what_sterna_prints() {
        assert_eq!(parse_version("sterna 0.1.0"), Some(v("0.1.0")));
        // A release says which release it is.
        assert_eq!(
            parse_version("sterna 0.1.0-pre.30"),
            Some(v("0.1.0-pre.30"))
        );
        assert_eq!(
            parse_version("sterna 1.2.3-pre.4\n"),
            Some(v("1.2.3-pre.4"))
        );
        assert_eq!(parse_version("v2.0.0"), Some(v("2.0.0")));
        assert_eq!(parse_version("sterna: unknown command"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn the_bundled_engine_goes_first_only_when_newer() {
        let (old, new) = (v("0.1.0"), v("0.2.0"));
        assert!(bundled_first(Some(&old), Some(&new)));
        assert!(!bundled_first(Some(&new), Some(&old)));
        assert!(!bundled_first(Some(&old), Some(&old)));
        assert!(bundled_first(Some(&v("0.1.0-pre.9")), Some(&old)));
        assert!(bundled_first(None, Some(&old)));
        assert!(!bundled_first(Some(&old), None));
        assert!(!bundled_first(None, None));
    }

    #[test]
    fn transient_mounts_are_recognised() {
        let appdir = Path::new("/tmp/.mount_SternaX1");
        let inside = Path::new("/tmp/.mount_SternaX1/usr/bin/sterna-desktop");
        assert!(on_transient_mount("linux", inside, true, Some(appdir)));
        assert!(on_transient_mount(
            "linux",
            Path::new("/run/.mount_a/x"),
            false,
            None
        ));
        let plain = Path::new("/usr/bin/sterna-desktop");
        assert!(!on_transient_mount(
            "linux",
            plain,
            true,
            Some(Path::new("/opt/app"))
        ));
        assert!(on_transient_mount(
            "linux",
            Path::new("/opt/app/x"),
            true,
            Some(Path::new("/opt/app"))
        ));
        assert!(!on_transient_mount(
            "linux",
            Path::new("/opt/app/x"),
            false,
            Some(Path::new("/opt/app"))
        ));
        let dmg = Path::new("/Volumes/Sterna/Sterna.app/Contents/MacOS/Sterna");
        assert!(on_transient_mount("macos", dmg, false, None));
        let moved =
            "/private/var/folders/x/AppTranslocation/1234/d/Sterna.app/Contents/MacOS/Sterna";
        assert!(on_transient_mount("macos", Path::new(moved), false, None));
        let placed = Path::new("/Applications/Sterna.app/Contents/MacOS/Sterna");
        assert!(!on_transient_mount("macos", placed, false, None));
        assert!(!on_transient_mount("windows", dmg, true, None));
        let windows =
            r"C:\Users\a\AppData\Local\Programs\sterna\versions\v0.1.0-pre.30\desktop\Sterna.exe";
        assert!(!on_transient_mount(
            "windows",
            Path::new(windows),
            false,
            None
        ));
    }

    /// The shape a release places on Windows, with this platform's file
    /// names: `<root>/versions/<tag>/desktop/Sterna` and its engine beside
    /// it, the command line in `<root>/current/bin`.
    #[cfg(unix)]
    #[test]
    fn the_engine_beside_a_placed_app_is_found() {
        let scratch = Scratch::new("placed");
        let root = scratch.0.join("Programs/sterna");
        let app = root.join("versions/v0.1.0-pre.30/desktop");
        fake_engine(&app.join("sterna-desktop-engine"), "0.1.0-pre.30");
        fake_engine(&app.join("sterna-desktop-gateway"), "0.1.0-pre.30");
        let places = Places {
            exe: Some(app.join("Sterna")),
            install_root: Some(root.clone()),
            ..Places::default()
        };
        let engines = choose(&places).unwrap();
        assert_eq!(engines.len(), 1);
        assert_eq!(engines[0].program, app.join("sterna-desktop-engine"));
        assert_eq!(engines[0].gateway, Some(app.join("sterna-desktop-gateway")));
        fake_engine(&root.join("current/bin/sterna"), "0.1.0-pre.30");
        let kinds: Vec<Kind> = choose(&places).unwrap().iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [Kind::Installed, Kind::Bundled]);
    }

    #[test]
    fn the_data_folder_follows_the_engines_rule() {
        let p = |text: &str| Some(PathBuf::from(text));
        let home = p("/home/u");
        assert_eq!(
            data_folder(p("/d"), p("/c"), home.clone(), p("/l"), false),
            p("/d/sterna")
        );
        assert_eq!(
            data_folder(None, p("/c"), home.clone(), p("/l"), false),
            p("/c/sterna/data")
        );
        assert_eq!(
            data_folder(None, p("/home/u/.config"), home.clone(), p("/l"), false),
            p("/l/sterna")
        );
        assert_eq!(
            data_folder(None, None, home, p("/l"), true),
            p("/l/sterna/data")
        );
        assert_eq!(data_folder(None, None, None, None, false), None);
    }

    #[test]
    fn a_copy_is_made_once_and_is_runnable() {
        let scratch = Scratch::new("copy");
        let from = scratch.0.join("source");
        std::fs::write(&from, b"first").unwrap();
        let to = copy_in(&from, &scratch.0.join("staged/0.1.0/sterna")).unwrap();
        assert_eq!(to, scratch.0.join("staged/0.1.0/sterna"));
        assert_eq!(std::fs::read(&to).unwrap(), b"first");
        // Same size: left alone, even though the contents differ.
        std::fs::write(&from, b"other").unwrap();
        assert_eq!(
            std::fs::read(copy_in(&from, &to).unwrap()).unwrap(),
            b"first"
        );
        // Another size: replaced.
        std::fs::write(&from, b"longer one").unwrap();
        assert_eq!(
            std::fs::read(copy_in(&from, &to).unwrap()).unwrap(),
            b"longer one"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&to).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        let leftovers: Vec<_> = std::fs::read_dir(to.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[cfg(unix)]
    fn fake_engine(path: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\necho 'sterna {version}'\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::wait_until_runnable(path, &["--version"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_command_line_copy_wins_unless_the_bundled_engine_is_newer() {
        let scratch = Scratch::new("choose");
        let root = scratch.0.join("cli");
        let app = scratch.0.join("app");
        let installed = root.join("current/bin/sterna");
        let bundled = app.join("sterna-desktop-engine");
        let places = Places {
            exe: Some(app.join("Sterna")),
            install_root: Some(root.clone()),
            ..Places::default()
        };
        let order = |places: &Places| -> Vec<Kind> {
            choose(places)
                .unwrap()
                .iter()
                .map(|engine| engine.kind)
                .collect()
        };
        assert!(
            choose(&places)
                .unwrap_err()
                .contains("The engine is missing")
        );
        fake_engine(&bundled, "0.2.0");
        assert_eq!(order(&places), [Kind::Bundled]);
        fake_engine(&installed, "0.2.0");
        assert_eq!(order(&places), [Kind::Installed, Kind::Bundled]);
        fake_engine(&installed, "0.1.9");
        assert_eq!(order(&places), [Kind::Bundled, Kind::Installed]);
        // The gateway is set only for the bundled engine, and only if present.
        let engines = choose(&places).unwrap();
        assert_eq!(engines[0].gateway, None);
        fake_engine(&app.join("sterna-desktop-gateway"), "0.2.0");
        let engines = choose(&places).unwrap();
        assert_eq!(engines[0].gateway, Some(app.join("sterna-desktop-gateway")));
        assert_eq!(engines[1].gateway, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_bundled_engine_on_a_mount_runs_from_a_copy() {
        let scratch = Scratch::new("stage");
        let app = scratch.0.join("app");
        fake_engine(&app.join("sterna-desktop-engine"), "0.3.0");
        fake_engine(&app.join("sterna-desktop-gateway"), "0.3.0");
        let mut places = Places {
            exe: Some(app.join("Sterna")),
            stage_root: Some(scratch.0.join("data/engine")),
            ..Places::default()
        };
        let mut engine = choose(&places).unwrap().remove(0);
        prepare(&mut engine, &places).unwrap();
        assert_eq!(engine.program, app.join("sterna-desktop-engine"));
        places.transient = true;
        prepare(&mut engine, &places).unwrap();
        let staged = scratch.0.join("data/engine/0.3.0");
        assert_eq!(engine.program, staged.join("sterna"));
        assert_eq!(engine.gateway, Some(staged.join("inference-gateway")));
        assert_eq!(engine.clone().version(), Some(v("0.3.0")));
    }
}
