//! Real PTY + terminal-emulator coverage; no external model or credentials.
//!
//! **The harness answers a cursor-position query, and on Windows nothing works
//! without it.** A terminal that is asked `ESC[6n` replies with the cursor's
//! row and column; crossterm needs that answer on Windows, where there is no
//! ioctl to read it from, and blocks until it arrives. Measured on the ARM64
//! Windows VM: sterna emitted exactly those four bytes and then waited forever,
//! so every test in this file timed out against a blank screen while sterna
//! itself was perfectly healthy. Unix never showed it because crossterm reads
//! the position from the kernel there and never asks.
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

struct App {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    input: Box<dyn Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    screen: vt100::Parser,
    bytes: Vec<u8>,
    /// Every byte the reader thread has taken off the pty, and how many it
    /// had taken when the last `send` was made: a timed-out `wait` reports
    /// the difference, which is what tells a session that never answered
    /// from a fixture that stopped listening.
    received: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    received_at_send: usize,
    sent_at: Instant,
    first_after_send: Option<Duration>,
    reader_ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
    root: PathBuf,
    #[cfg(unix)]
    terminal_flags: Vec<u8>,
    /// What this terminal answers when asked its background colour
    /// (OSC 11); `None` is a terminal that ignores the question.
    ground: Option<&'static str>,
}
impl App {
    fn start(base: &str) -> Self {
        Self::start_with(base, false, None)
    }

    fn start_bare(base: &str) -> Self {
        Self::start_with(base, true, None)
    }

    fn start_with_helpers(base: &str, model: &str) -> Self {
        Self::start_with(base, false, Some(model))
    }

    fn start_with(base: &str, bare: bool, helper_model: Option<&str>) -> Self {
        Self::start_with_flags(base, bare, helper_model, &[])
    }

    fn start_with_flags(
        base: &str,
        bare: bool,
        helper_model: Option<&str>,
        flags: &[&str],
    ) -> Self {
        Self::start_seeded(base, bare, helper_model, flags, &|_| {})
    }

    /// The same start, with `seed` writing into the project before sterna runs.
    fn start_seeded(
        base: &str,
        bare: bool,
        helper_model: Option<&str>,
        flags: &[&str],
        seed: &dyn Fn(&std::path::Path),
    ) -> Self {
        Self::start_in(base, bare, helper_model, flags, seed, &[])
    }

    /// The same start in a terminal that says `colours` of itself. Every
    /// other start is a terminal that claims no true colour, whatever the
    /// developer's own says, so the theme nobody chose is the same for all.
    fn start_in(
        base: &str,
        bare: bool,
        helper_model: Option<&str>,
        flags: &[&str],
        seed: &dyn Fn(&std::path::Path),
        colours: &[(&str, &str)],
    ) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "sterna-live-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).unwrap();
        seed(&root);
        if let Some(model) = helper_model {
            std::fs::create_dir_all(root.join(".sterna")).unwrap();
            std::fs::write(
                root.join(".sterna/config.toml"),
                format!("[helpers]\nacceptance_list = false\nmodel = \"{model}\"\npreflight = true\npreflight_scope = \"always\"\n"),
            )
            .unwrap();
        }
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sterna"));
        if bare {
            std::fs::create_dir_all(root.join(".sterna")).unwrap();
            std::fs::write(
                root.join(".sterna/config.toml"),
                "[model]\nparent = \"fixture-model\"\n",
            )
            .unwrap();
            command.cwd(&root);
            // A developer install must not receive this fixture's lifecycle
            // events. The absent command is the normal fail-soft seam.
            command.env("PATH", "");
        } else {
            command.args(["session", "--root"]);
            command.arg(&root);
            command.args(["--model", "fixture-model"]);
            command.arg("--gateway");
            command.arg(root.join("no-gateway"));
            // Absent: the base URL below is a loopback host, so this session
            // is *hosted*, and a hosted session's catalogue is the gateway
            // binary's. Naming the absent path pins the resolution here
            // rather than at whatever gateway the developer has installed;
            // a picker test writes its script at that path.
            command.env("INFERENCE_GATEWAY_BIN", root.join("no-gateway"));
        }
        command.args(flags.iter().copied());
        command.env_remove("COLORTERM");
        for (name, value) in colours {
            command.env(name, value);
        }
        command.env("ANTHROPIC_BASE_URL", base);
        // Every live session gets an isolated user-settings root. Besides
        // keeping these tests away from the developer's real HOME, this
        // makes Global/Project scope assertions deterministic on every OS.
        command.env("XDG_CONFIG_HOME", root.join("global-config"));
        command.env_remove("ANTHROPIC_API_KEY");
        command.env_remove("ANTHROPIC_AUTH_TOKEN");
        command.env("TERM", "xterm-256color");
        #[cfg(unix)]
        let terminal_flags = pair
            .master
            .get_termios()
            .unwrap()
            .local_flags
            .bits()
            .to_ne_bytes()
            .to_vec();
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let input = pair.master.take_writer().unwrap();
        let (sender, output) = mpsc::channel();
        let received = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader_ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let counted = std::sync::Arc::clone(&received);
        let ended = std::sync::Arc::clone(&reader_ended);
        thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        counted.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
                        if sender.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            ended.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        Self {
            master: pair.master,
            child,
            input,
            output,
            screen: vt100::Parser::new(30, 80, 1000),
            bytes: Vec::new(),
            received,
            received_at_send: 0,
            sent_at: Instant::now(),
            first_after_send: None,
            reader_ended,
            root,
            #[cfg(unix)]
            terminal_flags,
            ground: None,
        }
    }
    /// Waits for a file the session was asked to write to appear.
    ///
    /// **A screen probe cannot stand in for this any more.** A cell now
    /// shows its own source, so a path named in the program is on the screen
    /// long before anything has written it; the file itself is the only
    /// unambiguous evidence that an approval was answered.
    fn wait_for_file(&mut self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        // Non-empty, not merely present: a write that has created the file
        // and not yet flushed its bytes is not the evidence being waited on.
        while std::fs::read(self.root.join(name)).is_ok_and(|b| b.is_empty())
            || !self.root.join(name).exists()
        {
            assert!(
                Instant::now() < deadline,
                "{name} never appeared:\n{}",
                self.screen.screen().contents()
            );
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(25)) {
                self.answer_cursor_query(&bytes);
                self.screen.process(&bytes);
                self.bytes.extend(bytes);
            }
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        self.received_at_send = self.received.load(std::sync::atomic::Ordering::SeqCst);
        self.sent_at = Instant::now();
        self.first_after_send = None;
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }
    fn wait(&mut self, description: &str, predicate: impl Fn(&vt100::Screen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if predicate(self.screen.screen()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{description}:\n{}\n--- {} bytes arrived after the last send (the first of them {:?} \
                 after it); reader thread ended: {}; the last bytes received: {:?} ---",
                self.screen.screen().contents(),
                self.received.load(std::sync::atomic::Ordering::SeqCst) - self.received_at_send,
                self.first_after_send,
                self.reader_ended.load(std::sync::atomic::Ordering::SeqCst),
                String::from_utf8_lossy(&self.bytes[self.bytes.len().saturating_sub(240)..]),
            );
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(25)) {
                if self.first_after_send.is_none() {
                    self.first_after_send = Some(self.sent_at.elapsed());
                }
                self.answer_cursor_query(&bytes);
                self.screen.process(&bytes);
                self.bytes.extend(bytes);
            }
        }
    }
    /// Reply to `ESC[6n` the way a real terminal does, with the cursor's
    /// position. **Row 1, column 1 is a true answer here**, not a placeholder:
    /// the emulator this fixture keeps is the only screen there is, and the
    /// cursor starts at its origin.
    ///
    /// It answers the background query the way a terminal does too: OSC 11
    /// with its ground when it has one, and always the device attributes
    /// (`ESC [ c`) that follow it, which every terminal reports.
    fn answer_cursor_query(&mut self, bytes: &[u8]) {
        if bytes.windows(4).any(|w| w == b"\x1b[6n") {
            let _ = self.input.write_all(b"\x1b[1;1R");
            let _ = self.input.flush();
        }
        if let Some(ground) = self.ground
            && bytes.windows(6).any(|w| w == b"\x1b]11;?")
        {
            let _ = write!(self.input, "\x1b]11;{ground}\x1b\\");
            let _ = self.input.flush();
        }
        if bytes.windows(3).any(|w| w == b"\x1b[c") {
            let _ = self.input.write_all(b"\x1b[?62;22c");
            let _ = self.input.flush();
        }
    }

    fn contains(&mut self, needle: &str) {
        self.wait(needle, |screen| screen.contents().contains(needle));
    }
    /// Waits for a whole screen line to be exactly this text.
    ///
    /// **A cell shows its own source now.** `answer("X")` puts `X` on the
    /// screen the moment the program is drawn, long before the turn that
    /// returns it has ended, so a substring probe for a returned answer
    /// matches the program that will produce it and every test built on it
    /// races the session. A returned answer stands on a line of its own.
    fn contains_line(&mut self, needle: &str) {
        self.wait(&format!("a line reading {needle:?}"), |screen| {
            screen.contents().lines().any(|line| {
                // A result line may carry the run's own pass/fail mark, and
                // sit inside a cell card whose edges -- and the session card
                // beyond them -- are not its words.
                line.split('│').any(|cell| {
                    cell.trim().trim_start_matches(['✓', '✕', '·', '❯']).trim() == needle
                })
            })
        });
    }
    /// The file a choice is saved to unless Project is chosen in Settings
    /// (decision 6), in the store's own spelling.
    fn global_settings(&self) -> std::path::PathBuf {
        sterna::settings::Store::with_global(
            &self.root,
            Some(self.root.join("global-config").join("sterna")),
        )
        .unwrap()
        .path(sterna::settings::Scope::Global)
    }
    /// The session's first frame is up. The header's model chip is not the
    /// sign: at eighty columns how often it asks and which mode it is in
    /// outrank the model's name, and the name gives way first.
    fn ready(&mut self) {
        self.contains("⠿ STERNA");
    }
    /// Asserts text is **not** on a settled screen.
    ///
    /// It settles first, on purpose: `wait` stops pumping the instant its
    /// predicate holds, so a screen that was never brought up to date can
    /// satisfy an absence for entirely the wrong reason. Every absence in
    /// this file goes through here so that trap is paid for once.
    fn refute(&mut self, description: &str, needle: &str) {
        self.settle(120);
        assert!(
            !self.screen.screen().contents().contains(needle),
            "{description}: {needle:?} is on screen:\n{}",
            self.screen.screen().contents()
        );
    }
    /// Apply whatever the session has emitted so far. An assertion about the
    /// *absence* of text needs this: `wait` stops pumping the moment its
    /// predicate holds, so a screen that was never brought up to date can
    /// satisfy a `!contains` for the wrong reason.
    fn settle(&mut self, millis: u64) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        while Instant::now() < deadline {
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(25)) {
                self.answer_cursor_query(&bytes);
                self.screen.process(&bytes);
                self.bytes.extend(bytes);
            }
        }
    }
    /// Send an SGR mouse report the way the defect arrives in a real PTY: the
    /// Escape in one write, the printable remainder in the next.
    ///
    /// **This is a smoke test, not a control.** Nothing here can make the
    /// reader split at a chosen boundary: on an idle machine both writes are
    /// usually drained in one read and the split never happens, and on a
    /// loaded one the pause is whatever the scheduler gives. That is exactly
    /// how a timing-dependent reassembler shipped green from here and failed
    /// on a CI runner. The boundary-by-boundary coverage is the unit tests in
    /// `session::ui::terminal_input`, which feed the reassembler directly.
    fn send_split_mouse_report(&mut self, tail: &[u8]) {
        self.send_report_split_by(tail, Duration::from_millis(5));
    }
    /// The same, with the gap named. A *large* gap is the control the 5 ms one
    /// is not: sterna polls the terminal every 40--100 ms, so a third of a
    /// second between the two writes cannot land in one read.
    fn send_report_split_by(&mut self, tail: &[u8], gap: Duration) {
        self.send(b"\x1b");
        thread::sleep(gap);
        self.send(tail);
    }
    /// Resize the terminal and return once sterna has begun redrawing for the
    /// new size.
    ///
    /// **Returning earlier races the resize itself.** The emulator keeps its
    /// contents across `set_size`, so a caller's next `wait` can pass on the
    /// old frame and its next key is sent while sterna still has the `SIGWINCH`
    /// in hand — and crossterm's Unix source returns from a resize without
    /// reading the tty bytes the same poll reported, so that key is stranded
    /// until the one after it (measured: 5 of 12 runs of the telemetry test
    /// under 12 busy loops; the same crossterm defect Glasshouse's
    /// `tui/event.rs` narrows in its own loop, and a sterna packet of its own).
    /// Ratatui begins every post-resize redraw with a clear, so the clear is
    /// what "begun redrawing" means here; the caller still waits for the
    /// content it needs.
    fn resize(&mut self, width: u16) {
        let mark = self.bytes.len();
        self.screen.screen_mut().set_size(30, width);
        self.master
            .resize(PtySize {
                rows: 30,
                cols: width,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.bytes[mark..]
            .windows(4)
            .any(|window| window == b"\x1b[2J")
        {
            assert!(
                Instant::now() < deadline,
                "sterna did not begin a redraw (a clear) within 10s of a resize to {width} columns:\n{}",
                self.screen.screen().contents()
            );
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(25)) {
                self.answer_cursor_query(&bytes);
                self.screen.process(&bytes);
                self.bytes.extend(bytes);
            }
        }
    }
    fn exited(&mut self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                while let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(30)) {
                    self.screen.process(&bytes);
                    self.bytes.extend(bytes);
                }
                #[cfg(unix)]
                assert_eq!(
                    self.master
                        .get_termios()
                        .unwrap()
                        .local_flags
                        .bits()
                        .to_ne_bytes()
                        .to_vec(),
                    self.terminal_flags,
                    "raw terminal mode was not restored"
                );
                // A session that ended some other way than the two it is
                // asked to end by says why: what it printed last.
                if ![0, 130].contains(&status.exit_code()) {
                    let tail = &self.bytes[self.bytes.len().saturating_sub(2000)..];
                    eprintln!(
                        "sterna exited {}; its screen:\n{}\nits last output: {:?}",
                        status.exit_code(),
                        self.screen.screen().contents(),
                        String::from_utf8_lossy(tail)
                    );
                }
                return status.exit_code();
            }
            assert!(
                Instant::now() < deadline,
                "session did not exit: {}",
                self.screen.screen().contents()
            );
            // Kept up to date while waiting, so a session that does not exit
            // is reported with the screen it is actually showing.
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(20)) {
                self.screen.process(&bytes);
                self.bytes.extend(bytes);
            }
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Answers every request in turn, each after 700 ms, and hands each to the
/// channel -- so the first request a test reads is the first the session
/// sent. It used to answer one connection and drop the listener: a second
/// turn (the Windows paste branch of
/// `live_composition_completion_model_selection_busy_input_resize_and_exit`)
/// then found no server, and Windows does not refuse a closed loopback port
/// promptly (`approval_provider` has the measurement).
fn provider() -> (String, mpsc::Receiver<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (sender, requests) = mpsc::channel();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    len = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                continue;
            };
            let streaming = request["stream"] == true;
            let _ = sender.send(request);
            thread::sleep(Duration::from_millis(700));
            let body=serde_json::json!({"role":"assistant","content":[{"type":"text","text":"```sterna\nanswer(\"LIVE RESULT INTACT\");\n```"}],"usage":{"input_tokens":123,"output_tokens":12}}).to_string();
            // The client is allowed to be gone by now: every test kills sterna in
            // `App::drop`, and this thread is still inside its 700 ms sleep when
            // that happens. Windows spells the resulting write `ConnectionReset`
            // rather than `BrokenPipe`, and unwrapping it panicked a detached
            // thread mid-run for no defect at all. The request was already
            // delivered above, so a test that needed it is unaffected, and one
            // that does not gets a quiet exit instead of a panic in the log.
            if streaming {
                let body: serde_json::Value = serde_json::from_str(&body).unwrap();
                let response = body["content"][0]["text"].as_str().unwrap();
                let events = [
                    serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":123}}}),
                    serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":response}}),
                    serde_json::json!({"type":"message_delta","usage":{"output_tokens":12}}),
                    serde_json::json!({"type":"message_stop"}),
                ];
                let body = events
                    .iter()
                    .map(|e| format!("data: {e}\n\n"))
                    .collect::<String>();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            } else {
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        }
    });
    (base, requests)
}

