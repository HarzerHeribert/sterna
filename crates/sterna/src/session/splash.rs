//! The tern that flies while a session starts.
//!
//! Starting waits on the gateway, and the gateway on every subscription
//! broker it starts: under a second on a good day, longer when a broker is
//! slow. The splash fills that wait on the alternate screen.
//!
//! - **Only a slow start draws.** Nothing appears for the first `GRACE`, so
//!   a quick start goes straight to the live UI.
//! - **The drawing is the traced mark, moved, never redrawn**
//!   (`art/birds/tern-mark`): the tern rises and falls a pixel as it glides
//!   and the sea runs under it. `ui.motion` slows it or holds it still.
//! - **The terminal comes back whole.** Typed keys are not echoed over the
//!   drawing, Ctrl-C still stops the process, and a signal that does puts
//!   the screen and the input mode back first.
//! - **Joined before the environment write.** [`start_gateway`] stops the
//!   thread before [`gateway::point_environment_at`], which is only sound
//!   while this process is single-threaded. The last frame stays up until
//!   the live UI starts.
//!
//! Unix only: a Windows console is left as it was.

use std::path::Path;

use crate::gateway::{self, Gateway, Serving};

/// Starts or attaches to the session's gateway -- the tern drawn while that
/// takes a while, when `settings` are a terminal session's -- then points the
/// wire at what started. The splash's last frame comes back [`Held`].
pub(super) fn start_gateway(
    settings: Option<&toml::Value>,
    gateway: &Gateway,
    named: bool,
    log: &Path,
) -> Result<(Option<Serving>, Option<Held>), String> {
    #[cfg(unix)]
    let splash = settings.map(flight::Splash::start);
    #[cfg(not(unix))]
    let _ = settings;
    let serving = gateway::start_or_attach(gateway, named, log)?;
    #[cfg(unix)]
    let held = splash.and_then(flight::Splash::stop);
    #[cfg(not(unix))]
    let held = None;
    if let Some(serving) = &serving {
        gateway::point_environment_at(serving);
    }
    Ok((serving, held))
}

/// The splash's last frame, up on the alternate screen. Dropped, it leaves
/// that screen -- right before the live UI enters it, so the live UI saves
/// the shell's cursor and not the splash's.
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) struct Held;

impl Drop for Held {
    fn drop(&mut self) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?1049l");
        let _ = out.flush();
    }
}

#[cfg(unix)]
mod flight {
    use std::io::Write;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::Held;
    use crate::tui::Motion;
    use crate::tui::background::{self, Background};
    use crate::workbench::plumage::{self, Bird};

    /// How long a start runs before the splash draws.
    const GRACE: Duration = Duration::from_millis(150);
    /// What the start is waiting on.
    const STATUS: &str = "Starting the model gateway";
    /// From here on the status line counts the seconds.
    const COUNT_AFTER: Duration = Duration::from_secs(3);
    /// A waterline with a few low crests and breaks, read from a moving
    /// offset.
    const SEA: &str =
        "▁▁▁▂▁▁▁▁▁ ▁▁▁▁▂▂▁▁▁▁▁▁  ▁▁▁▁▂▁▁▁▁▁▁▁▁ ▁▁▁▂▂▁▁▁▁▁▁▁▁▁▁  ▁▁▂▁▁▁▁▁▁ ▁▁▁▁▁▁▂▁▁▁▁▁";
    /// The drawing in cells: the mark is 38 pixels by 31, two pixels a cell,
    /// with one spare pixel row to fall into.
    const BIRD: (u16, u16) = (38, 16);
    /// The tern, a gap, the sea, a gap, the status line.
    const BLOCK: u16 = BIRD.1 + 4;
    /// Frames between the glide's rise and its fall.
    const GLIDE: u64 = 4;
    /// The widest the sea runs.
    const SEA_WIDTH: u16 = 64;

    pub(super) struct Splash {
        stop: mpsc::Sender<()>,
        thread: Option<thread::JoinHandle<bool>>,
    }

