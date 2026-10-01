//! A real `sterna` under a pseudo-terminal, for the checks where the
//! terminal is one of the engine's clients. The same answers to the
//! terminal's own questions as `tui_live.rs` gives (the cursor position,
//! which crossterm needs on Windows; the device attributes).
#![allow(dead_code)]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub struct Terminal {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    input: Box<dyn Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    pub screen: vt100::Parser,
}

impl Terminal {
    /// `sterna` with `args`, run in `root` with `variables` set, on a
    /// 30 by `cols` terminal.
    pub fn start(root: &Path, args: &[&str], variables: &[(String, String)], cols: u16) -> Self {
        Self::sized(root, args, variables, 30, cols)
    }

    /// The same on a `rows` by `cols` terminal.
    pub fn sized(
        root: &Path,
        args: &[&str],
        variables: &[(String, String)],
        rows: u16,
        cols: u16,
    ) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sterna"));
        command.cwd(root);
        command.args(args);
        command.env_remove("COLORTERM");
        command.env_remove("ANTHROPIC_API_KEY");
        command.env_remove("ANTHROPIC_AUTH_TOKEN");
        command.env("TERM", "xterm-256color");
        for (name, value) in variables {
            command.env(name, value);
        }
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let input = pair.master.take_writer().unwrap();
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            master: pair.master,
            child,
            input,
            output,
            screen: vt100::Parser::new(rows, cols, 2000),
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }

    /// Types `text` and presses Enter.
    pub fn say(&mut self, text: &str) {
        self.send(text.as_bytes());
        thread::sleep(Duration::from_millis(60));
        self.send(b"\r");
    }

    fn pump(&mut self, wait: Duration) {
        if let Ok(bytes) = self.output.recv_timeout(wait) {
            if bytes.windows(4).any(|w| w == b"\x1b[6n") {
                let _ = self.input.write_all(b"\x1b[1;1R");
                let _ = self.input.flush();
            }
            if bytes.windows(3).any(|w| w == b"\x1b[c") {
                let _ = self.input.write_all(b"\x1b[?62;22c");
                let _ = self.input.flush();
            }
            self.screen.process(&bytes);
        }
    }

    pub fn contents(&self) -> String {
        self.screen.screen().contents()
    }

    /// Waits until the screen satisfies `wanted`.
    pub fn wait(&mut self, description: &str, wanted: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if wanted(&self.contents()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{description}; the screen:\n{}",
                self.contents()
            );
            self.pump(Duration::from_millis(25));
        }
    }

    pub fn contains(&mut self, needle: &str) {
        self.wait(&format!("{needle:?} never showed"), |screen| {
            screen.contains(needle)
        });
    }

    /// A left click on the first place the screen shows `text`, the way a
    /// mouse sends it: a press and a release there.
    pub fn click_on(&mut self, text: &str) {
        self.contains(text);
        // Where it stands once the screen has stopped moving: a turn that
        // just ended still scrolls its answer into view.
        let mut screen = self.contents();
        loop {
            self.settle(300);
            let now = self.contents();
            if now == screen {
                break;
            }
            screen = now;
        }
        let (row, line) = screen
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(text))
            .expect("the text is on screen");
        let before = &line[..line.find(text).unwrap()];
        let column = before.chars().count() + text.chars().count() / 2 + 1;
        let row = row + 1;
        self.send(format!("\x1b[<0;{column};{row}M").as_bytes());
        std::thread::sleep(Duration::from_millis(30));
        self.send(format!("\x1b[<0;{column};{row}m").as_bytes());
    }

    /// The first frame is up.
    pub fn ready(&mut self) {
        self.contains("STERNA");
    }

    /// Everything the session has drawn so far, applied.
    pub fn settle(&mut self, millis: u64) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        while Instant::now() < deadline {
            self.pump(Duration::from_millis(25));
        }
    }

    /// Whether the process has exited, after giving it `millis` to.
    pub fn exited_within(&mut self, millis: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(millis);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return true;
            }
            self.pump(Duration::from_millis(25));
        }
        false
    }

    pub fn resize(&mut self, cols: u16) {
        let (rows, _) = self.screen.screen().size();
        self.screen.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