/// Single deterministic model response for approval tests. Tool calls still
/// run through the shipped runtime, sandbox, and terminal thread.
fn approval_provider(program: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut len = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                len = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let text = format!("```sterna\n{program}\n```");
        let (mime, body) = if request["stream"] == true {
            let events = [
                serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":10}}}),
                serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":text}}),
                serde_json::json!({"type":"message_delta","usage":{"output_tokens":20}}),
                serde_json::json!({"type":"message_stop"}),
            ];
            (
                "text/event-stream",
                events
                    .iter()
                    .map(|event| format!("data: {event}\n\n"))
                    .collect::<String>(),
            )
        } else {
            ("application/json", serde_json::json!({"role":"assistant","content":[{"type":"text","text":text}],"usage":{"input_tokens":10,"output_tokens":20}}).to_string())
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        drop(stream);
        // The turn after a denied write asks the model again, and this
        // provider answers once: every later connection is accepted and
        // dropped, so the refusal is explicit. Dropping the listener instead
        // relied on the OS refusing a closed loopback port, which Windows
        // does not do promptly — measured on the ARM64 VM, 2026-09-17: the
        // connect hung past 40 s where macOS refused it at once, the turn
        // stayed "thinking", and the queued `/exit` never ran.
        while let Ok((later, _)) = listener.accept() {
            drop(later);
        }
    });
    base
}