    impl Splash {
        pub(super) fn start(settings: &toml::Value) -> Self {
            let word =
                |key| crate::settings_session::value(settings, key).and_then(toml::Value::as_str);
            let motion = word("ui.motion")
                .and_then(Motion::parse)
                .unwrap_or_default();
            let background = word("ui.background")
                .and_then(Background::parse)
                .unwrap_or_default();
            let (stop, stopped) = mpsc::channel();
            let thread = thread::spawn(move || fly(motion, background, &stopped));
            Self {
                stop,
                thread: Some(thread),
            }
        }

        /// Stops the drawing and joins its thread: `Some` is the frame it
        /// left up.
        pub(super) fn stop(mut self) -> Option<Held> {
            self.join().then_some(Held)
        }

        fn join(&mut self) -> bool {
            let _ = self.stop.send(());
            self.thread
                .take()
                .is_some_and(|thread| thread.join().unwrap_or(false))
        }
    }

    impl Drop for Splash {
        /// A start that failed takes its drawing down before it says why.
        fn drop(&mut self) {
            if self.join() {
                drop(Held);
            }
        }
    }

    /// The splash's thread: waits out [`GRACE`], then draws until told to
    /// stop. `true` when it drew, and so left the alternate screen up.
    fn fly(motion: Motion, background: Background, stopped: &mpsc::Receiver<()>) -> bool {
        let started = Instant::now();
        if !matches!(stopped.recv_timeout(GRACE), Err(RecvTimeoutError::Timeout)) {
            return false;
        }
        let Some(quiet) = Quiet::begin() else {
            return false;
        };
        // Asked here, with echo off, so the live UI finds it answered.
        background::ask();
        let light = background.light();
        let truecolor = plumage::truecolor();
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J");
        ENTERED.store(true, Ordering::SeqCst);
        let mut tick = 0;
        let mut drawn = None;
        loop {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let scene = Scene {
                cols,
                rows,
                tick,
                elapsed: started.elapsed(),
                light,
                truecolor,
            };
            let mut bytes = String::new();
            if drawn.is_some_and(|(c, r, _)| (c, r) != (cols, rows)) {
                bytes.push_str("\x1b[2J");
            }
            bytes.push_str(&frame(scene, drawn != Some((cols, rows, scene.lowered()))));
            let _ = out.write_all(bytes.as_bytes());
            let _ = out.flush();
            drawn = Some((cols, rows, scene.lowered()));
            if !matches!(
                stopped.recv_timeout(motion.period()),
                Err(RecvTimeoutError::Timeout)
            ) {
                break;
            }
            if motion != Motion::Off {
                tick += 1;
            }
        }
        let _ = out.write_all(b"\x1b[?25h");
        let _ = out.flush();
        drop(quiet);
        true
    }

    /// What one frame shows.
    #[derive(Debug, Clone, Copy)]
    struct Scene {
        cols: u16,
        rows: u16,
        /// Frames the glide has come: the sea has run this many cells.
        tick: u64,
        elapsed: Duration,
        light: bool,
        truecolor: bool,
    }

    impl Scene {
        /// Pixels the tern sits lower: it rises and falls every [`GLIDE`]
        /// frames.
        fn lowered(self) -> usize {
            usize::from((self.tick / GLIDE) % 2 == 1)
        }

        /// The tern needs true colour and room for the whole block.
        fn shows_bird(self) -> bool {
            self.truecolor && self.cols >= BIRD.0 + 2 && self.rows >= BLOCK
        }
    }

