//! Whether the terminal's background is dark or light.
//!
//! A person can say so (`ui.background = dark|light`). Otherwise the
//! terminal is asked once, when the screen opens: an OSC 11 query for its
//! background colour, followed by a device-attributes query that every
//! terminal answers. The second answer is the fence: a terminal that
//! ignores OSC 11 is known to have ignored it once its attributes arrive,
//! so a late reply never lands in the composer as typed text. Without an
//! answer `COLORFGBG` decides, and without that the ground is dark.

use std::sync::OnceLock;

/// The `ui.background` choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Background {
    /// Ask the terminal.
    #[default]
    Auto,
    Dark,
    Light,
}

impl Background {
    pub const NAMES: [&'static str; 3] = ["auto", "dark", "light"];

    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// Whether this choice means a light ground on this terminal.
    #[must_use]
    pub fn light(self) -> bool {
        match self {
            Self::Dark => false,
            Self::Light => true,
            Self::Auto => detected(),
        }
    }
}

/// What the terminal answered, once it has been asked.
static ANSWER: OnceLock<Option<bool>> = OnceLock::new();

/// The terminal's answer, else `COLORFGBG`, else dark.
#[must_use]
pub fn detected() -> bool {
    ANSWER
        .get()
        .copied()
        .flatten()
        .or_else(|| {
            std::env::var("COLORFGBG")
                .ok()
                .as_deref()
                .and_then(from_colorfgbg)
        })
        .unwrap_or(false)
}

/// `COLORFGBG` is `fg;bg` (sometimes `fg;x;bg`) in the 16 ANSI colours: a
/// background of white (7) or a bright colour other than grey (8) is light.
#[must_use]
pub fn from_colorfgbg(value: &str) -> Option<bool> {
    let background: u8 = value.rsplit(';').next()?.trim().parse().ok()?;
    Some(matches!(background, 7 | 9..=15))
}

/// An OSC 11 reply, `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` and its terminator: a
/// background brighter than middle grey is light.
#[must_use]
pub fn from_osc11(reply: &str) -> Option<bool> {
    let rgb = &reply[reply.find("rgb:")? + 4..];
    let mut parts = rgb.split('/');
    let mut channel = || -> Option<f64> {
        let hex: String = parts
            .next()?
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .take(4)
            .collect();
        let most = (1u32 << (4 * hex.len())) - 1;
        Some(f64::from(u32::from_str_radix(&hex, 16).ok()?) / f64::from(most))
    };
    let (r, g, b) = (channel()?, channel()?, channel()?);
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b > 0.5)
}

/// Asks the terminal, once per process. Call it with raw mode on and before
/// anything else reads the input: the replies are read here and never
/// reach the key reader.
pub fn ask() {
    let _ = ANSWER.set(query());
}

#[cfg(unix)]
fn query() -> Option<bool> {
    use std::io::Write;
    use std::time::{Duration, Instant};
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    out.flush().ok()?;
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut seen: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 256];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the process's own stdin.
        let ready = unsafe { libc::poll(&mut fd, 1, left.as_millis() as libc::c_int) };
        // Only readable input is read: a descriptor `poll` cannot watch
        // (macOS says so of some terminals) would make the read below block.
        if ready <= 0 || fd.revents & libc::POLLIN == 0 {
            break;
        }
        // SAFETY: reads at most the buffer's length into the buffer.
        let read = unsafe { libc::read(0, buffer.as_mut_ptr().cast(), buffer.len()) };
        let Ok(read) = usize::try_from(read) else {
            break;
        };
        if read == 0 {
            break;
        }
        seen.extend_from_slice(&buffer[..read]);
        // The attributes reply, `ESC [ ? … c`, is the last to arrive.
        if let Some(at) = seen.windows(3).position(|w| w == b"\x1b[?")
            && seen[at..].contains(&b'c')
        {
            break;
        }
    }
    from_osc11(&String::from_utf8_lossy(&seen))
}

/// A console on Windows is not asked: `COLORFGBG` or the setting decides.
#[cfg(not(unix))]
fn query() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorfgbg_names_the_background_last() {
        assert_eq!(from_colorfgbg("15;0"), Some(false));
        assert_eq!(from_colorfgbg("0;15"), Some(true));
        assert_eq!(from_colorfgbg("0;default;7"), Some(true));
        assert_eq!(from_colorfgbg("7;8"), Some(false));
        assert_eq!(from_colorfgbg("default"), None);
    }

    #[test]
    fn an_osc_11_reply_is_read_whatever_its_precision() {
        assert_eq!(from_osc11("\x1b]11;rgb:ffff/ffff/ffff\x1b\\"), Some(true));
        assert_eq!(from_osc11("\x1b]11;rgb:0a0a/0e0e/1212\x07"), Some(false));
        assert_eq!(from_osc11("\x1b]11;rgb:f4/f6/f8\x07"), Some(true));
        assert_eq!(from_osc11("\x1b[?62;22c"), None);
    }

    #[test]
    fn a_named_background_is_not_asked_about() {
        assert!(Background::Light.light());
        assert!(!Background::Dark.light());
        for name in Background::NAMES {
            assert_eq!(Background::parse(name).map(Background::name), Some(name));
        }
    }
}
