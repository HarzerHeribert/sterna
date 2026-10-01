//! The user's data folder: the engine's own state (`docs/engine.md`). The
//! list of folders and sessions, the host's address, and a running
//! session's port each live here, in files only the user can read.

use std::io::Write;
use std::path::{Path, PathBuf};

/// `$XDG_DATA_HOME/sterna` when that variable is set and absolute, else the
/// platform's own: `~/Library/Application Support/sterna`,
/// `~/.local/share/sterna`, `%LOCALAPPDATA%\sterna\data`.
///
/// **A settings folder moved elsewhere takes the data with it.** When
/// `$XDG_CONFIG_HOME` names a folder other than `~/.config` and no data
/// home is named, the data lives in `$XDG_CONFIG_HOME/sterna/data`: a
/// session pointed at other user settings -- a test's, a portable install's
/// -- must not list itself among the person's own sessions.
#[must_use]
pub fn folder() -> Option<PathBuf> {
    let named = |variable: &str| {
        std::env::var_os(variable)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    resolve(
        named("XDG_DATA_HOME"),
        named("XDG_CONFIG_HOME"),
        named("HOME").or_else(|| named("USERPROFILE")),
    )
    .or_else(|| {
        directories::ProjectDirs::from("", "", "sterna")
            .map(|dirs| dirs.data_local_dir().to_path_buf())
    })
}

/// The rule [`folder`] applies, given the variables it read; `None` is the
/// platform's own folder.
fn resolve(
    data: Option<PathBuf>,
    config: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(data) = data {
        return Some(data.join("sterna"));
    }
    let config = config?;
    let usual = home.map(|home| home.join(".config"));
    (usual.as_deref() != Some(config.as_path())).then(|| config.join("sterna").join("data"))
}

/// A fresh 256-bit secret, as hex.
#[must_use]
pub fn token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system's random source answers");
    hex::encode(bytes)
}

/// Writes `bytes` to `path` whole or not at all, readable by the user
/// alone: a temporary file beside it, then a rename over it.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    private_folder(parent);
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let temporary = parent.join(format!(".{name}.{}.{}", std::process::id(), token_short()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    replace(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// A rename over an existing file. Windows refuses one while a scanner
/// holds the target open for a moment, so it is tried a few times there.
fn replace(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error)
                if cfg!(windows)
                    && error.kind() == std::io::ErrorKind::PermissionDenied
                    && tries < 20 =>
            {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
}

fn token_short() -> String {
    token()[..8].to_string()
}

/// The data folder and its subfolders are the user's alone. Windows has no
/// mode bits: the profile folder's own access rules already keep it so.
fn private_folder(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// A running session's entry: where its port listens and the token that
/// opens it (`live/<id>.json`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Live {
    pub id: String,
    pub root: String,
    pub listening: String,
    pub token: String,
    pub pid: u32,
    /// When the session started, in Unix milliseconds.
    pub started: u64,
}

impl Live {
    fn path(folder: &Path, id: &str) -> PathBuf {
        folder.join("live").join(format!("{id}.json"))
    }

    /// Every running session's entry the data folder holds; one that cannot
    /// be read is left out.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let Some(folder) = folder() else {
            return Vec::new();
        };
        let mut found: Vec<Self> = std::fs::read_dir(folder.join("live"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        found.sort_by_key(|live| live.started);
        found
    }

    /// Whether the session still answers on its port.
    #[must_use]
    pub fn answers(&self) -> bool {
        self.listening
            .parse::<std::net::SocketAddr>()
            .ok()
            .is_some_and(|address| {
                std::net::TcpStream::connect_timeout(
                    &address,
                    std::time::Duration::from_millis(300),
                )
                .is_ok()
            })
    }

    /// Writes the entry; it goes when the returned guard does.
    pub fn publish(self) -> std::io::Result<Published> {
        let folder = folder().ok_or_else(|| std::io::Error::other("no data folder"))?;
        let path = Self::path(&folder, &self.id);
        write_private(
            &path,
            serde_json::to_string(&self)
                .map_err(std::io::Error::other)?
                .as_bytes(),
        )?;
        Ok(Published(path))
    }
}

/// A running session's entry, removed when the session ends.
#[derive(Debug)]
pub struct Published(PathBuf);

impl Drop for Published {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_settings_folder_takes_the_data_with_it_and_the_usual_one_does_not() {
        let home = PathBuf::from("/home/ada");
        assert_eq!(
            resolve(
                Some("/data".into()),
                Some("/elsewhere".into()),
                Some(home.clone())
            ),
            Some(PathBuf::from("/data/sterna"))
        );
        assert_eq!(
            resolve(None, Some("/elsewhere".into()), Some(home.clone())),
            Some(PathBuf::from("/elsewhere/sterna/data"))
        );
        assert_eq!(
            resolve(None, Some(home.join(".config")), Some(home.clone())),
            None
        );
        assert_eq!(resolve(None, None, Some(home)), None);
    }

    #[test]
    fn a_token_is_256_bits_of_hex_and_never_the_same_twice() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn a_private_write_replaces_the_file_whole() {
        let dir = std::env::temp_dir().join(format!("sterna-data-{}", token_short()));
        let path = dir.join("one.json");
        write_private(&path, b"first").unwrap();
        write_private(&path, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no temporary file stays behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