    /// The bytes that draw `scene`, the tern included when `bird` says so:
    /// it moves every [`GLIDE`] frames, so the frames between redraw the sea
    /// and the status line alone.
    fn frame(scene: Scene, bird: bool) -> String {
        let height = if scene.shows_bird() { BLOCK } else { 3 };
        let top = scene.rows.saturating_sub(height) / 2 + 1;
        let mut out = String::from("\x1b[?2026h");
        if scene.shows_bird() && bird {
            let left = (scene.cols - BIRD.0) / 2 + 1;
            for (row, line) in (top..).zip(plumage::mark(scene.light, scene.lowered())) {
                out.push_str(&format!("\x1b[{row};1H\x1b[2K\x1b[{row};{left}H{line}"));
            }
        }
        let width = scene.cols.saturating_sub(4).min(SEA_WIDTH);
        let run = scene.tick % SEA.chars().count() as u64;
        let sea: String = SEA
            .chars()
            .cycle()
            .skip(usize::try_from(run).unwrap_or(0))
            .take(width.into())
            .collect();
        // The tern's own grey, which reads on a dark ground and a light one.
        let grey = Bird::ArcticTern.plumage().highlight;
        let colour = if scene.truecolor {
            format!(
                "\x1b[38;2;{};{};{}m",
                grey >> 16,
                (grey >> 8) & 0xff,
                grey & 0xff
            )
        } else {
            "\x1b[2m".to_owned()
        };
        let row = top + height - 3;
        let left = (scene.cols - width) / 2 + 1;
        out.push_str(&format!(
            "\x1b[{row};1H\x1b[2K\x1b[{row};{left}H{colour}{sea}\x1b[0m"
        ));
        let said: String = status(scene.elapsed)
            .chars()
            .take(scene.cols.into())
            .collect();
        let row = top + height - 1;
        let left = scene.cols.saturating_sub(said.chars().count() as u16) / 2 + 1;
        out.push_str(&format!("\x1b[{row};1H\x1b[2K\x1b[{row};{left}H{said}"));
        out.push_str("\x1b[?2026l");
        out
    }

    /// What the start is waiting on, with the seconds once it is slow.
    fn status(elapsed: Duration) -> String {
        if elapsed < COUNT_AFTER {
            STATUS.to_owned()
        } else {
            format!("{STATUS} · {} s", elapsed.as_secs())
        }
    }

    /// The terminal's input mode as it was, for the signal handler.
    static SAVED: OnceLock<libc::termios> = OnceLock::new();
    /// Whether the alternate screen is up, for the signal handler.
    static ENTERED: AtomicBool = AtomicBool::new(false);
    /// The signals that stop a start: each puts the terminal back first.
    const STOPPING: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

    /// Echo and line mode off while the tern flies, so a typed key cannot
    /// land on the drawing; signals still arrive.
    struct Quiet {
        saved: libc::termios,
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl Quiet {
        fn begin() -> Option<Self> {
            // SAFETY: zeroed is a valid termios for `tcgetattr` to fill, and
            // it is used only when that call succeeds.
            let mut saved: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(0, &mut saved) } != 0 {
                return None;
            }
            let _ = SAVED.set(saved);
            ENTERED.store(false, Ordering::SeqCst);
            let mut quiet = saved;
            quiet.c_lflag &= !(libc::ECHO | libc::ICANON);
            quiet.c_cc[libc::VMIN] = 1;
            quiet.c_cc[libc::VTIME] = 0;
            // SAFETY: this terminal's own termios with two flags cleared.
            if unsafe { libc::tcsetattr(0, libc::TCSANOW, &quiet) } != 0 {
                return None;
            }
            let previous = STOPPING
                .iter()
                .map(|&signal| {
                    // SAFETY: a zeroed sigaction is an empty one; the handler
                    // is set and the mask emptied before it is installed, and
                    // the previous disposition is kept to put back.
                    unsafe {
                        let mut action: libc::sigaction = std::mem::zeroed();
                        action.sa_sigaction =
                            restore_then_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
                        libc::sigemptyset(&mut action.sa_mask);
                        let mut previous: libc::sigaction = std::mem::zeroed();
                        libc::sigaction(signal, &action, &mut previous);
                        (signal, previous)
                    }
                })
                .collect();
            Some(Self { saved, previous })
        }
    }

