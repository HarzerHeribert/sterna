//! Opening what the person clicks: files and folders with the system's
//! default app, web pages in the browser.
//!
//! **Only plain documents are opened.** Anything else -- a program, a
//! script, an installer, a shortcut, an app bundle, a file with no
//! extension -- is revealed in its folder instead, so a click on a file the
//! model wrote can never start it. What is opened is the canonical path the
//! check was made on, not the path as it was given.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// Extensions (lower case) of plain documents: text, source code that no
/// interpreter runs on a double click, markdown, images, PDF, data files,
/// and HTML, which opens as a file in the browser. Only these open; a file
/// with any other extension, or with none, is revealed in its folder. An
/// allow-list on purpose: what launches when opened differs from system to
/// system (`.fileloc`, `.inetloc`, `.py` under the Python Launcher, `.msc`,
/// `.appref-ms`, `.iso`…), and a list of those always has a gap.
const DOCUMENTS: &[&str] = &[
    // text and notes
    "txt", "text", "md", "markdown", "mdx", "rst", "adoc", "org", "log", "rtf",
    // data and settings
    "csv", "tsv", "json", "jsonl", "ndjson", "toml", "yaml", "yml", "xml", "ini", "cfg", "conf",
    "lock", "diff", "sql", "graphql", "proto",
    // source code no interpreter runs when it is opened
    "rs", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "m", "mm", "swift", "go", "java", "kt", "kts",
    "scala", "cs", "fs", "ts", "tsx", "jsx", "vue", "svelte", "css", "scss", "sass", "less", "zig",
    "hs", "ml", "ex", "exs", "erl", "dart", "nim", "sol", "tf", "nix", "gradle",
    // web pages, opened as files
    "html", "htm", // images and documents
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "heic", "avif", "pdf",
];

/// Folder extensions (lower case) that are bundles, not folders, to macOS:
/// apps and what runs inside them, installers, scripts and workflows.
const BUNDLES: &[&str] = &[
    "app",
    "appex",
    "bundle",
    "framework",
    "plugin",
    "kext",
    "prefpane",
    "xpc",
    "workflow",
    "action",
    "pkg",
    "mpkg",
    "saver",
    "qlgenerator",
    "mdimporter",
    "component",
    "scptd",
];

/// Opens a file or folder with the system's default app, or reveals it in
/// its folder when it is something that runs ([`runs_when_opened`]).
///
/// Without `root`, `path` must be a full path. With `root`, a relative
/// `path` is taken inside it, and whatever it names must lie inside `root`
/// once every link is followed.
#[tauri::command]
pub async fn open_path(app: AppHandle, path: String, root: Option<String>) -> Result<(), String> {
    let target = resolve(&path, root.as_deref())?;
    let shown = plain(&target);
    if runs_when_opened(&target) {
        app.opener()
            .reveal_item_in_dir(&shown)
            .map_err(|e| format!("{path} could not be shown in its folder: {e}"))
    } else {
        app.opener()
            .open_path(shown.to_string_lossy(), None::<&str>)
            .map_err(|e| format!("{path} could not be opened: {e}"))
    }
}

/// Opens an `http://` or `https://` page in the system's browser.
#[tauri::command]
pub async fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    let page = web_page(&url)?;
    app.opener()
        .open_url(page.as_str(), None::<&str>)
        .map_err(|e| format!("{url} could not be opened: {e}"))
}

/// The existing file or folder `path` names, every link followed.
pub fn resolve(path: &str, root: Option<&str>) -> Result<PathBuf, String> {
    let asked = Path::new(path);
    let Some(root) = root else {
        if !asked.is_absolute() {
            return Err(format!("{path} is not a full path, so it was not opened."));
        }
        return std::fs::canonicalize(asked).map_err(|_| format!("{path} does not exist."));
    };
    let folder = Path::new(root);
    if !folder.is_absolute() {
        return Err(format!("{root} is not a full path, so nothing was opened."));
    }
    let inside = std::fs::canonicalize(folder).map_err(|_| format!("{root} does not exist."))?;
    let target = std::fs::canonicalize(folder.join(asked))
        .map_err(|_| format!("{path} does not exist in {root}."))?;
    if !target.starts_with(&inside) {
        return Err(format!("{path} is outside {root}, so it was not opened."));
    }
    Ok(target)
}

/// Whether `path` is shown in its folder rather than opened: a file that
/// is not one of the [`DOCUMENTS`] -- an extension not on the list, or none
/// -- or that has an execute bit on Unix; a folder that is a bundle
/// ([`BUNDLES`]); anything that cannot be read, or is neither file nor folder.
pub fn runs_when_opened(path: &Path) -> bool {
    let Ok(about) = std::fs::metadata(path) else {
        return true;
    };
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if about.is_dir() {
        return BUNDLES.contains(&extension.as_str());
    }
    !about.is_file() || !DOCUMENTS.contains(&extension.as_str()) || executable(&about)
}

#[cfg(unix)]
fn executable(about: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    about.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_about: &std::fs::Metadata) -> bool {
    false
}

/// A canonical path as the shell expects it: Windows' `\\?\` prefix off.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(disk) if disk.as_bytes().get(1) == Some(&b':') => PathBuf::from(disk),
        _ => path.to_path_buf(),
    }
}