#[test]
fn live_approval_once_session_and_deny_gate_actual_writes() {
    let base = approval_provider(
        r#"
        write({path: "once.txt", content: "once"});
        write({path: "remember.txt", content: "remember"});
        write({path: "./remember.txt", content: "remember"});
        try { write({path: "denied.txt", content: "must not appear"}); } catch (e) {}
        return "APPROVAL FINISHED";
    "#,
    );
    let mut app = App::start_with_flags(&base, false, None, &["--sandbox", "ask"]);
    app.ready();
    // The dialog prints the call's resolved path, and at 80 columns a long
    // temp root (Windows: `\\?\C:\Users\<name>\AppData\Local\Temp\…`) wraps
    // it mid-name, so `once.txt` is not on one line. Wide enough never to.
    app.resize(160);
    app.send(b"proceed\r");
    app.contains("APPROVE");
    app.contains("once.txt");
    assert!(!app.root.join("once.txt").exists());
    // Enter and pasted text must never accept the modal by accident: the
    // prompt has not been up, quietly, for half a second.
    app.send(b"\r\x1b[200~o\x1b[201~");
    app.settle(100);
    if app.root.join("once.txt").exists() {
        // Where the console strips the bracketed-paste markers before the
        // app can see them, the pasted `o` is a keystroke to the app and
        // the guard is not measurable: ConPTY does this — traced on the
        // Windows ARM64 VM, 2026-09-17: `\r ESC[200~ o ESC[201~` arrived as
        // Enter, Enter, `o` — so that `o` has just served as the allow-once
        // below. A product limit, recorded in design-decisions.md.
        println!(
            "skipped: this console strips bracketed-paste markers, so a pasted `o` is a \
             keystroke here and the paste guard is not measurable"
        );
    } else {
        assert!(!app.root.join("once.txt").exists());
        app.settle(600);
        app.send(b"o");
    }
    app.wait_for_file("once.txt");
    app.contains("│ remember");
    assert_eq!(
        std::fs::read_to_string(app.root.join("once.txt")).unwrap(),
        "once"
    );
    assert!(!app.root.join("remember.txt").exists());
    app.settle(600);
    app.send(b"s");
    // Canonically equivalent repeated arguments skip a second prompt.
    app.wait_for_file("remember.txt");
    assert_eq!(
        std::fs::read_to_string(app.root.join("remember.txt")).unwrap(),
        "remember"
    );
    assert!(!app.root.join("denied.txt").exists());
    app.contains("│ must not appear");
    app.settle(600);
    app.send(b"d");
    app.contains_line("APPROVAL FINISHED");
    assert!(!app.root.join("denied.txt").exists());
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn live_approval_ctrl_c_denies_pending_write_and_restores_terminal_on_exit() {
    let base =
        approval_provider(r#"write({path: "cancelled.txt", content: "no"}); return "done";"#);
    let mut app = App::start_with_flags(&base, false, None, &["--sandbox", "ask"]);
    app.ready();
    app.send(b"proceed\r");
    app.contains("APPROVE");
    app.send(b"\x03");
    app.wait("cancelled approval closes", |screen| {
        !screen.contents().contains("APPROVE")
    });
    // Ctrl-C at an approval stops the turn as Ctrl-C does anywhere, and
    // says so.
    app.contains("stopped by Ctrl-C");
    assert!(!app.root.join("cancelled.txt").exists());
    app.settle(300);
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// **An approval is answered on purpose.** Keys typed while it appears --
/// a sentence carried on past the moment it showed -- are held back, never
/// taken as an answer: the `s` and the `a` in "also please make sure" must
/// neither allow the write for the session nor open "another way".
#[test]
fn an_approval_ignores_keys_typed_before_it_was_shown() {
    let base = approval_provider(r#"write({path: "typed.txt", content: "no"}); return "done";"#);
    let mut app = App::start_with_flags(&base, false, None, &["--sandbox", "ask"]);
    app.ready();
    app.send(b"write it\r");
    // Alone, so it sends: an Enter with more typing already behind it is a
    // newline in the draft.
    thread::sleep(Duration::from_millis(150));
    for byte in b"also please make sure" {
        app.send(&[*byte]);
        thread::sleep(Duration::from_millis(80));
    }
    app.contains("APPROVE");
    app.settle(200);
    assert!(
        !app.root.join("typed.txt").exists(),
        "a typed-ahead letter answered the approval"
    );
    let screen = app.screen.screen().contents();
    assert!(
        screen.contains("APPROVE"),
        "the approval is gone:\n{screen}"
    );
    assert!(!screen.contains("ANOTHER WAY"), "{screen}");
    // Once the typing stops and the prompt has been up for half a second,
    // a key is an answer again.
    app.settle(600);
    app.send(b"d");
    app.wait("the denied approval closes", |screen| {
        !screen.contents().contains("APPROVE")
    });
    assert!(!app.root.join("typed.txt").exists());
    app.settle(300);
    app.send(b"\x15/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Accept one request and hold its response until the test releases it. The
/// request notification is sent only after the complete headers and body have
/// arrived, so a screen assertion made after it observes the actual interval
/// in which preflight is blocked on the provider.
fn held_provider() -> (String, mpsc::Receiver<serde_json::Value>, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (request_sender, requests) = mpsc::channel();
    let (release, held) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut len = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                len = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        request_sender
            .send(serde_json::from_slice(&body).unwrap())
            .unwrap();
        if held.recv().is_err() {
            return;
        }
        let body = serde_json::json!({
            "role":"assistant",
            "content":[{"type":"text","text":"```sterna\nanswer(\"released\");\n```"}],
        })
        .to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    (base, requests, release)
}

/// Answer the task turn with an explicit Scout call, then hold that Scout's
/// own request so the real PTY can prove the in-flight lane is wired through.
fn held_cell_helper_provider() -> (String, mpsc::Receiver<serde_json::Value>, mpsc::Sender<()>) {
    fn read_request(stream: &mut std::net::TcpStream) -> serde_json::Value {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut len = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                len = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn answer_task(stream: &mut std::net::TcpStream, request: &serde_json::Value) {
        let program =
            "```sterna\nconst found = await helper.find(\"find the needle\");\nreturn found;\n```";
        if request["stream"] == true {
            let events = [
                serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":12}}}),
                serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":program}}),
                serde_json::json!({"type":"message_delta","usage":{"output_tokens":8}}),
                serde_json::json!({"type":"message_stop"}),
            ];
            let body = events
                .iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect::<String>();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        } else {
            let body = serde_json::json!({
                "role":"assistant",
                "content":[{"type":"text","text":program}],
            })
            .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (request_sender, requests) = mpsc::channel();
    let (release, held) = mpsc::channel();
    thread::spawn(move || {
        let (mut task, _) = listener.accept().unwrap();
        let request = read_request(&mut task);
        answer_task(&mut task, &request);

        let (mut helper, _) = listener.accept().unwrap();
        let request = read_request(&mut helper);
        request_sender.send(request).unwrap();
        let _ = held.recv();
    });
    (base, requests, release)
}

#[test]
fn live_preflight_shows_the_request_scout_and_actual_effort_before_network_returns() {
    let (base, requests, _release) = held_provider();
    let mut app = App::start_with_helpers(&base, "helper-tier");
    app.ready();
    app.send(b"/effort medium\r");
    app.contains("Effort is now medium");
    let task = "find where helper cancellation is implemented";
    app.send(format!("{task}\r").as_bytes());

    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request["model"], "helper-tier");
    app.wait(
        "submitted request and Scout visible during preflight",
        |screen| {
            let text = screen.contents();
            text.contains(task)
                && text.contains("PREFLIGHT · SCOUT")
                && text.contains("scanning")
                && text.contains("searching")
                && text.contains("effort medium")
        },
    );

    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
    assert!(!app.screen.screen().alternate_screen());
    // Every way out says how to come back, Ctrl-C included.
    assert!(
        String::from_utf8_lossy(&app.bytes).contains("resume it with:  sterna --resume"),
        "a Ctrl-C exit printed no resume line"
    );
}

#[test]
fn live_cell_helper_shows_its_lane_before_its_provider_returns() {
    let (base, requests, _release) = held_cell_helper_provider();
    let mut app = App::start_with_helpers(&base, "helper-tier");
    app.ready();
    app.send(b"find this\r");

    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request["model"], "helper-tier");
    app.wait("in-flight cell helper lane", |screen| {
        let text = screen.contents();
        text.contains("find")
            && text.contains("scanning")
            && text.contains("scanning find the needle")
            && text.contains("find · helper-tier")
            && text.contains("executing")
    });

    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
    assert!(!app.screen.screen().alternate_screen());
}

#[test]
fn bare_sterna_opens_the_live_composer_in_its_current_project() {
    let mut app = App::start_bare("http://127.0.0.1:1");
    app.contains("STERNA /");
    app.contains("message or / for commands");
    app.send(b"bare entrypoint draft");
    app.contains("bare entrypoint draft");
    // Asked, so the session is one worth keeping.
    app.send(b"\r");
    app.contains("ERROR:");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
    // One file per session now, named by the id `/exit` prints, so the
    // assertion is that a rollout was written -- not where a single
    // project-wide one used to live.
    let written: Vec<String> = std::fs::read_dir(app.root.join(".sterna/sessions"))
        .expect(".sterna/sessions")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    // One session writes one rollout, and its event stream sits beside it
    // sharing the stem -- a reader that found either finds the other by
    // changing the extension. Counting directory entries would now count
    // both, so the rollout is named rather than counted.
    let rollouts: Vec<&String> = written
        .iter()
        .filter(|name| name.ends_with(".jsonl") && !name.ends_with(".events.jsonl"))
        .collect();
    assert_eq!(rollouts.len(), 1, "one session, one rollout: {written:?}");
    let stem = rollouts[0].trim_end_matches(".jsonl");
    assert!(
        written
            .iter()
            .any(|name| name == &format!("{stem}.events.jsonl")),
        "the event stream shares the rollout's stem: {written:?}"
    );
    assert!(!app.screen.screen().alternate_screen());
}

#[test]
fn live_composition_completion_model_selection_busy_input_resize_and_exit() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.contains("Describe the next step");
    assert!(app.screen.screen().alternate_screen());
    app.send(b"/theme amber\r");
    app.contains("Theme: amber");
    app.send(b"/effort medium\r");
    app.contains("Effort is now medium");
    app.send(b"/mo");
    app.contains("set the parent, helper or subagent model");
    app.send(b"\tfixture-next\r");
    app.contains("model changed to fixture-next");
    app.send(b"\x1b[200~first line\nsecond line\x1b[201~");
    app.contains("second line");
    let screen = app.screen.screen().contents();
    // How many turns this session ends up running: one, or two where the
    // paste branch below submitted the first line as a turn of its own.
    let mut turns = 1;
    let a_turn_is_running = screen.contains("thinking") || screen.contains("LIVE RESULT INTACT");
    if a_turn_is_running {
        // The console stripped the markers and the pasted newline submitted
        // the first line as a turn (the once-session test has the trace):
        // the session is thinking on it while the composer holds the second
        // line. A turn running is the whole signal -- the sent line is shown
        // at once now, so its absence from the screen no longer is.
        // The composition guard is not measurable here. Take that turn's
        // request and let it end, so the assertions below read the request
        // they were written for.
        println!(
            "skipped: this console strips bracketed-paste markers, so a pasted newline \
             submits here and the composition guard is not measurable"
        );
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        app.contains_line("LIVE RESULT INTACT");
        turns = 2;
    } else {
        assert!(
            screen.contains("first line"),
            "the first pasted line is gone:\n{screen}"
        );
    }
    app.send(b"\x15answer this\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request["model"], "fixture-next");
    assert_eq!(request["thinking"]["budget_tokens"], 16384);
    assert!(request["max_tokens"].as_u64().unwrap() > 16384);
    app.contains("thinking");
    app.send(b"next draft");
    app.contains("next draft");
    // This turn's own ending, not the first turn's: where the paste branch
    // ran, "complete" and the result are already on screen from the turn
    // before, and a `/exit` sent while this one still runs is an Enter a
    // running turn ignores.
    app.wait("this turn completes", |screen| {
        // The transcript's line ends in a full stop; the composer's border
        // says "✓ complete" too, without one, and must not be counted.
        screen.contents().matches("✓ complete.").count() >= turns
    });
    app.contains_line("LIVE RESULT INTACT");
    assert!(app.screen.screen().contents().contains("next draft"));
    for width in [60, 80, 120, 200] {
        app.resize(width);
        app.contains_line("LIVE RESULT INTACT");
        app.wait("composer survives resize", |screen| {
            screen.contents().contains("next draft") && screen.contents().contains("╰─")
        });
        if width >= 120 {
            app.contains("telemetry");
        }
    }
    app.send(b"\x02");
    app.wait("sidebar hidden", |screen| {
        !screen.contents().contains("telemetry")
    });
    app.send(b"\x02");
    app.contains("telemetry");
    app.send(b"\x15/exit\r");
    assert_eq!(app.exited(), 0);
    assert!(!app.screen.screen().alternate_screen());
    assert!(app.bytes.windows(8).any(|bytes| bytes == b"\x1b[?2004h"));
}

#[test]
fn a_request_error_is_visible_and_the_editor_remains_usable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"fail this\r");
    app.contains("ERROR:");
    app.contains("request failed");
    app.contains("Couldn't reach the model's endpoint");
    app.refute(
        "a failure says what failed once",
        "request failed: request failed",
    );
    app.send(b"/theme amber\r");
    app.contains("Theme: amber");
    app.send(b"/effort medium\r");
    app.contains("Effort is now medium");
    app.send(b"/mo");
    app.contains("set the parent, helper or subagent model");
    app.send(b"\x15/exit\r");
    assert_eq!(app.exited(), 0);
    assert!(!app.screen.screen().alternate_screen());
}

#[test]
fn double_ctrl_c_restores_the_terminal_before_exit() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
    assert!(!app.screen.screen().alternate_screen());
}

#[test]
fn slash_plan_runs_one_read_only_request_and_the_next_one_works() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.contains("◼ Sandboxed");
    // No key cycles the level: Shift-Tab outside a form does nothing to it.
    app.send(b"\x1b[Z");
    app.refute("Shift-Tab moves nothing", "Sandbox is now");
    app.contains("◼ Sandboxed");
    // The last user message of a request: where task context rides.
    let task = |request: &serde_json::Value| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|message| message["role"] == "user")
            .unwrap()
            .to_string()
    };
    app.send(b"/plan plan this\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    // The plan line is task context: it rides in the task's own message.
    assert!(
        task(&request).contains("This is a plan request"),
        "{request}"
    );
    // Plan runs cells under the plan narrowing: a cell that changes
    // nothing runs, and writes are refused by the profile.
    app.contains_line("LIVE RESULT INTACT");
    // One request: the next one works as usual.
    app.send(b"now do it\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(task(&request).contains("now do it"), "{request}");
    assert!(
        !task(&request).contains("This is a plan request"),
        "{request}"
    );
    app.wait("the second turn ends", |screen| {
        screen
            .contents()
            .lines()
            .filter(|line| line.trim() == "LIVE RESULT INTACT")
            .count()
            >= 2
    });
    app.send(b"/context\r");
    app.contains("Next request:");
    app.send(b"\x1b");
    // Closed means the panel's frame is gone, not only its text: a redraw
    // caught halfway has already cleared "Next request:" while the frame
    // is still drawn (measured locally, 2026-09-25), and an open text panel
    // swallows every plain key, Enter included -- so `/statusline compact`
    // and `/exit` sent into that gap never reach the composer, which is the
    // macOS and Windows cells' "session did not exit" with the frame still
    // on screen. The statusline's own note is the outcome waited for next;
    // the header was on screen all along and waited for nothing.
    app.wait("context panel closes", |screen| {
        let screen = screen.contents();
        !screen.contains("Next request:") && !screen.contains("Esc · Close")
    });
    app.send(b"/statusline compact\r");
    app.contains("Status line");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Full access typed as a command is confirmed on the one sheet every
/// route to it uses, and that sheet opens on Cancel.
///
/// **It is its own test, and a short one, on purpose.** A probe about one
/// command belongs in a session that is doing nothing else: riding along at
/// the end of a longer walk it went red on macOS CI while passing locally,
/// because by then nothing in that test was waiting for anything in
/// particular.
#[test]
fn full_access_typed_as_a_command_is_confirmed_first() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.contains("◼ Sandboxed");
    app.send(b"/sandbox full\r");
    app.contains("CONFIRM");
    // A key sooner than half a second is held back: the confirm arms first.
    app.settle(600);
    app.send(b"\r");
    app.wait("Cancel goes back unchanged", |screen| {
        !screen.contents().contains("CONFIRM")
    });
    app.refute("a reflexive Enter changes nothing", "Sandbox is now");
    // By its label as well as its file word.
    app.send(b"/sandbox full access\r");
    app.contains("CONFIRM");
    app.settle(600);
    app.send(b"\x1b[B");
    app.settle(60);
    app.send(b"\r");
    app.contains("Sandbox is now Full access");
    // And the session bar says so, in the word it shows everywhere else.
    // The confirmation closed itself: no Escape is sent to the composer,
    // where one followed by `/exit` can read as Alt-/.
    app.wait("the confirmation closes", |screen| {
        !screen.contents().contains("Esc · Close") && !screen.contents().contains("Esc · Back")
    });
    app.contains("⟨ ▲ Full access ⟩");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// **A sign-in runs beside the session.** The panel opens on the link to
/// copy; Esc leaves it running behind a dock chip; commands keep working
/// meanwhile; and leaving Sterna takes the gateway's sign-in down with it.
#[cfg(unix)]
#[test]
fn a_sign_in_runs_beside_the_session_and_ends_with_it() {
    use std::os::unix::fs::PermissionsExt;
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    let pid_file = app.root.join("sign-in.pid");
    let executable = app.root.join("no-gateway");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  subscriptions)\n    echo $$ > '{}'\n    printf '%s\\n' '{{\"state\":\"opened\",\"authorize_url\":\"https://accounts.x.ai/sign-in?x=1\"}}'\n    exec sleep 60 ;;\n  *) printf '%s\\n' '{{\"version\":1,\"accounts\":[]}}' ;;\nesac\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    app.send(b"/login grok\r");
    app.contains("SIGN IN · GROK");
    app.contains("copy the sign-in link");
    // Nothing opened a browser: the gateway was told not to, and the open
    // row asks first.
    app.settle(200);
    app.send(b"\x1b");
    app.contains("signing in to Grok ▸");
    // The session is free: a command answers while the sign-in waits.
    app.send(b"/status\r");
    app.contains("Sandbox: Sandboxed");
    app.settle(200);
    app.send(b"\x1b");
    app.wait("the status sheet closes", |screen| {
        !screen.contents().contains("Esc · Close")
    });
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid])
            .status()
            .is_ok_and(|status| status.success())
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(), "the sign-in outlived the session");
}