    impl Drop for Quiet {
        fn drop(&mut self) {
            for (signal, previous) in &self.previous {
                // SAFETY: the disposition this signal had before `begin`.
                unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
            }
            // SAFETY: the termios `begin` read from this terminal.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.saved) };
        }
    }

    /// Puts the terminal back, then lets the signal do what it would have.
    extern "C" fn restore_then_stop(signal: libc::c_int) {
        const LEAVE: &[u8] = b"\x1b[?25h\x1b[?1049l";
        // SAFETY: `write`, `tcsetattr`, `signal` and `raise` are all
        // async-signal-safe, and the statics were set before this handler
        // was installed.
        unsafe {
            if ENTERED.load(Ordering::SeqCst) {
                libc::write(1, LEAVE.as_ptr().cast(), LEAVE.len());
            }
            if let Some(saved) = SAVED.get() {
                libc::tcsetattr(0, libc::TCSANOW, saved);
            }
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn scene(cols: u16, rows: u16, tick: u64) -> Scene {
            Scene {
                cols,
                rows,
                tick,
                elapsed: Duration::from_secs(1),
                light: false,
                truecolor: true,
            }
        }

        /// The tern sits in the middle of an 80 by 30 terminal, the sea two
        /// rows under it and the status line two under that.
        #[test]
        fn the_tern_sits_in_the_middle_over_the_sea_and_the_status() {
            let drawn = frame(scene(80, 30, 0), true);
            assert!(
                drawn.contains("\x1b[6;22H"),
                "the tern's first row: {drawn:?}"
            );
            assert!(
                drawn.contains("\x1b[21;22H"),
                "the tern's last row: {drawn:?}"
            );
            assert!(!drawn.contains("\x1b[22;22H"), "{drawn:?}");
            assert!(
                drawn.contains("\x1b[23;9H\x1b[38;2;112;122;133m"),
                "{drawn:?}"
            );
            assert!(drawn.contains(&format!("\x1b[25;28H{STATUS}")), "{drawn:?}");
        }

        /// A terminal without true colour, or too small for the tern, gets
        /// the sea and the status line alone.
        #[test]
        fn a_terminal_that_cannot_show_the_tern_gets_the_sea_and_the_status() {
            for (cols, rows, truecolor) in [(39, 30, true), (80, 19, true), (80, 30, false)] {
                let drawn = frame(
                    Scene {
                        truecolor,
                        ..scene(cols, rows, 0)
                    },
                    true,
                );
                assert!(
                    !drawn.contains('▀') && !drawn.contains('▄'),
                    "{cols}x{rows}"
                );
                assert!(drawn.contains(STATUS), "{cols}x{rows}: {drawn:?}");
            }
            assert!(frame(scene(40, 20, 0), true).contains('▀'));
        }

        /// The sea runs a cell a frame; the tern falls a pixel after
        /// [`GLIDE`] frames and rises again after as many more.
        #[test]
        fn the_sea_runs_and_the_tern_rises_and_falls() {
            let sea = |tick| {
                let drawn = frame(scene(80, 30, tick), false);
                drawn.split("\x1b[23;9H").nth(1).unwrap().to_owned()
            };
            assert_ne!(sea(0), sea(1));
            assert_eq!(sea(0), sea(SEA.chars().count() as u64));
            let lowered: Vec<usize> = (0..GLIDE * 2 + 1)
                .map(|tick| scene(80, 30, tick).lowered())
                .collect();
            assert_eq!(lowered, [0, 0, 0, 0, 1, 1, 1, 1, 0]);
            assert_ne!(plumage::mark(false, 0), plumage::mark(false, 1));
            assert!(!frame(scene(80, 30, 1), false).contains('▀'));
        }

        /// The seconds show only once the start is slow.
        #[test]
        fn the_seconds_show_once_the_start_is_slow() {
            assert_eq!(status(Duration::from_millis(2_900)), STATUS);
            assert_eq!(status(Duration::from_secs(4)), format!("{STATUS} · 4 s"));
        }
    }
}