/// `url` parsed, if it is a web page; any other scheme is refused.
fn web_page(url: &str) -> Result<tauri::Url, String> {
    let page = tauri::Url::parse(url.trim()).map_err(|_| format!("{url} is not a web address."))?;
    match page.scheme() {
        "http" | "https" => Ok(page),
        scheme => Err(format!(
            "{url} was not opened: only http and https addresses open in the browser, not {scheme}."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scratch;

    #[test]
    fn only_web_pages_open_in_the_browser() {
        for ok in [
            "https://example.com/sign-in?x=1",
            "http://127.0.0.1:8080/",
            " HTTPS://a.b ",
        ] {
            assert!(web_page(ok).is_ok(), "{ok}");
        }
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "mailto:a@b.c",
            "ftp://a.b/",
        ] {
            let error = web_page(refused).unwrap_err();
            assert!(error.contains("only http and https"), "{error}");
        }
        for nonsense in ["", "example.com", "/usr/bin"] {
            let error = web_page(nonsense).unwrap_err();
            assert!(error.contains("is not a web address"), "{error}");
        }
    }

    #[test]
    fn a_path_is_resolved_inside_its_root_and_never_outside() {
        let scratch = Scratch::new("resolve");
        let root = scratch.0.join("project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::write(scratch.0.join("secret.txt"), "").unwrap();
        let root_text = root.to_str().unwrap();
        assert_eq!(
            resolve("src/main.rs", Some(root_text)).unwrap(),
            root.join("src/main.rs")
        );
        assert_eq!(resolve(".", Some(root_text)).unwrap(), root);
        let inside = root.join("src/main.rs");
        assert_eq!(
            resolve(inside.to_str().unwrap(), Some(root_text)).unwrap(),
            inside
        );
        let error = resolve("../secret.txt", Some(root_text)).unwrap_err();
        assert!(error.contains("is outside"), "{error}");
        let outside = scratch.0.join("secret.txt");
        let error = resolve(outside.to_str().unwrap(), Some(root_text)).unwrap_err();
        assert!(error.contains("is outside"), "{error}");
        let error = resolve("src/missing.rs", Some(root_text)).unwrap_err();
        assert!(error.contains("does not exist in"), "{error}");
        let error = resolve("src/main.rs", Some("project")).unwrap_err();
        assert!(error.contains("is not a full path"), "{error}");
        let error = resolve("src/main.rs", None).unwrap_err();
        assert!(error.contains("is not a full path"), "{error}");
        assert_eq!(resolve(inside.to_str().unwrap(), None).unwrap(), inside);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(scratch.0.join("secret.txt"), root.join("link.txt"))
                .unwrap();
            let error = resolve("link.txt", Some(root_text)).unwrap_err();
            assert!(error.contains("is outside"), "{error}");
        }
    }

    #[test]
    fn programs_and_bundles_are_revealed_not_opened() {
        let scratch = Scratch::new("runs");
        let file = |name: &str| {
            let path = scratch.0.join(name);
            std::fs::write(&path, "").unwrap();
            path
        };
        let folder = |name: &str| {
            let path = scratch.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        for runs in [
            "tool.EXE",
            "x.sh",
            "a.command",
            "Notes.webloc",
            "y.AppImage",
            "z.desktop",
            // What a deny-list missed: they launch their target, or run.
            "Desk.fileloc",
            "Site.inetloc",
            "build.py",
            "gui.pyw",
            "console.msc",
            "App.appref-ms",
            "deploy.application",
            "panel.settingcontent-ms",
            "Docs.library-ms",
            "find.search-ms",
            "help.chm",
            "disk.iso",
            "disk.vhdx",
            // No extension says nothing about what it is.
            "README",
            "Makefile",
        ] {
            assert!(runs_when_opened(&file(runs)), "{runs}");
        }
        for runs in [
            "Tool.app",
            "Kit.framework",
            "Pane.prefPane",
            "Flow.workflow",
        ] {
            assert!(runs_when_opened(&folder(runs)), "{runs}");
        }
        for opens in [
            "notes.txt",
            "photo.PNG",
            "report.pdf",
            "main.rs",
            "Cargo.toml",
            "data.csv",
            "page.html",
            "plan.md",
        ] {
            assert!(!runs_when_opened(&file(opens)), "{opens}");
        }
        for opens in ["src", "my.folder", "docs.d"] {
            assert!(!runs_when_opened(&folder(opens)), "{opens}");
        }
        assert!(runs_when_opened(&scratch.0.join("missing.txt")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tool = file("build.txt");
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o744)).unwrap();
            assert!(runs_when_opened(&tool));
        }
    }

    #[test]
    fn windows_long_path_prefixes_come_off() {
        assert_eq!(
            plain(Path::new(r"\\?\C:\Users\a\x.txt")),
            PathBuf::from(r"C:\Users\a\x.txt")
        );
        assert_eq!(
            plain(Path::new(r"\\?\UNC\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
        assert_eq!(
            plain(Path::new("/home/a/x.txt")),
            PathBuf::from("/home/a/x.txt")
        );
    }
}