#[cfg(unix)]
#[test]
fn model_picker_sorts_accounts_and_selects_a_real_request_model() {
    use std::os::unix::fs::PermissionsExt;
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    let executable = app.root.join("no-gateway");
    std::fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' '{\"version\":1,\"accounts\":[{\"account\":\"z-account\",\"provider\":\"fixture\",\"models\":[\"z-model\"],\"scope\":\"provider-declared\"},{\"account\":\"a-account\",\"provider\":\"fixture\",\"models\":[\"b-model\",\"a-model\"],\"scope\":\"provider-declared\"}]}'\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    app.send(b"/models\r");
    // The agent tabs make every tier directly accessible.
    app.contains("⟨ Helper ⟩");
    app.contains("a-model");
    app.contains("z-model");
    // The list's last header, so the snapshot is of a whole frame, not one
    // the pty is still delivering.
    app.contains("Z-ACCOUNT");
    let content = app.screen.screen().contents();
    assert!(content.find("A-ACCOUNT").unwrap() < content.find("Z-ACCOUNT").unwrap());
    assert!(content.find("a-model").unwrap() < content.find("b-model").unwrap());
    // The picker stays open on a choice and names it; Esc leaves it.
    app.send(b"\r");
    app.contains("Main is now a-model");
    app.settle(120);
    app.send(b"\x1b");
    app.wait("the picker closes", |screen| {
        !screen.contents().contains("Esc · Close")
    });
    app.send(b"answer this\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request["model"], "a-model");
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[cfg(unix)]
#[test]
fn model_picker_searches_a_large_catalogue_and_applies_the_filtered_selection() {
    use std::os::unix::fs::PermissionsExt;
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    let models: Vec<_> = (0..304)
        .rev()
        .map(|i| format!("vendor/model-{i:03}"))
        .collect();
    let catalogue = serde_json::json!({"version": 1, "accounts": [
        {"account":"personal", "provider":"openrouter", "scope":"provider-declared", "models":models},
        {"account":"work", "provider":"openrouter", "scope":"provider-declared", "models":["vendor/model-303"]},
        {"account":"google-sub", "provider":"google", "scope":"subscription", "models":["gemini/exact"]},
        {"account":"claude-sub", "provider":"anthropic", "scope":"subscription", "models":["claude/exact"], "selectable":false, "unavailable_reason":"Pinned to another entitlement"},
        {"account":"openai-sub", "provider":"openai", "scope":"subscription", "models":["gpt/exact"]}
    ]});
    std::fs::write(app.root.join("catalogue.json"), catalogue.to_string()).unwrap();
    let executable = app.root.join("no-gateway");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncat '{}'\n",
            app.root.join("catalogue.json").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    app.send(b"/model\r");
    // The default offers what this session can actually route to, and
    // counts only that: the account pinned elsewhere is out of scope until
    // Ctrl-A asks for every account, and only then is the locked row on
    // screen to explain itself.
    app.contains("307 of 307");
    app.contains("gemini/exact");
    app.send(b"\x01");
    app.contains("308 of 308");
    app.contains("claude/exact");
    // The row says locked, and the line under the list gives the reason in
    // full.
    app.contains("locked");
    app.contains("Pinned to another entitlement");
    // The locked account's first row is its way in; the locked model under
    // it, chosen, says why it is locked and chooses nothing.
    app.contains("Sign in to anthropic");
    app.send(b"\x1b[H\x1b[B\r");
    app.settle(120);
    assert!(requests.try_recv().is_err());
    // Back to the routes this session can actually use: the locked account
    // is counted in the catalogue and gone from the list.
    app.send(b"\x01");
    app.contains("307 of 307");
    app.settle(120);
    assert!(!app.screen.screen().contents().contains("claude/exact"));
    // One list, each account under its own header: no carousel to step
    // through to reach a provider.
    app.contains("OPENROUTER");
    app.contains("gemini/exact");
    // `+` rather than a space: `Space` stages a choice for the active tier
    // now, so it no longer reaches the filter. Three terms still AND, and
    // they have to -- this fixture's `work` and `personal` accounts both
    // carry `vendor/model-303`, so one term cannot pick between them.
    app.send(b"OPENROUTER+work+303");
    app.contains("1 of 307");
    app.contains("OPENROUTER · WORK");
    app.contains("vendor/model-303");
    // `contains` stops pumping the moment it is satisfied, so an absence is
    // only true of a screen that has been brought up to date first.
    app.settle(120);
    assert!(
        !app.screen
            .screen()
            .contents()
            .to_lowercase()
            .contains("personal"),
        "{}",
        app.screen.screen().contents()
    );
    app.send(b"x");
    app.contains("No models match");
    app.send(b"\r");
    assert!(requests.try_recv().is_err());
    app.send(b"\x7f");
    app.contains("1 of 307");
    app.send(b"\x15");
    app.contains("307 of 307");
    app.send(b"\x1b[200~personal 302\x1b[201~");
    app.contains("vendor/model-302");
    app.contains("1 of 307");
    app.resize(40);
    app.contains("vendor/model-302");
    app.send(b"\r");
    // Forty columns leave the notice no room; the row's mark moves.
    app.contains("vendor/model-302  ● now");
    // The picker stays open: Esc clears the search, a second leaves.
    app.settle(120);
    app.send(b"\x1b");
    app.settle(120);
    app.send(b"\x1b");
    app.wait("the picker closes", |screen| {
        !screen.contents().contains("Esc · Close")
    });
    app.send(b"answer this\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request["model"], "vendor/model-302");
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn telemetry_and_motion_are_local_controls_with_real_response_usage() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.send(b"/motion off\r");
    app.contains("Motion reduced");
    app.send(b"/telemetry\r");
    app.contains("↑↓ request · Esc returns");
    assert!(requests.try_recv().is_err());
    app.send(b"answer this\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request["model"], "fixture-model");
    app.contains("REQUEST 01");
    app.contains("input 123");
    app.contains("output 12");
    app.contains("cost unreported");
    app.contains("1 deliveries");
    app.send(b"\x14");
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"\x14");
    app.contains("↑↓ request · Esc returns");
    app.send(b"next draft");
    for width in [60, 80, 120, 200] {
        app.resize(width);
        app.wait("redraw after resize", |screen| {
            // The current-context reading is the statusline's highest-priority
            // right-edge signal and must survive even the narrow layout.
            (25..30).any(|row| {
                screen
                    .contents_between(row, 0, row, width)
                    .contains("context 123 tokens")
            })
        });
        app.contains("REQUEST 01");
        app.contains("next draft");
    }
    app.send(b"\x1b");
    app.contains_line("LIVE RESULT INTACT");
    app.contains("next draft");
    app.send(b"\x15/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn theme_picker_applies_local_palettes_without_a_request() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.send(b"/theme\r");
    app.contains("THEMES");
    app.contains("violet");
    // The sheet opens on the theme in force; Enter applies the focused one
    // at once and the sheet stays open.
    app.send(b"\x1b[B\x1b[B\x1b[B\x1b[B\r");
    app.contains("Theme is now violet");
    app.contains("THEMES");
    app.send(b"\x1b");
    app.wait("theme sheet closed", |screen| {
        !screen.contents().contains("THEMES")
    });
    app.send(b"/theme cobalt\r");
    app.contains("Theme: cobalt");
    app.send(b"/theme mint\r");
    app.contains("Theme: mint");
    app.send(b"/theme rose\r");
    app.contains("Theme: rose");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Only a real terminal can show that Ctrl-F reaches `screen_regions` at all:
/// the structural test proves the layout, and this proves the key is bound and
/// that the composer it leaves behind still accepts and keeps a draft.
#[test]
fn ctrl_f_takes_the_screen_and_gives_it_back_with_the_draft_intact() {
    let mut app = App::start("http://127.0.0.1:1");
    app.contains("STERNA /");
    app.contains("Describe the next step");
    app.send(b"a draft mid-thought");
    app.contains("a draft mid-thought");
    app.send(b"\x06");
    app.wait("header and status gone after Ctrl-F", |screen| {
        let screen = screen.contents();
        !screen.contains("STERNA /") && !screen.contains("⟨ Settings ⟩")
    });
    // The composer is not part of the hide-set, and neither is what is in it.
    app.contains("a draft mid-thought");
    app.send(b" still typing");
    app.contains("a draft mid-thought still typing");
    app.send(b"\x06");
    app.contains("STERNA /");
    app.contains("⟨ Settings ⟩");
    app.contains("a draft mid-thought still typing");
    app.send(b"\x15/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Fullscreen says how to come back while it lasts, and with nothing typed
/// and nothing running Escape leaves it.
#[test]
fn fullscreen_says_the_way_back_and_escape_takes_it() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.send(b"\x06");
    app.wait("the chrome is gone", |screen| {
        !screen.contents().contains("STERNA /")
    });
    // Ctrl-F says what it did, as /fullscreen does, and the way back stays.
    app.contains("Fullscreen. Ctrl-F or /fullscreen restores the chrome.");
    app.contains("Ctrl-F restores");
    app.send(b"\x1b");
    app.contains("STERNA /");
    app.contains("Chrome restored.");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Unix-only, like its one caller's assertions: see
/// `mouse_reporting_asks_only_for_the_modes_the_ui_consumes`. Left ungated it
/// would be dead code on Windows, and dead code is an error under
/// `-D warnings`.
#[cfg(unix)]
fn emitted(stream: &[u8], needle: &[u8]) -> bool {
    stream.windows(needle.len()).any(|bytes| bytes == needle)
}

/// The modes sterna asks the terminal for are the modes its own event loop
/// reads. `?1002`/`?1003` would report every pointer movement over the window
/// into a handler that only matches the wheel, and each of those reports is
/// another chance for a read boundary to split one into the composer.
///
/// **The negotiation this reads is a DECSET one, and DECSET negotiation is not
/// how a Windows host asks for mouse input** — crossterm reports the ANSI form
/// unsupported there and sets `ENABLE_MOUSE_INPUT` on the console handle
/// instead, which writes no byte at all, while the bytes that do reach this
/// pty are conhost's rendering of Sterna's screen rather than Sterna's own output
/// (it consumes `?1000h`/`?1006h` into its emulator and asks the outer
/// terminal in its own words). Neither the presence nor the absence of a mode
/// on this wire is a fact about sterna there, so the wire assertions are unix's.
/// What Windows still proves is the effect, in
/// `a_fragmented_wheel_report_still_scrolls_the_transcript`, which never reads
/// a byte sterna wrote.
#[test]
fn mouse_reporting_asks_only_for_the_modes_the_ui_consumes() {
    let mut app = App::start("http://127.0.0.1:1");
    app.contains("STERNA /");
    #[cfg(unix)]
    let startup = app.bytes.len();
    #[cfg(unix)]
    {
        assert!(
            emitted(&app.bytes, b"\x1b[?1000h"),
            "press/release reporting must be requested"
        );
        assert!(
            emitted(&app.bytes, b"\x1b[?1002h"),
            "motion while a button is held must be requested: it is what a drag-selection reads"
        );
        // Every pointer move: hover highlights the target under it, and a
        // frame is drawn only when that target changes.
        assert!(
            emitted(&app.bytes, b"\x1b[?1003h"),
            "any-motion reporting must be requested for hover"
        );
        assert!(
            emitted(&app.bytes, b"\x1b[?1006h"),
            "SGR encoding must be requested"
        );
    }
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
    #[cfg(unix)]
    {
        let shutdown = &app.bytes[startup..];
        assert!(
            emitted(shutdown, b"\x1b[?1000l")
                && emitted(shutdown, b"\x1b[?1002l")
                && emitted(shutdown, b"\x1b[?1003l")
                && emitted(shutdown, b"\x1b[?1006l"),
            "every requested mode must be reset on exit"
        );
    }
}

/// A click is the case the wheel-only repair missed. Before it, the tail of a
/// split `[<0;10;5M` failed the wheel test, was queued behind the Escape and
/// typed: the composer held `[<0;10;5M[<0;10;5m` and the model was sent it.
///
/// Smoke only — `send_split_mouse_report` cannot guarantee the split, so a
/// green run here does not prove the reassembly. That proof is
/// `a_report_split_at_any_boundary_is_never_typed` in the unit tests.
#[test]
fn a_fragmented_click_report_does_not_become_prompt_text() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.send_split_mouse_report(b"[<0;10;5M");
    app.send_split_mouse_report(b"[<0;10;5m");
    app.settle(200);
    let screen = app.screen.screen().contents();
    assert!(
        !screen.contains("[<0"),
        "report reached the screen:\n{screen}"
    );
    assert!(
        !screen.contains("10;5"),
        "report reached the screen:\n{screen}"
    );
    app.send(b"CLICK_INPUT_OK\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        request["messages"][0]["content"][0]["text"],
        "CLICK_INPUT_OK"
    );
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// **The one PTY test here that controls the split.** Five milliseconds is a
/// hope — both writes usually drain in a single read, and then nothing is
/// split at all — but 300 ms cannot be: sterna polls every 40--100 ms, so the
/// Escape is certainly read alone and the tail certainly arrives in a later
/// read, long after any timer a reassembler could have kept. This is the
/// condition CI created by accident on a loaded runner, and the condition the
/// timing-based predecessor lost under: it typed `[<65;101;28M` into the
/// composer and sent it to the model. Holding a run by the grammar rather than
/// by a clock is what makes the size of the gap irrelevant.
#[test]
fn a_report_whose_halves_are_a_third_of_a_second_apart_is_still_not_typed() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    let gap = Duration::from_millis(300);
    app.send_report_split_by(b"[<65;101;28M", gap);
    app.send_report_split_by(b"[<0;10;5M", gap);
    app.send_report_split_by(b"[<0;10;5m", gap);
    app.settle(200);
    let screen = app.screen.screen().contents();
    for leak in ["[<65", "[<0;", "101;28", "10;5"] {
        assert!(
            !screen.contains(leak),
            "report reached the screen:\n{screen}"
        );
    }
    app.send(b"SLOW_SPLIT_OK\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        request["messages"][0]["content"][0]["text"],
        "SLOW_SPLIT_OK"
    );
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// The wheel half of the same repair, proved by its effect rather than by the
/// absence of text: a transcript taller than the viewport scrolls back to its
/// first line under fragmented wheel-up reports. What only a real terminal can
/// show is that the reassembled event reaches the scroll handler at all; that
/// it survives *every* split boundary is the unit tests' job, not this one's.
///
/// **The transcript is built from submitted turns, not from one bracketed
/// paste.** A paste is not how every host delivers a multi-line draft:
/// crossterm reads Windows input as console records and has no `Event::Paste`
/// there at all, so the paste that made this transcript tall on unix left it
/// short on Windows — and the test then failed on its own setup, three lines
/// before it reached the wheel it exists to test. A submitted turn is the same
/// height everywhere.
#[test]
fn a_fragmented_wheel_report_still_scrolls_the_transcript() {
    let mut app = App::start("http://127.0.0.1:1");
    app.contains("STERNA /");
    app.send(b"TOP_OF_TRANSCRIPT\r");
    app.contains("ERROR:");
    // Named before it is pushed away, so "the first line left the viewport"
    // can never be satisfied by a first line that was never drawn.
    app.contains("TOP_OF_TRANSCRIPT");
    // Each refused turn adds a user block and an error block, so the viewport
    // fills in a handful; the rest of the budget is slack for a host that
    // renders either one shorter.
    for line in 0..12 {
        if !app.screen.screen().contents().contains("TOP_OF_TRANSCRIPT") {
            break;
        }
        let marker = format!("filler {line:02}");
        app.send(format!("{marker}\r").as_bytes());
        // **The submitted turn, and then the end of it.** Typing while a
        // turn runs is a feature — it fills the draft — and Enter is ignored
        // there, so an Enter that lands mid-turn is dropped and its text
        // stays in the composer. Waiting for the marker *anywhere* on screen
        // is satisfied by that draft, which is how this loop used to
        // "succeed" twelve times over a transcript that was still two turns
        // tall. Measured on the Windows ARM64 VM, 2026-09-11: the composer
        // held `filler 01filler 02…filler 08` and the loop timed out on
        // `filler 09` only because the draft line had passed 80 columns.
        // Unix never showed it because a refused loopback connection there
        // ends before the next key is sent.
        app.contains(&format!("┃ {marker}"));
        app.wait("the turn ends before the next Enter", |screen| {
            !screen.contents().contains("thinking")
        });
        app.settle(40);
    }
    app.wait("the first line leaves the viewport", |screen| {
        !screen.contents().contains("TOP_OF_TRANSCRIPT")
    });
    for _ in 0..30 {
        app.send_split_mouse_report(b"[<64;10;5M");
        app.settle(20);
        if app.screen.screen().contents().contains("TOP_OF_TRANSCRIPT") {
            break;
        }
    }
    app.contains("TOP_OF_TRANSCRIPT");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// The report must not reach the composer, and the text typed after it must
/// arrive alone. Smoke only, for the reason in `send_split_mouse_report`: this
/// test was green on every local run of the timing-based reassembler and red
/// on a loaded CI runner, where the composer held
/// `[<65;101;28MWHEEL_INPUT_OK`.
#[test]
fn fragmented_mouse_reports_do_not_become_prompt_text() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.send_split_mouse_report(b"[<65;101;28M");
    app.send(b"WHEEL_INPUT_OK\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        request["messages"][0]["content"][0]["text"],
        "WHEEL_INPUT_OK"
    );
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn handlers_can_be_inspected_and_cancelled_during_an_active_task() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (waiting, requests) = mpsc::channel();
    let (release, allowed) = mpsc::channel();
    thread::spawn(move || {
        for turn in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            if turn == 1 {
                waiting.send(()).unwrap();
                allowed.recv_timeout(Duration::from_secs(15)).unwrap();
            }
            let text = if turn == 0 {
                "```sterna\nconst noise = on({}, 'batch.ack(batch.rest().map(e => e.id));');\n```"
            } else {
                "```sterna\nanswer('HANDLER CONTROL DONE');\n```"
            };
            let events = [
                serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":20}}}),
                serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":text}}),
                serde_json::json!({"type":"message_delta","usage":{"output_tokens":12}}),
                serde_json::json!({"type":"message_stop"}),
            ];
            let body = events
                .iter()
                .map(|e| format!("data: {e}\n\n"))
                .collect::<String>();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let mut app = App::start(&base);
    app.ready();
    app.send(b"register noise handler\r");
    requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.send(b"/handlers\r");
    app.contains("STANDING HANDLERS");
    app.contains("noise");
    app.contains("active");
    app.send(b"\x1b");
    app.wait("handler panel closed", |screen| {
        !screen.contents().contains("STANDING HANDLERS")
    });
    app.send(b"/handlers off noise\r");
    app.contains("cancellation queued");
    app.send(b"/handlers\r");
    app.contains("STANDING HANDLERS");
    app.contains("noise");
    app.resize(40);
    app.contains("STANDING HANDLERS");
    release.send(()).unwrap();
    // Keep the panel open across task completion, including on a narrow
    // terminal. Reopening it would conceal a stale snapshot regression.
    app.contains("No handlers in this task");
    app.send(b"\x1b");
    app.wait("completed handler panel closed", |screen| {
        !screen.contents().contains("STANDING HANDLERS")
    });
    app.resize(80);
    app.contains("HANDLER CONTROL DONE");
    app.send(b"/handles\r");
    app.contains("LAST HANDLE PREVIEW");
    app.contains("stale");
    app.send(b"\x1b");
    app.wait("handle panel closed", |screen| {
        !screen.contents().contains("LAST HANDLE PREVIEW")
    });
    // With nothing standing, the answer is one line: a notice, not a sheet,
    // so there is nothing to close before the next command.
    app.send(b"/handlers\r");
    app.contains("No handlers in this task");
    app.settle(120);
    assert!(!app.screen.screen().contents().contains("STANDING HANDLERS"));
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn settings_tabs_name_their_destinations_and_escape_creates_nothing() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.resize(160);
    // The editor names the file the store will write, in the store's own
    // spelling: on Windows that is the canonical long-name form of a temp
    // root `temp_dir()` may hand out as an 8.3 short name, with native
    // separators. Ask the store rather than join on the raw root.
    let store = sterna::settings::Store::with_global(
        &app.root,
        Some(app.root.join("global-config").join("sterna")),
    )
    .unwrap();
    let project = store.path(sterna::settings::Scope::Local);
    let global = store.path(sterna::settings::Scope::Global);

    app.send(b"/settings\r");
    // **The surface's own heading, not the control that opened it.** The
    // session bar's `[ Settings ]` is gone the moment the surface is drawn,
    // so a probe for the mixed-case spelling was really a race against the
    // frame before it -- one the Unix stream happened to win and a ConPTY,
    // which coalesces the redraw, lost every time. Its sibling below already
    // waits for the heading.
    app.contains("SETTINGS");
    app.contains("Global");
    app.contains("Project");
    // It opens on Global (decision 6): a project file is written only when
    // Project is chosen here.
    app.contains("Your settings, for every project");
    app.contains("config.toml");
    // Both values are chips, and the one in force carries its mark.
    app.contains("⟨ Off");
    app.contains("⟨ On");
    app.contains(" ● ⟩");
    assert!(
        !app.screen
            .screen()
            .contents()
            .contains(&project.display().to_string()),
        "the full path is not on the sheet"
    );

    // F6 switches scope without saving. The destination shown must switch
    // with the selected tab, so a Global label cannot conceal a Project
    // write (or vice versa). The platform's own separator:
    // `.sterna\config.toml` on Windows.
    app.send(b"\x1b[17~");
    app.contains("This project only · .sterna");
    app.send(b"\x1b");
    app.wait("settings editor closes", |screen| {
        !screen.contents().contains("This project only")
    });
    assert!(!project.exists(), "viewing/cancelling created {project:?}");
    assert!(!global.exists(), "viewing/cancelling created {global:?}");

    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn bare_statusline_opens_its_row_and_the_shortcut_saves_globally() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    let project = app.root.join(".sterna/config.toml");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    let original = "# retained on cancel\n[ui]\ntheme = \"amber\"\n";
    std::fs::write(&project, original).unwrap();

    // Opening the editor and closing it again writes nothing: viewing is
    // not an edit, which is the half of the old preview contract that
    // survives direct save.
    app.send(b"/statusline\r");
    app.contains("SETTINGS");
    app.contains("Global");
    app.contains("Project");
    app.send(b"\x1b");
    app.wait("settings editor closes", |screen| {
        !screen.contents().contains("SETTINGS")
    });
    assert_eq!(
        std::fs::read_to_string(&project).unwrap(),
        original,
        "opening and closing the editor changed the settings file"
    );

    // A completed choice saves itself. `/statusline compact` is the same
    // save by its shortest route.
    app.send(b"/statusline compact\r");
    app.contains("Status line saved: compact");
    let saved = std::fs::read_to_string(app.global_settings())
        .expect("the shortcut writes the global settings");
    assert!(saved.contains("statusline = \"compact\""), "{saved}");
    assert_eq!(
        std::fs::read_to_string(&project).unwrap(),
        original,
        "the project file is written only when Project is chosen"
    );

    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn statusline_compact_shortcut_persists_without_contacting_inference() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();

    app.send(b"/statusline compact\r");
    app.contains("Status line saved: compact");
    let saved = std::fs::read_to_string(app.global_settings())
        .expect("shortcut writes the global settings");
    assert!(saved.contains("statusline = \"compact\""), "{saved}");

    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn typed_newlines_compose_one_message_and_a_lone_enter_still_sends_it() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    // No bracketed-paste markers, which is exactly what another program
    // typing into the pty produces. The separator is `\r`, not `\n`: in raw
    // mode crossterm reads `\n` as Ctrl+J and only `\r` as Enter, so `\r` is
    // what a typed newline actually is here -- and what silently submitted
    // "first line" as the whole task on 2026-09-19, leaving the rest in the
    // composer with nobody told about it.
    app.send(b"first line\rsecond line\rthird line");
    app.contains("third line");
    let screen = app.screen.screen().contents();
    assert!(
        screen.contains("first line") && screen.contains("second line"),
        "a typed newline submitted part of the payload:\n{screen}"
    );
    // The fixture provider hands every request to the channel in order, so
    // the request read below is the first thing this session ever sent --
    // proof that neither newline sent anything on its own.
    app.send(b"\r");
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let sent = serde_json::to_string(&request["messages"]).unwrap();
    for line in ["first line", "second line", "third line"] {
        assert!(
            sent.contains(line),
            "`{line}` never reached the model, so a lone Enter no longer sends:\n{sent}"
        );
    }
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"\x15/exit\r");
    assert_eq!(app.exited(), 0);
}

/// A cell that finishes the task, and one that does not -- the second is
/// what a test needs to prove that a turn stopped rather than simply ran
/// out of work to do.
const ANSWERING: &str = "```sterna\nanswer(\"done\");\n```";
const UNFINISHED: &str = "```sterna\n1 + 1;\n```";

/// Answers every request in turn and holds the first until it is released,
/// so a test can type into a session that is provably still working.
///
/// `held_provider` accepts one connection and returns; a queue is only a
/// queue if something comes after it, so this one keeps serving.
fn serving_provider(
    program: &'static str,
) -> (String, mpsc::Receiver<serde_json::Value>, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (request_sender, requests) = mpsc::channel();
    let (release, held) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut first = true;
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    len = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            request_sender.send(request.clone()).unwrap();
            if first {
                first = false;
                if held.recv().is_err() {
                    return;
                }
            }
            let (content_type, body) = if request["stream"] == true {
                let events = [
                    serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":12}}}),
                    serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":program}}),
                    serde_json::json!({"type":"message_delta","usage":{"output_tokens":8}}),
                    serde_json::json!({"type":"message_stop"}),
                ];
                (
                    "text/event-stream",
                    events
                        .iter()
                        .map(|event| format!("data: {event}\n\n"))
                        .collect::<String>(),
                )
            } else {
                (
                    "application/json",
                    serde_json::json!({"role":"assistant","content":[{"type":"text","text":program}]})
                        .to_string(),
                )
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, requests, release)
}

/// Everything a request's conversation said, flattened, so a test can ask
/// whether a message reached the model without knowing the block shape.
fn said(request: &serde_json::Value) -> String {
    request["messages"].to_string()
}

#[test]
fn live_a_message_sent_while_working_is_queued_and_becomes_the_next_task() {
    let (base, requests, release) = serving_provider(ANSWERING);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"first task\r");
    let first = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(said(&first).contains("first task"), "{first}");

    // The session is provably inside the held request, so this Enter lands
    // while a task is running -- the case that used to answer with a notice
    // and keep the text as a draft nobody was told about again.
    app.send(b"steer me instead\r");
    app.wait("the queued message is shown over the composer", |screen| {
        let text = screen.contents();
        text.contains("QUEUED") && text.contains("steer me instead")
    });

    let _ = release.send(());
    let second = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        said(&second).contains("steer me instead"),
        "the queued message never became a task: {second}"
    );
    app.wait(
        "the queue empties when its message becomes the task",
        |screen| !screen.contents().contains("QUEUED"),
    );

    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// A bare `--resume` opens the newest session with the resume sheet over it:
/// a normal sheet, with the conversation behind it, listing this folder's
/// sessions by what was asked in them. Choosing another ends this session
/// and starts that one in the same terminal. A session's `.events.jsonl` is
/// not a session.
#[test]
fn a_bare_resume_opens_the_resume_sheet_inside_the_session() {
    let seed = |root: &std::path::Path| {
        let sessions = root.join(".sterna/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let turn = |text: &str| {
            format!(
                "{{\"kind\":\"turn\",\"session_id\":\"x\",\"turn\":1,\"role\":\"user\",\"text\":\"{text}\",\"blocks\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}\n"
            )
        };
        std::fs::write(
            sessions.join("tlaaaa-1.jsonl"),
            turn("build a habit tracker"),
        )
        .unwrap();
        std::fs::write(sessions.join("tlaaaa-1.events.jsonl"), "{}\n").unwrap();
        thread::sleep(Duration::from_millis(1100));
        std::fs::write(sessions.join("tlbbbb-2.jsonl"), turn("fix the flaky test")).unwrap();
    };
    let mut app = App::start_seeded("http://127.0.0.1:1", false, None, &["--resume"], &seed);
    app.contains("RESUME A SESSION");
    app.contains("this session · 1 prompt");
    app.contains("fix the flaky test");
    app.contains("build a habit tracker ›");
    assert!(
        !app.screen.screen().contents().contains(".events"),
        "an event log is listed as a session"
    );
    // The sheet opens on the session running now; the other is one down.
    app.send(b"\x1b[B\r");
    app.wait("the chosen session is the one running", |screen| {
        let text = screen.contents();
        text.contains("STERNA /")
            && text.contains("build a habit tracker")
            && !text.contains("fix the flaky test")
    });
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
    let bytes = String::from_utf8_lossy(&app.bytes).into_owned();
    assert!(
        bytes.contains("resume it with:  sterna --resume tlaaaa-1"),
        "the last session is the one to come back to"
    );
}

/// A session nobody asked anything in is not kept: no file stays behind
/// and no line says how to come back to it.
#[test]
fn an_empty_session_leaves_no_file_and_no_resume_line() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.send(b"\x04");
    assert_eq!(app.exited(), 0);
    let bytes = String::from_utf8_lossy(&app.bytes).into_owned();
    assert!(!bytes.contains("resume it with"), "{bytes}");
    let sessions = app.root.join(".sterna/sessions");
    let left: Vec<_> = std::fs::read_dir(&sessions)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| name.to_string_lossy().ends_with(".jsonl"))
                .filter(|name| !name.to_string_lossy().contains(".gateway"))
                .collect()
        })
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

/// An Escape whose task answered before its cell boundary was never read,
/// and it stopped the next task 56 ms after the person sent it -- before a
/// single request left. The next task must start clean and reach the model.
#[test]
fn live_a_stop_left_over_from_a_finished_task_does_not_stop_the_next() {
    let (base, requests, release) = serving_provider(ANSWERING);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"first task\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.send(b"\x1b");
    app.wait("the first Escape asks for the gentle stop", |screen| {
        screen.contents().contains("Stopping after this cell")
    });
    let _ = release.send(());
    app.settle(1500);
    app.send(b"the next task\r");
    let next = requests
        .recv_timeout(Duration::from_secs(10))
        .expect("the next task never reached the model");
    assert!(said(&next).contains("the next task"), "{next}");
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

#[test]
fn live_escape_ends_the_turn_at_the_cell_boundary_with_no_further_model_turn() {
    // A cell that answers nothing, so the task would keep taking turns for
    // as long as it is allowed to: what ends this turn is the Escape, and
    // a test that let the task finish on its own would prove nothing.
    let (base, requests, release) = serving_provider(UNFINISHED);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"a task to stop\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();

    app.send(b"\x1b");
    app.wait("the first Escape asks for the gentle stop", |screen| {
        screen.contents().contains("Stopping after this cell")
    });

    // Released, so the held turn completes normally and its cell runs. The
    // stop is read at the boundary after it, which is the whole contract:
    // nothing in flight is destroyed and nothing further is sent.
    let _ = release.send(());
    assert!(
        requests.recv_timeout(Duration::from_secs(6)).is_err(),
        "a turn was sent after the person stopped the task"
    );
    // The turn has ended, and said so: a bare `2` would match the session
    // id in the header before anything ran.
    app.wait(
        "the cell that was in flight ran and the turn stopped",
        |screen| screen.contents().contains("stopped · what ran stands"),
    );
    assert!(
        app.child.try_wait().unwrap().is_none(),
        "stopping a turn ended the session"
    );

    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// Two Ctrl-C read in one go -- a quick double tap, or a terminal that
/// sends both at once -- are two presses, and two presses end the session.
#[test]
fn two_ctrl_c_read_together_end_the_session() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.send(b"\x03\x03");
    assert_eq!(app.exited(), 130);
}

#[test]
fn live_a_second_escape_escalates_to_the_call_in_flight_and_still_spares_the_session() {
    let (base, requests, release) = serving_provider(UNFINISHED);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"a task to interrupt\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();

    app.send(b"\x1b");
    app.wait("the gentle rung", |screen| {
        screen.contents().contains("Stopping after this cell")
    });
    app.send(b"\x1b");
    app.wait("the abrupt rung", |screen| {
        screen.contents().contains("Cancelling the call in flight")
    });

    // **Two Escapes are not two Ctrl-Cs.** Ctrl-C twice ends the session on
    // purpose; Escape twice must leave it standing, or the gentle rung is a
    // trap rather than the first step of a ladder.
    thread::sleep(Duration::from_millis(400));
    assert!(
        app.child.try_wait().unwrap().is_none(),
        "a second Escape ended the session"
    );
    // **And the call really is given up on.** The provider still holds the
    // first turn and never answers it, so the only way the task stops
    // thinking is that it stopped waiting (2026-09-23: it did not, and the
    // Escape printed "Cancelling" seven times while the turn went on).
    app.wait(
        "the cancelled turn ends while the provider still holds it",
        |screen| !screen.contents().contains("thinking"),
    );
    drop(release);

    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// A sheet is a local control, not a model turn: opening and closing one
/// starts no turn clock and ends in no "complete".
#[test]
fn a_sheet_opened_and_closed_is_not_a_turn() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    // Answered by the session, not the screen: the path a model turn takes.
    app.send(b"/login\r");
    app.contains("SIGN IN");
    app.send(b"\x1b");
    app.settle(500);
    app.refute("a sheet is not a turn that completed", "complete");
    app.refute("a sheet starts no turn clock", "on this turn");
    app.refute("a sheet is not a turn the model is on", "thinking");
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// A turn the person stopped reads "stopped", never "complete".
#[test]
fn a_stopped_turn_says_stopped_not_complete() {
    let (base, requests, release) = serving_provider(UNFINISHED);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"a task to stop\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.send(b"\x1b");
    app.contains("Stopping after this cell");
    app.send(b"\x1b");
    app.contains("stopped");
    app.refute("a stopped turn is not a completed one", "complete");
    drop(release);
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// A message sent while a turn runs is held in Sterna's queue, not sent:
/// Esc takes it back into the composer and it never reaches the model.
#[test]
fn a_queued_message_is_held_until_the_turn_ends_and_esc_takes_it_back() {
    let (base, requests, release) = serving_provider(ANSWERING);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"first task\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.send(b"not after all\r");
    app.contains("Esc takes the last one back");
    app.send(b"\x1b");
    app.contains("Took the queued message back");
    let _ = release.send(());
    assert!(
        requests.recv_timeout(Duration::from_secs(3)).is_err(),
        "a message taken back was sent to the model"
    );
    app.contains("not after all");
    // The first Ctrl-C clears the draft that was taken back.
    app.send(b"\x03");
    app.settle(200);
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// Ctrl-C mid-turn says what it does, as Escape does, and the turn ends as
/// stopped by it.
#[test]
fn ctrl_c_mid_turn_says_it_is_stopping() {
    let (base, requests, release) = serving_provider(UNFINISHED);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"a task to interrupt\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.send(b"\x03");
    app.contains("Ctrl-C again within 2 s quits");
    app.contains("stopped by Ctrl-C");
    drop(release);
    thread::sleep(Duration::from_millis(2200));
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// /exit is honoured while a turn runs: the turn is stopped and the
/// session ends.
#[test]
fn exit_mid_turn_stops_the_turn_and_ends_the_session() {
    let (base, requests, _release) = serving_provider(UNFINISHED);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"a task to leave\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// The terminal is asked to tell Shift-Enter from Enter, and the request is
/// taken back on the way out. Windows reads console records, which carry
/// the modifiers already, so nothing is asked for there.
#[cfg(not(windows))]
#[test]
fn the_keyboard_protocol_is_asked_for_and_given_back() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    assert!(emitted(&app.bytes, b"\x1b[>1u"), "the flags were pushed");
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
    assert!(emitted(&app.bytes, b"\x1b[<1u"), "the flags were popped");
}

/// Ctrl-C clears a draft and says Ctrl-Z brings it back; on an empty
/// composer it says a second one quits, and the notice lapses with the
/// window.
#[test]
fn ctrl_c_says_what_it_did_to_a_draft_and_what_a_second_one_does() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.send(b"a careful draft");
    app.contains("a careful draft");
    app.send(b"\x03");
    app.contains("Draft cleared · Ctrl-Z brings it back");
    app.send(b"\x1a");
    app.contains("a careful draft");
    app.send(b"\x15");
    app.send(b"\x03");
    app.contains("Ctrl-C again within 2 s to quit");
    thread::sleep(Duration::from_millis(2300));
    app.refute(
        "the quit notice lapses with its window",
        "within 2 s to quit",
    );
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// Ctrl-D while a turn runs says what it would do and when.
#[test]
fn ctrl_d_mid_turn_says_it_waits_for_the_turn() {
    let (base, requests, _release) = serving_provider(UNFINISHED);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"a long task\r");
    let _ = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.send(b"\x04");
    app.contains("Ctrl-D quits between turns");
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// A confirmed rollback marks the cell it undid, and says so in one line.
#[test]
fn a_rollback_marks_its_cell_and_says_so_in_one_line() {
    let base =
        approval_provider(r#"await write({path: "made.txt", content: "x"}); return "done";"#);
    let mut app = App::start(&base);
    app.ready();
    app.send(b"make a file\r");
    app.contains("EXECUTED");
    assert!(
        app.root.join("made.txt").exists(),
        "the cell wrote its file"
    );
    app.send(b"/rollback\r");
    app.contains("Confirm rollback");
    app.settle(600);
    app.send(b"\x1b[A");
    // A decision sheet takes a key only after half a second without one.
    app.settle(700);
    app.send(b"\r");
    app.contains("Rolled back cell 001");
    app.contains("ROLLED BACK");
    assert!(!app.root.join("made.txt").exists(), "the file is gone");
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// Esc puts the command popup away and a second Esc clears the slash word;
/// Enter on a command typed in full runs that command, not a longer one.
#[test]
fn esc_puts_the_popup_away_and_enter_runs_the_exact_command() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.send(b"/mo");
    app.contains("browse models");
    app.send(b"\x1b");
    app.refute("the popup is put away", "browse models");
    app.send(b"\x1b");
    app.contains("Describe the next step");
    // `/cells` is listed before `/cell`, and the name typed in full wins.
    app.send(b"/cell\r");
    app.contains("No cell has run yet.");
    app.settle(200);
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

/// After an `@` popup is put away, a second Esc takes back the `@` word it
/// was for and nothing else the person typed.
#[test]
fn a_second_escape_after_a_path_popup_keeps_the_rest_of_the_draft() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    std::fs::write(app.root.join("notes-for-escape.md"), "notes").unwrap();
    app.send(b"look at @notes-for");
    app.contains("notes-for-escape.md");
    app.send(b"\x1b");
    app.refute("the popup is put away", "notes-for-escape.md");
    app.send(b"\x1b");
    app.refute("the @ word is taken back", "@notes-for");
    app.contains("look at");
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    assert_eq!(app.exited(), 130);
}

#[test]
fn workbench_settings_save_directly_and_do_not_consume_the_draft() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.contains("⠿ STERNA");
    app.send(b"keep this draft");
    app.send(b"\x1bOQ"); // F2
    app.contains("SETTINGS");
    app.send(b"\t");
    app.contains("Display");
    app.send(b"\x1b[C"); // theme advances, no Apply step
    app.contains("Theme is now");
    let saved = std::fs::read_to_string(app.global_settings()).unwrap();
    assert!(saved.contains("amber"), "{saved}");
    app.send(b"\x1b");
    app.contains("keep this draft");
    app.send(b"\x15/exit\r");
    assert_eq!(app.exited(), 0);
}

/// The headline of the settings rework, proved against a real terminal: a
/// choice made on the panel is in force in the session that is already
/// running, not in the next one.
///
/// **This is the defect the whole pass started from.** Sixty-six of the
/// seventy-one keys said `restart` and the panel applied four of them, so
/// changing your reasoning effort on the settings screen printed *"Saved for
/// a new session"* while typing `/effort high` two lines lower changed it
/// immediately. The control commands were always there; the panel had no
/// caller for them. This walks the screen, not the code: effort is stepped
/// on the panel, and the status strip -- which reads the live session, never
/// the file -- is what has to agree.
#[test]
fn a_setting_chosen_on_the_panel_is_in_force_in_this_session() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.send(b"\x1bOQ"); // F2 opens settings on the everyday category
    app.contains("SETTINGS");
    // Reasoning effort is the second row; one Down and one Right is the
    // whole gesture, and there is no Apply.
    app.send(b"\x1b[B");
    app.settle(60);
    app.send(b"\x1b[C");
    app.contains("Effort is now low");
    app.send(b"\x1b");
    // The strip reads the running session. If the choice had only reached
    // the file, this would still say `default`.
    app.contains("effort low");
    // And it reached the file too, so the next session starts there.
    let saved = std::fs::read_to_string(app.global_settings()).unwrap();
    assert!(saved.contains("low"), "{saved}");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn workbench_final_answer_survives_the_actual_provider_and_terminal_loop() {
    let (base, requests) = provider();
    let mut app = App::start(&base);
    app.contains("⠿ STERNA");
    app.send(b"Return the fixture answer.\r");
    requests.recv_timeout(Duration::from_secs(10)).unwrap();
    app.contains_line("LIVE RESULT INTACT");
    app.send(b"/diff\r");
    // The expanded cell's tabs, which only the command draws. "before"
    // was also the completion hint's word ("before/after diff"), so the
    // wait could end while `/diff` was still being typed, and the `/exit`
    // behind it turned its Enter into a new line of the same draft.
    app.contains("Cell program");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Ctrl-C over a selection copies it, the way a terminal's own copy key
/// does: pressed twice it would otherwise have ended the session.
#[test]
fn ctrl_c_over_a_selection_copies_and_interrupts_nothing() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    // A note to select, not a task: a turn in flight would make the `/exit`
    // below an Enter a running turn ignores, and on Windows a refused
    // connection is slow enough to still be in flight.
    app.send(b"/theme amber\r");
    app.contains("Theme: amber");
    app.settle(300);
    let y = app
        .screen
        .screen()
        .rows(0, 80)
        .position(|row| row.contains("Theme: amber"))
        .unwrap()
        + 1;
    app.send(format!("\x1b[<0;4;{y}M").as_bytes());
    app.send(format!("\x1b[<32;30;{y}M").as_bytes());
    app.settle(200);
    app.send(format!("\x1b[<0;30;{y}m").as_bytes());
    app.contains("Copied selection.");
    app.send(b"\x03");
    thread::sleep(Duration::from_millis(100));
    app.send(b"\x03");
    app.settle(300);
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// A typed level is set like every other route that sets it: saved, and
/// offered back beside its notice, where a click takes it back.
#[test]
fn a_typed_level_offers_the_one_it_left_back() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.ready();
    app.contains("⟨ ◼ Sandboxed ⟩");
    app.send(b"/sandbox ask\r");
    app.contains("undo · Sandbox Sandboxed");
    app.contains("⟨ ◼ Ask ⟩");
    let rows: Vec<_> = app.screen.screen().rows(0, 80).collect();
    let (y, row) = rows
        .iter()
        .enumerate()
        .find(|(_, row)| row.contains("undo · Sandbox Sandboxed"))
        .unwrap();
    let x = row
        .char_indices()
        .position(|(byte, _)| row[byte..].starts_with("undo"))
        .unwrap()
        + 1;
    let y = y + 1;
    app.send(format!("\x1b[<0;{x};{y}M").as_bytes());
    app.send(format!("\x1b[<0;{x};{y}m").as_bytes());
    app.contains("⟨ ◼ Sandboxed ⟩");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn workbench_pointer_opens_settings_only_on_release_and_wheel_stays_local() {
    let (base, _requests) = provider();
    let mut app = App::start(&base);
    app.contains("Settings");
    // Header is one row; the chip's column is counted in cells, not bytes,
    // because the glyphs before it are more than one byte each.
    let rows: Vec<_> = app.screen.screen().rows(0, 80).collect();
    let x = rows[0]
        .char_indices()
        .position(|(byte, _)| rows[0][byte..].starts_with("Settings"))
        .unwrap()
        + 1;
    app.send(format!("\x1b[<0;{x};1M").as_bytes());
    app.settle(120);
    assert!(!app.screen.screen().contents().contains("SETTINGS"));
    app.send(format!("\x1b[<0;{x};1m").as_bytes());
    app.contains("SETTINGS");
    app.send(b"\x1b[<65;30;12M");
    app.settle(120);
    assert!(app.screen.screen().contents().contains("SETTINGS"));
    app.send(b"\x1b");
    app.contains("⠿ STERNA");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Serves a task turn slowly -- the model's prose first, then a cell that
/// runs for a moment and answers -- and holds every helper request that
/// arrives after it (the fresh checker behind the answer) until released.
fn motion_provider() -> (String, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (release, held) = mpsc::channel::<()>();
    let held = std::sync::Arc::new(std::sync::Mutex::new(held));
    thread::spawn(move || {
        let answered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { return };
            let (answered, held) = (answered.clone(), held.clone());
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        len = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; len];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let helper = request["model"] == "helper-tier";
                let pieces: Vec<(&str, u64)> = if helper {
                    if answered.load(std::sync::atomic::Ordering::SeqCst) {
                        let _ = held.lock().unwrap().recv();
                    }
                    vec![("holds\nThe run returned the value the answer claims.", 0)]
                } else {
                    answered.store(true, std::sync::atomic::Ordering::SeqCst);
                    vec![
                        ("Reading the motion guard first; ", 500),
                        ("the check is cheap, so I will run it ", 500),
                        ("and report what it says.\n\n", 500),
                        (
                            "```sterna\nconst t = Date.now();\nwhile (Date.now() - t < 1800) {}\nanswer(\"The guard holds: 3 of 3 cases pass.\");\n```",
                            0,
                        ),
                    ]
                };
                if request["stream"] != true {
                    let text: String = pieces.iter().map(|(t, _)| *t).collect();
                    let body = serde_json::json!({"role":"assistant","content":[{"type":"text","text":text}],"usage":{"input_tokens":20,"output_tokens":9}}).to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    return;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                );
                let event = |stream: &mut std::net::TcpStream, value: serde_json::Value| {
                    let _ = write!(stream, "data: {value}\n\n");
                    let _ = stream.flush();
                };
                event(
                    &mut stream,
                    serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":20}}}),
                );
                for (text, pause) in pieces {
                    event(
                        &mut stream,
                        serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":text}}),
                    );
                    thread::sleep(Duration::from_millis(pause));
                }
                event(
                    &mut stream,
                    serde_json::json!({"type":"message_delta","usage":{"output_tokens":9}}),
                );
                event(&mut stream, serde_json::json!({"type":"message_stop"}));
            });
        }
    });
    (base, release)
}

/// One turn through a real terminal: the model's prose arrives under a
/// live rail, the cell runs, the answer lands, and the check behind the
/// answer is visible while it runs and settles into its verdict. The frames
/// are printed (`--nocapture`) so the look can be read, not only asserted.
fn walk_a_turn(bird: bool) {
    let (base, release) = motion_provider();
    let colours: &[(&str, &str)] = if bird {
        &[("COLORTERM", "truecolor")]
    } else {
        &[]
    };
    let mut app = App::start_in(&base, false, Some("helper-tier"), &[], &|_| {}, colours);
    app.ready();
    let frame = |app: &mut App, name: &str| {
        // A whole frame, not one the pty is still delivering.
        app.settle(90);
        eprintln!(
            "--- {} · {name} ---\n{}",
            if bird { "parrot" } else { "classic" },
            app.screen.screen().contents()
        );
    };
    if bird {
        app.send(b"/theme amazon\r");
        app.contains("Theme: amazon");
    }
    app.settle(300);
    frame(&mut app, "idle");
    app.send(b"check the motion guard\r");
    app.contains("the check is cheap");
    frame(&mut app, "prose arriving");
    if !bird {
        app.contains("▎ Reading the motion guard");
    }
    app.contains("xecuting this cell");
    frame(&mut app, "cell running");
    app.contains_line("The guard holds: 3 of 3 cases pass.");
    app.contains("checking the answer");
    frame(&mut app, "answer landed, check behind it");
    release.send(()).unwrap();
    app.contains("checked after the answer: holds");
    app.refute(
        "the working row goes when its verdict lands",
        "checking the answer",
    );
    app.settle(1200);
    frame(&mut app, "verdict settled");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

#[test]
fn the_instrument_moves_where_attention_is_and_the_check_lands_behind_the_answer() {
    walk_a_turn(false);
}

#[test]
fn a_parrot_theme_walks_the_same_turn() {
    walk_a_turn(true);
}

/// **The session card names the level in force**, not the one the session
/// began on: the card's own line moves with `/sandbox`.
#[test]
fn the_card_names_the_level_in_force() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.contains("Sandbox: Sandboxed");
    app.send(b"/sandbox ask\r");
    app.contains("Sandbox: Ask");
    app.refute("the card moved", "Sandbox: Sandboxed");
}

/// **Every effort level is open on any model**: the word rides the request
/// and the gateway carries it, so `xhigh` and `max` are not refused for a
/// model that is not Claude -- a refusal there left the strip's effort chip
/// stuck and printing one refusal per click.
#[test]
fn xhigh_and_max_are_taken_on_a_model_that_is_not_claude() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    app.send(b"/effort xhigh\r");
    app.contains("Effort is now xhigh");
    app.send(b"/effort max\r");
    app.contains("Effort is now max");
}

/// **Leaving with Ctrl-D says how to come back**, after the screen is gone:
/// the line used to go to the stopped screen and was lost.
#[test]
fn ctrl_d_leaves_the_resume_line_in_the_terminal() {
    let mut app = App::start("http://127.0.0.1:1");
    app.ready();
    // Something asked, so there is something to come back to.
    app.send(b"hello\r");
    app.contains("ERROR:");
    app.send(b"\x04");
    assert_eq!(app.exited(), 0);
    assert!(!app.screen.screen().alternate_screen());
    let bytes = String::from_utf8_lossy(&app.bytes).into_owned();
    let after = bytes.rsplit("\x1b[?1049l").next().unwrap_or_default();
    assert!(
        after.contains("resume it with:  sterna --resume"),
        "no resume line after the screen closed:\n{after}"
    );
    assert!(
        after.contains("sterna: ended by Ctrl-D on an empty prompt"),
        "the exit did not say what ended it:\n{after}"
    );
}

/// **A true-colour terminal starts with a parrot** when no theme was chosen:
/// the plumage is the palette and the bird perches on the card in colour.
#[test]
fn a_true_colour_terminal_starts_with_a_parrot_perched_on_the_card() {
    let mut app = App::start_in(
        "http://127.0.0.1:1",
        false,
        None,
        &[],
        &|_| {},
        &[("COLORTERM", "truecolor")],
    );
    app.ready();
    app.wait("the parrot's half-block sprite on the card", |screen| {
        let text = screen.contents();
        text.contains('▀') || text.contains('▄')
    });
}

/// A terminal that answers that its background is white gets the light
/// palette: the person's label is drawn in the light "you" colour, and no
/// part of the answer reaches the composer as typed text. (A Windows
/// console is not asked; `COLORFGBG` or the setting decides there.)
#[cfg(unix)]
#[test]
fn a_terminal_that_answers_light_gets_the_light_colours() {
    let (base, _requests) = provider();
    let mut app = App::start_in(
        &base,
        false,
        None,
        &[],
        &|_| {},
        &[("COLORTERM", "truecolor")],
    );
    app.ground = Some("rgb:ffff/ffff/ffff");
    app.ready();
    app.send(b"hello there\r");
    app.wait("the person's label in the light colour", |screen| {
        (0..30).any(|row| {
            (0..80).any(|col| {
                screen.cell(row, col).is_some_and(|cell| {
                    cell.contents() == "y" && cell.fgcolor() == vt100::Color::Rgb(0x5b, 0x3f, 0xb5)
                })
            })
        })
    });
    app.refute("the reply is not typed", "rgb:ffff");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}

/// Answers the acceptance lister with a two-item list and every task turn
/// with one cell that writes the file the list names and answers.
fn acceptance_provider() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    len = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                continue;
            };
            let text = if request.to_string().contains("acceptance items") {
                "file: done.txt exists\njudge: the note says hello"
            } else {
                "```sterna\nawait write({path: \"done.txt\", content: \"hello\\n\"});\nanswer(\"WROTE THE NOTE\");\n```"
            };
            let reply = if request["stream"] == true {
                let events = [
                    serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":10}}}),
                    serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":text}}),
                    serde_json::json!({"type":"message_delta","usage":{"output_tokens":5}}),
                    serde_json::json!({"type":"message_stop"}),
                ];
                let body = events
                    .iter()
                    .map(|e| format!("data: {e}\n\n"))
                    .collect::<String>();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                let body = serde_json::json!({"role":"assistant","content":[{"type":"text","text":text}],"usage":{"input_tokens":10,"output_tokens":5}}).to_string();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    base
}

/// The acceptance list reaches the screen from a real session: the lister's
/// items stand open before the work, and the file item turns met once the
/// cell has written it -- counted on the chip an eighty-column terminal
/// shows in place of the sidebar.
#[test]
fn live_acceptance_list_counts_the_file_the_cell_wrote() {
    let base = acceptance_provider();
    let mut app = App::start_seeded(&base, false, None, &[], &|root| {
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        std::fs::write(
            root.join(".sterna/config.toml"),
            "[helpers]\nmodel = \"helper-tier\"\npreflight = false\nacceptance_list = true\n\
             completion_check = false\nlearn = false\n",
        )
        .unwrap();
    });
    app.ready();
    app.send(b"write done.txt with a greeting\r");
    app.wait_for_file("done.txt");
    app.contains_line("WROTE THE NOTE");
    app.contains("✓ 1 of 2 met");
    app.send(b"/exit\r");
    assert_eq!(app.exited(), 0);
}
