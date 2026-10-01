use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// The longest tail an SGR mouse report can have: `[<`, three fields wide
/// enough for `u16::MAX`, two separators and the terminator, which is
/// `[<65535;65535;65535M`. Nothing longer can become a report, so nothing
/// longer is ever held — this is the bound, and it is a length rather than a
/// duration because waiting changes what a run *is* not at all.
const MAX_TAIL: usize = 20;

/// The markers a terminal wraps a paste in once `?2004h` has asked it to.
const PASTE_OPEN: &str = "[200~";
const PASTE_CLOSE: &str = "[201~";

/// How much one paste may accumulate before it is delivered regardless.
///
/// A bound rather than trust, for [`MAX_TAIL`]'s reason: a closing marker
/// that never arrives must not grow a buffer without end. It is far above
/// anything a terminal sends in one paste, and reaching it delivers what was
/// collected rather than dropping it — the rest then arrives as ordinary
/// keys, which is visible instead of silent.
const MAX_PASTE: usize = 1 << 20;

/// How long an Escape with nothing after it yet waits before it is handed to
/// the UI as a key press.
///
/// It decides one thing: whether the UI sees an Escape that belonged to a
/// report. It cannot decide whether report characters are typed, because the
/// matcher stays armed across the delivery — see [`TerminalInput::quiet`]. Too
/// short costs a stray Escape when a report is fragmented *and* slow, which
/// closes an open panel and otherwise does nothing; too long costs Escape
/// latency. Both answers are cheap, which is why a duration is allowed here
/// and nowhere else in this file.
const ESCAPE_GRACE: Duration = Duration::from_millis(20);

/// Reassembles SGR mouse reports that arrive split across terminal reads.
///
/// **The invariant: no character of an SGR mouse report is ever handed back as
/// text, however long its pieces take to arrive.** An SGR report is
/// `ESC [ < digits ; digits ; digits (M|m)`, so while the run after an Escape
/// is a strict prefix of that grammar it is unambiguously incomplete and is
/// held — with no timer, across as many reads as it takes, because no amount
/// of waiting changes what a prefix is. The moment a character arrives that
/// the grammar cannot accept, the run was never a report: it is released to
/// the editor at once, ahead of that character. A complete report is consumed
/// either way and is handed back as the same `Event::Mouse` that crossterm's
/// ordinary byte path produces. This includes button presses: provider and
/// model clicks use them, while wheel reports keep scrolling.
///
/// The predecessor made this a race: a 20 ms grace that restarted on every
/// character, with a 200 ms ceiling, and a release into the editor when either
/// expired. It was green on this machine and red on a loaded CI runner, which
/// typed `[<65;101;28M` into the composer and sent it to a model.
pub(super) struct TerminalInput {
    /// Resolved events waiting to be handed to the caller, in arrival order.
    ready: VecDeque<Event>,
    hold: Hold,
    /// Which console this input comes through, which decides what this
    /// file may still have to assemble — see [`Console`].
    console: Console,
}

/// How the bytes a terminal sends reach this file, and so what is left for
/// it to assemble.
///
/// Where crossterm parses the bytes itself (`Unix`) it consumes a paste's
/// markers before this file is asked, and a run that merely looks like an
/// opener stays the text it is. Where crossterm reads console records
/// (`Records`, Windows without raw terminal input) the console has already
/// parsed the input on Sterna's behalf: a paste's markers never arrive, and a
/// report's characters carry the keyboard layout's modifiers, so this file
/// reassembles what it can. Where the console grants raw terminal input
/// (`VtInput`, Windows with `ENABLE_VIRTUAL_TERMINAL_INPUT`) the terminal's
/// own bytes arrive as characters, and this file parses them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Console {
    Unix,
    Records,
    VtInput,
}

impl Console {
    /// The console this host reads without asking for anything.
    pub(super) fn host() -> Self {
        if cfg!(windows) {
            Self::Records
        } else {
            Self::Unix
        }
    }
}

impl Default for TerminalInput {
    fn default() -> Self {
        Self::new(Console::host())
    }
}

/// What may still be assembling. `Idle` cannot become a report; `Open` is a
/// run that still can, holding the Escape that opened it until either the
/// report completes (the Escape is never delivered at all) or the grace
/// expires (it is delivered and `escape` becomes `None`, the matcher staying
/// armed for a tail that is merely late).
#[derive(Default)]
enum Hold {
    #[default]
    Idle,
    Open {
        escape: Option<Event>,
        tail: Vec<Event>,
    },
    /// Between `ESC [ 200 ~` and `ESC [ 201 ~`: every character is payload.
    /// `closing` holds the run after an Escape while it could still be the
    /// closing marker.
    Pasting {
        text: String,
        closing: Option<String>,
    },
}

impl TerminalInput {
    pub(super) fn new(console: Console) -> Self {
        Self {
            ready: VecDeque::new(),
            hold: Hold::Idle,
            console,
        }
    }

    /// Whether `ESC [ 200 ~ … ESC [ 201 ~` is reassembled here into an
    /// `Event::Paste`: everywhere crossterm does not parse the bytes itself.
    fn reassembles_pastes(&self) -> bool {
        self.console != Console::Unix
    }

    /// Hands `event` back, to be read again first: the caller had news to
    /// take in before answering it.
    pub(super) fn put_back(&mut self, event: Event) {
        self.ready.push_front(event);
    }

    /// True while resolved events are waiting; the caller must drain them
    /// before it blocks on the terminal again.
    pub(super) fn queued(&self) -> bool {
        !self.ready.is_empty()
    }

    /// Whether another **keystroke** is already waiting — which is not the
    /// same question as whether another console *record* is.
    ///
    /// Asking `event::poll(ZERO)` directly is the wrong question on Windows,
    /// where crossterm emits a `KeyEventKind::Release` record after every
    /// press: a poll immediately after a key was read is answered by that
    /// key's own release and says "more input is waiting" when the person has
    /// stopped typing. This drains whatever the terminal already holds into
    /// the resolved queue first — where [`Self::accept`] drops releases and
    /// reassembles runs — and then reports what actually survived.
    ///
    /// It cannot spin: each turn either resolves an event, which ends the
    /// loop, or consumes one the terminal already had, and the poll is for
    /// zero duration, so it stops as soon as the terminal is empty.
    pub(super) fn typing_waiting(&mut self) -> io::Result<bool> {
        while !self.queued() && event::poll(Duration::ZERO)? {
            self.accept(event::read()?);
        }
        Ok(self.queued())
    }

    /// Read one logical event, or `None` when the bytes read so far belong to
    /// a report that is not finished. The caller loops on `None`.
    pub(super) fn read(&mut self) -> io::Result<Option<Event>> {
        if let Some(event) = self.ready.pop_front() {
            return Ok(Some(event));
        }
        self.accept(event::read()?);
        if self.ready.is_empty() && self.escape_alone() && !event::poll(ESCAPE_GRACE)? {
            self.quiet();
        }
        Ok(self.ready.pop_front())
    }

    /// The stream went silent while a run was open.
    ///
    /// Only an Escape with *nothing* after it is resolved by silence: an
    /// Escape alone is already a complete key press, so it is delivered. The
    /// matcher stays armed, because silence is not evidence — on a loaded
    /// machine the rest of a fragmented report arrives after this, and it must
    /// still be recognised rather than typed. A run that has started its tail
    /// is not resolved by silence at all.
    fn quiet(&mut self) {
        if let Hold::Open { escape, tail } = &mut self.hold
            && tail.is_empty()
        {
            self.ready.extend(escape.take());
        }
    }

    fn escape_alone(&self) -> bool {
        matches!(&self.hold, Hold::Open { escape: Some(_), tail } if tail.is_empty())
    }

    /// One character of a paste payload, or the end of one.
    ///
    /// Returns `true` when the paste completed and was pushed to `ready`.
    fn paste(&mut self, mut text: String, closing: Option<String>, event: Event) {
        // A closing marker in progress: extend it while it can still become
        // `ESC [ 201 ~`, and give its characters back to the payload when it
        // cannot. The Escape that opened the run is dropped rather than
        // typed — a terminal escapes an `ESC` inside a bracketed paste, so a
        // run that reaches here and is not the marker is not payload a
        // caller can act on, and typing it is the one outcome this file
        // exists to prevent.
        if let Some(run) = closing {
            let extended = match paste_char(&event) {
                Some(character) => format!("{run}{character}"),
                None => {
                    self.hold = Hold::Pasting {
                        text,
                        closing: Some(run),
                    };
                    return;
                }
            };
            if extended == PASTE_CLOSE {
                self.ready.push_back(Event::Paste(text));
                self.hold = Hold::Idle;
                return;
            }
            if PASTE_CLOSE.starts_with(&extended) {
                self.hold = Hold::Pasting {
                    text,
                    closing: Some(extended),
                };
                return;
            }
            text.push_str(&extended);
            self.hold = Hold::Pasting {
                text,
                closing: None,
            };
            return;
        }
        if is_plain_escape(&event) {
            self.hold = Hold::Pasting {
                text,
                closing: Some(String::new()),
            };
            return;
        }
        if let Some(character) = paste_char(&event) {
            text.push(character);
        }
        if text.len() >= MAX_PASTE {
            self.ready.push_back(Event::Paste(text));
            self.hold = Hold::Idle;
            return;
        }
        self.hold = Hold::Pasting {
            text,
            closing: None,
        };
    }

    /// Offer one event to the state machine. Everything it resolves is pushed
    /// to `ready`, in order; anything still ambiguous stays in `hold`.
    fn accept(&mut self, event: Event) {
        // **A key release is dropped here and never reaches a caller.** It
        // carries no character, so it can neither extend a run nor end one,
        // and every consumer in this crate discards it on arrival anyway --
        // `ui.rs`'s `Event::Key` arm and `workbench::input`.
        //
        // Windows is why this is load-bearing, twice. crossterm's console
        // event source maps every `bKeyDown == false` record straight to
        // `KeyEventKind::Release`, so one arrives between every two
        // characters of a report there: treating it as "not a report
        // character" released the whole run into the composer, and
        // `[<0;10;5M[<0;10;5m` was typed on screen. That was fixed by
        // stepping over a release inside an open run. What the narrower fix
        // left behind is that a release still sat in the queue between two
        // presses, and code downstream that asks "is more input already
        // waiting?" was answered yes by a key nobody pressed -- which is how
        // Enter stopped sending on Windows entirely. Dropping the release at
        // the source makes "something is waiting" mean "someone typed
        // something", which is what every caller of it meant.
        if is_key_release(&event) {
            return;
        }
        // Taken **once**: a second `take` would replace the state this one
        // just removed with `Idle` and drop an open run on the floor, which
        // is every report typed into the draft one character at a time.
        let held = std::mem::take(&mut self.hold);
        if let Hold::Pasting { text, closing } = held {
            self.paste(text, closing, event);
            return;
        }
        let Hold::Open { escape, mut tail } = held else {
            if is_plain_escape(&event) {
                self.hold = Hold::Open {
                    escape: Some(event),
                    tail: Vec::new(),
                };
            } else {
                self.ready.push_back(event);
            }
            return;
        };
        if report_char(&event).is_none() {
            // Not a character a report can contain: the run was text.
            self.ready.extend(escape);
            self.ready.extend(tail);
            self.accept(event);
            return;
        }
        if tail.len() == MAX_TAIL {
            // The bound, and the only place a run is dropped rather than
            // released. A run this long is not one any terminal emits, and
            // typing what could still have been a report is the worse of the
            // two wrong answers. Input resumes from the character that
            // overflowed.
            self.accept(event);
            return;
        }
        tail.push(event);
        let run = text(&tail);
        // **Where crossterm produces no `Event::Paste` of its own, the
        // markers a terminal wraps a paste in after `?2004h` arrive as keys,
        // and this grammar is asked before the report's.** That is Windows:
        // crossterm reads console records there and `EnableBracketedPaste`
        // answers `Unsupported` (crossterm 0.29 `event/sys/windows/`), so
        // without this the markers were typed into the draft and the
        // payload's newline submitted a turn mid-paste — measured on the
        // Windows ARM64 VM, 2026-09-11. Where crossterm parses the bytes it
        // consumes the markers first, so there the grammar is off: an opener
        // typed by hand with gaps would otherwise hold every keystroke until
        // a closing marker that is not coming, and `[2` would be held where
        // it used to be released (`GH-PANE-WINDOWS-VERIFY`, findings 7–8).
        if self.reassembles_pastes() {
            if PASTE_OPEN == run {
                self.hold = Hold::Pasting {
                    text: String::new(),
                    closing: None,
                };
                return;
            }
            if PASTE_OPEN.starts_with(&run) {
                self.hold = Hold::Open { escape, tail };
                return;
            }
        }
        match classify(&run) {
            Sgr::Opening | Sgr::Body => self.hold = Hold::Open { escape, tail },
            Sgr::Report => {
                if let Some(mouse) = sgr_mouse_event(&text(&tail)) {
                    self.ready.push_back(Event::Mouse(mouse));
                }
            }
            Sgr::Other => {
                self.ready.extend(escape);
                self.ready.extend(tail);
            }
        }
    }
}

/// What the characters after an Escape can still turn into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sgr {
    /// The opening of a report and nothing yet committed: ``, `[`, `[<`.
    Opening,
    /// Committed: `[<` and a well-formed start of the numeric body.
    Body,
    /// `[<code;column;row` closed by `M` or `m`, all three fields well formed.
    Report,
    /// Cannot be an SGR mouse report, whatever arrives next.
    Other,
}

/// Classify a run against `[ < digits ; digits ; digits (M|m)`. Everything
/// that is not a prefix of that grammar is `Other`, and `Other` is decided as
/// early as the grammar allows — a fourth field, a non-digit, or an empty
/// field that is not the one still being typed all end the run at once rather
/// than waiting for a terminator that would only confirm it.
fn classify(text: &str) -> Sgr {
    if text.is_empty() || text == "[" || text == "[<" {
        return Sgr::Opening;
    }
    let Some(body) = text.strip_prefix("[<") else {
        return Sgr::Other;
    };
    let terminated = body.ends_with(['M', 'm']);
    let fields = if terminated {
        &body[..body.len() - 1]
    } else {
        body
    };
    let fields: Vec<&str> = fields.split(';').collect();
    if fields.len() > 3 {
        return Sgr::Other;
    }
    for (index, field) in fields.iter().enumerate() {
        let growing = !terminated && index + 1 == fields.len();
        if (field.is_empty() && !growing) || !field.bytes().all(|byte| byte.is_ascii_digit()) {
            return Sgr::Other;
        }
    }
    match (terminated, parse_report(text)) {
        (false, _) => Sgr::Body,
        (true, Some(_)) => Sgr::Report,
        (true, None) => Sgr::Other,
    }
}

/// One SGR mouse report: `ESC [ < code ; column ; row` closed by `M` for a
/// press or `m` for a release. Coordinates are 1-based, so a zero is
/// malformed and not a report at all.
struct Report {
    code: u16,
    column: u16,
    row: u16,
    press: bool,
}

fn parse_report(text: &str) -> Option<Report> {
    let body = text.strip_prefix("[<")?;
    let press = body.ends_with('M');
    let body = body.strip_suffix(['M', 'm'])?;
    let mut fields = body.split(';');
    let code = fields.next()?.parse::<u16>().ok()?;
    let column = fields.next()?.parse::<u16>().ok()?;
    let row = fields.next()?.parse::<u16>().ok()?;
    if fields.next().is_some() || column == 0 || row == 0 {
        return None;
    }
    Some(Report {
        code,
        column,
        row,
        press,
    })
}

fn is_plain_escape(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind != KeyEventKind::Release
        && key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE)
}

/// True for a key release, which no platform's report grammar contains and
/// which [`TerminalInput::accept`] steps over rather than reading.
fn is_key_release(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release)
}

/// Whether a key's modifiers name only how it was struck on this layout —
/// nothing, Shift, or AltGr — and not a different meaning.
///
/// **Shift is a layout fact everywhere** (`<` on US, `;` on German, a digit on
/// AZERTY): the Windows console derives it from `control_key_state`, so a
/// report that disqualified Shift died on its second character there. **AltGr
/// is the console's Control-and-Alt together**, which is how a German layout
/// types `[`, `]`, `~` and `@` — every one of them a character a report or a
/// paste marker carries; measured on the ARM64 VM (German layout),
/// 2026-09-17: a report's `[` arrived as `CONTROL | ALT`, the run was
/// released as text and `<65;101;28M` reached the composer. Control or Alt
/// alone does change what a key means, so either still disqualifies one.
fn struck_plainly(modifiers: KeyModifiers) -> bool {
    let beyond_shift = modifiers - KeyModifiers::SHIFT;
    beyond_shift.is_empty() || beyond_shift == KeyModifiers::CONTROL | KeyModifiers::ALT
}

/// The character an event contributes to a run, or `None` if the event cannot
/// be part of a report at all: a pressed character key [`struck_plainly`].
fn report_char(event: &Event) -> Option<char> {
    let Event::Key(key) = event else { return None };
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    struck_plainly(key.modifiers).then_some(character)
}

/// Every element of a held tail came through `report_char`, so none is lost.
fn text(tail: &[Event]) -> String {
    tail.iter().filter_map(report_char).collect()
}

/// The character an event contributes to a **paste payload**, which is a
/// wider question than [`report_char`]'s: a paste carries the newlines and
/// tabs of the text that was pasted, and a host that delivers it as key
/// presses delivers those as `Enter` and `Tab`.
///
/// A key that produces no character — an arrow, a function key — contributes
/// nothing rather than ending the paste: the terminal said where the paste
/// ends, and only the closing marker is allowed to say so.
fn paste_char(event: &Event) -> Option<char> {
    let Event::Key(key) = event else { return None };
    if key.kind == KeyEventKind::Release || !struck_plainly(key.modifiers) {
        return None;
    }
    match key.code {
        KeyCode::Char(character) => Some(character),
        KeyCode::Enter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        _ => None,
    }
}

/// Convert the report through the same bit layout crossterm uses on its
/// ordinary Unix parser. This path exists for reports that crossterm surfaced
/// as key events (notably fragmented PTY/ConPTY input), so dropping a button
/// here would make click behavior depend on how one report happened to split.
fn sgr_mouse_event(text: &str) -> Option<MouseEvent> {
    let report = parse_report(text)?;
    let code = u8::try_from(report.code).ok()?;
    let button = (code & 0b0000_0011) | ((code & 0b1100_0000) >> 4);
    let dragging = code & 0b0010_0000 != 0;
    let kind = match (button, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3..=5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return None,
    };
    let kind = if report.press {
        kind
    } else {
        match kind {
            MouseEventKind::Down(button) => MouseEventKind::Up(button),
            other => other,
        }
    };
    let mut modifiers = KeyModifiers::NONE;
    if code & 0b0000_0100 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if code & 0b0000_1000 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if code & 0b0001_0000 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    Some(MouseEvent {
        kind,
        column: report.column - 1,
        row: report.row - 1,
        modifiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn escape() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    /// The events crossterm produces for a run of printable bytes: `M` is the
    /// only one it reports with Shift.
    fn chars(text: &str) -> Vec<Event> {
        text.chars()
            .map(|character| {
                Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    if character.is_uppercase() {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::NONE
                    },
                ))
            })
            .collect()
    }

    /// An Escape and the printable tail a terminal sends after it.
    fn report(tail: &str) -> Vec<Event> {
        let mut events = vec![escape()];
        events.extend(chars(tail));
        events
    }

    /// Drive the reassembler over `stream` with the terminal falling silent
    /// after each index in `quiet_after`. Silence is the only thing a read
    /// boundary can change, and it is the one thing a PTY cannot be made to do
    /// on purpose — which is why the coverage lives here. Returns what the UI
    /// would receive, in order.
    fn drive(stream: &[Event], quiet_after: &[usize]) -> Vec<Event> {
        drive_on(cfg!(windows), stream, quiet_after)
    }

    /// [`drive`] with the paste grammar chosen explicitly, so both the
    /// console-record path and the byte-parsing path are covered on every
    /// host.
    fn drive_on(reassembles_pastes: bool, stream: &[Event], quiet_after: &[usize]) -> Vec<Event> {
        let mut input = TerminalInput::new(if reassembles_pastes {
            Console::Records
        } else {
            Console::Unix
        });
        let mut seen = Vec::new();
        for (index, event) in stream.iter().enumerate() {
            input.accept(event.clone());
            if quiet_after.contains(&(index + 1)) {
                input.quiet();
            }
            seen.extend(input.ready.drain(..));
        }
        seen
    }

    /// What the composer would be left holding. The release filter is belt
    /// and braces: [`TerminalInput::accept`] drops a key release outright, so
    /// one reaching here would already be the defect
    /// `a_key_release_is_never_delivered` names.
    fn typed(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Char(character) => Some(character),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn mouse_kinds(events: &[Event]) -> Vec<MouseEventKind> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Mouse(mouse) => Some(mouse.kind),
                _ => None,
            })
            .collect()
    }

    fn escapes(events: &[Event]) -> usize {
        events.iter().filter(|event| is_plain_escape(event)).count()
    }

    /// **The regression this rewrite exists for, at every boundary a read can
    /// fall on.** A report split anywhere, with the terminal then silent for as
    /// long as it likes, still reaches the UI as a mouse event and never as
    /// text. CI failed on the first boundary: the Escape alone, then the rest
    /// once the 20 ms grace had already released it.
    #[test]
    fn a_report_split_at_any_boundary_is_never_typed() {
        for (tail, kind) in [
            ("[<65;101;28M", Some(MouseEventKind::ScrollDown)),
            ("[<64;1;2M", Some(MouseEventKind::ScrollUp)),
            ("[<0;10;5M", Some(MouseEventKind::Down(MouseButton::Left))),
            ("[<0;10;5m", Some(MouseEventKind::Up(MouseButton::Left))),
            ("[<65535;65535;65535M", None),
        ] {
            let stream = report(tail);
            let expected: Vec<MouseEventKind> = kind.into_iter().collect();
            for split in 1..stream.len() {
                let seen = drive(&stream, &[split]);
                assert_eq!(typed(&seen), "", "{tail} split after {split}");
                assert_eq!(mouse_kinds(&seen), expected, "{tail} split after {split}");
            }
        }
    }

    /// Three reads, with both pauses wherever the kernel happened to put them.
    #[test]
    fn a_report_split_three_ways_is_never_typed() {
        let stream = report("[<65;101;28M");
        for first in 1..stream.len() {
            for second in first + 1..stream.len() {
                let seen = drive(&stream, &[first, second]);
                assert_eq!(typed(&seen), "", "split after {first} and {second}");
                assert_eq!(
                    mouse_kinds(&seen),
                    vec![MouseEventKind::ScrollDown],
                    "split after {first} and {second}"
                );
            }
        }
    }

    /// The exact shape CI hit: the Escape arrives alone, the runner is loaded,
    /// the grace expires and the Escape is delivered — and the tail that turns
    /// up afterwards still scrolls instead of being typed.
    #[test]
    fn a_tail_that_arrives_after_the_escape_was_delivered_still_scrolls() {
        let seen = drive(&report("[<65;101;28M"), &[1]);
        assert_eq!(typed(&seen), "");
        assert_eq!(mouse_kinds(&seen), vec![MouseEventKind::ScrollDown]);
        assert_eq!(escapes(&seen), 1);
    }

    /// A run stops being a report the moment a character the grammar cannot
    /// accept arrives — at *any* position — and everything held is released to
    /// the editor at once, in order, behind the Escape that opened it. Nothing
    /// is released before that character, so holding costs no text and no
    /// reordering.
    #[test]
    fn a_run_that_stops_matching_is_released_whole_and_immediately() {
        let good = "[<65;101;28M";
        for cut in 0..good.len() {
            let text = format!("{}x", &good[..cut]);
            let stream = report(&text);
            assert!(
                drive(&stream[..stream.len() - 1], &[]).is_empty(),
                "released before the run was decided, cut {cut}"
            );
            let seen = drive(&stream, &[]);
            assert_eq!(typed(&seen), text, "cut {cut}");
            assert_eq!(escapes(&seen), 1, "cut {cut}");
            assert_eq!(seen.len(), text.len() + 1, "cut {cut}");
        }
    }

    /// Read whole, the report costs the UI nothing at all: no Escape, no text.
    #[test]
    fn a_report_read_whole_never_shows_the_ui_an_escape() {
        let seen = drive(&report("[<65;101;28M"), &[]);
        assert_eq!(mouse_kinds(&seen), vec![MouseEventKind::ScrollDown]);
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn two_reports_in_one_read_are_both_consumed() {
        let mut stream = report("[<64;1;2M");
        stream.extend(report("[<65;3;4M"));
        let seen = drive(&stream, &[]);
        assert_eq!(
            mouse_kinds(&seen),
            vec![MouseEventKind::ScrollUp, MouseEventKind::ScrollDown]
        );
        assert_eq!(typed(&seen), "");
        assert_eq!(escapes(&seen), 0);
    }

    #[test]
    fn text_after_a_report_in_the_same_read_reaches_the_editor_intact() {
        let mut stream = report("[<0;10;5M");
        stream.extend(chars("hello"));
        let seen = drive(&stream, &[]);
        assert_eq!(typed(&seen), "hello");
        assert_eq!(escapes(&seen), 0);
        assert_eq!(
            mouse_kinds(&seen),
            vec![MouseEventKind::Down(MouseButton::Left)]
        );
    }

    /// A real Escape key press, resolved by silence. With no silence at all it
    /// is released the moment the first character proves the run is not a
    /// report, so the answer does not depend on the pause either way.
    #[test]
    fn a_lone_escape_reaches_the_ui_and_the_text_after_it_is_intact() {
        let mut stream = vec![escape()];
        stream.extend(chars("hi"));
        for quiet in [vec![1usize], vec![]] {
            let seen = drive(&stream, &quiet);
            assert_eq!(typed(&seen), "hi", "quiet {quiet:?}");
            assert_eq!(escapes(&seen), 1, "quiet {quiet:?}");
            assert!(seen.first().is_some_and(is_plain_escape), "quiet {quiet:?}");
        }
    }

    /// After a delivered Escape the matcher stays armed — that is what makes a
    /// late tail safe — so a `[` typed next is held for exactly one character
    /// and released, in order, as soon as the next character decides it.
    #[test]
    fn a_bracket_typed_after_a_lone_escape_is_released_by_the_next_character() {
        let mut stream = vec![escape()];
        stream.extend(chars("[a]"));
        let seen = drive(&stream, &[1]);
        assert_eq!(typed(&seen), "[a]");
        assert_eq!(escapes(&seen), 1);
    }

    /// A prefix whose terminator never arrives is held, not typed, and silence
    /// anywhere in it changes nothing: waiting cannot decide what a prefix is.
    #[test]
    fn a_prefix_whose_terminator_never_arrives_is_never_typed() {
        let stream = report("[<65;101;28");
        for quiet in 1..=stream.len() {
            let seen = drive(&stream, &[quiet]);
            assert_eq!(typed(&seen), "", "quiet after {quiet}");
            assert!(mouse_kinds(&seen).is_empty(), "quiet after {quiet}");
        }
    }

    /// Anything that cannot be part of a report ends the run, including an
    /// event carrying no character at all, and it arrives behind what was held.
    #[test]
    fn a_non_key_event_ends_a_run_and_follows_what_was_held() {
        let mut stream = report("[<65;101");
        stream.push(Event::Resize(80, 24));
        let seen = drive(&stream, &[]);
        assert_eq!(typed(&seen), "[<65;101");
        assert_eq!(escapes(&seen), 1);
        assert!(matches!(seen.last(), Some(Event::Resize(80, 24))));
    }

    /// The bound is a size, and it is the grammar's own maximum. A run that
    /// reaches it can never become a report, so it is dropped rather than
    /// typed, and input resumes from the character that overflowed.
    #[test]
    fn an_unterminated_prefix_is_dropped_at_the_size_bound() {
        assert_eq!("[<65535;65535;65535M".len(), MAX_TAIL);
        let digits = 30;
        let stream = report(&format!("[<{}", "1".repeat(digits)));
        let seen = drive(&stream, &[]);
        assert_eq!(typed(&seen), "1".repeat(digits - (MAX_TAIL - 2)));
        assert_eq!(escapes(&seen), 0);
    }

    #[test]
    fn the_grammar_admits_exactly_the_reports_and_their_prefixes() {
        for text in [
            "[<0;10;5M",
            "[<0;10;5m",
            "[<2;1;1M",
            "[<32;80;24m",
            "[<64;1;2M",
            "[<65;101;28M",
            "[<65535;65535;65535M",
        ] {
            assert_eq!(classify(text), Sgr::Report, "rejected {text}");
        }
        for text in ["", "[", "[<"] {
            assert_eq!(classify(text), Sgr::Opening, "committed {text}");
        }
        for text in ["[<0", "[<0;", "[<65;101", "[<65;101;28"] {
            assert_eq!(classify(text), Sgr::Body, "released {text}");
        }
        for text in [
            "[200~",
            "[<a",
            "[<;1;1",
            "[<1;2;3;",
            "[<1;2;3;4M",
            "[<65;1;2M;",
            "[<65;0;2M",
            "[<0;1;0M",
            "ok",
            "[<M",
            "literal[<65;101;28M",
        ] {
            assert_eq!(classify(text), Sgr::Other, "held {text}");
        }
    }

    #[test]
    fn sgr_reports_preserve_buttons_wheels_coordinates_and_modifiers() {
        let down = sgr_mouse_event("[<65;101;28M").unwrap();
        assert_eq!(down.kind, MouseEventKind::ScrollDown);
        assert_eq!((down.column, down.row), (100, 27));
        let up = sgr_mouse_event("[<64;1;2M").unwrap();
        assert_eq!(up.kind, MouseEventKind::ScrollUp);
        assert_eq!((up.column, up.row), (0, 1));
        let left = sgr_mouse_event("[<0;10;5M").unwrap();
        assert_eq!(left.kind, MouseEventKind::Down(MouseButton::Left));
        assert_eq!((left.column, left.row), (9, 4));
        let release = sgr_mouse_event("[<0;10;5m").unwrap();
        assert_eq!(release.kind, MouseEventKind::Up(MouseButton::Left));
        let modified = sgr_mouse_event("[<28;2;3M").unwrap();
        assert_eq!(modified.kind, MouseEventKind::Down(MouseButton::Left));
        assert_eq!(
            modified.modifiers,
            KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL
        );
        for text in [
            "[<65;101;28",
            "[<65;101;28;M",
            "[<65;0;2M",
            "[<65535;1;2M",
            "[200~",
            "literal[<65;101;28M",
        ] {
            assert!(sgr_mouse_event(text).is_none(), "accepted {text}");
        }
    }

    /// The same report as the Windows console delivers it. Two differences
    /// from the Unix stream, and each on its own typed the report into the
    /// composer: conhost synthesises a release record for every press, and
    /// crossterm derives the modifiers from `control_key_state`, so `<` — a
    /// shifted key on a US layout — arrives with `SHIFT` just as `M` does.
    fn windows_report(tail: &str) -> Vec<Event> {
        let mut events = Vec::new();
        let mut struck = |code: KeyCode, modifiers: KeyModifiers| {
            for kind in [KeyEventKind::Press, KeyEventKind::Release] {
                events.push(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)));
            }
        };
        struck(KeyCode::Esc, KeyModifiers::NONE);
        for character in tail.chars() {
            let shifted = character == '<' || character.is_uppercase();
            let modifiers = if shifted {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            struck(KeyCode::Char(character), modifiers);
        }
        events
    }

    /// The same report from a German-layout Windows console, which is what
    /// the ARM64 VM has: `[` and `~` are AltGr keys and arrive as Control
    /// and Alt together, `;` is shifted, `<` is not.
    fn german_console_report(tail: &str) -> Vec<Event> {
        let mut events = Vec::new();
        let mut struck = |code: KeyCode, modifiers: KeyModifiers| {
            for kind in [KeyEventKind::Press, KeyEventKind::Release] {
                events.push(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)));
            }
        };
        struck(KeyCode::Esc, KeyModifiers::NONE);
        for character in tail.chars() {
            let modifiers = match character {
                '[' | '~' => KeyModifiers::CONTROL | KeyModifiers::ALT,
                ';' => KeyModifiers::SHIFT,
                _ if character.is_uppercase() => KeyModifiers::SHIFT,
                _ => KeyModifiers::NONE,
            };
            struck(KeyCode::Char(character), modifiers);
        }
        events
    }

    /// **The report the ARM64 VM typed into the composer as `<65;101;28M`**:
    /// its `[` came as AltGr, which is Control and Alt together, and the old
    /// rule released the run there. Every boundary, like the shapes above.
    #[test]
    fn a_report_in_the_german_console_shape_is_never_typed() {
        for (tail, kind) in [
            ("[<65;101;28M", MouseEventKind::ScrollDown),
            ("[<0;10;5M", MouseEventKind::Down(MouseButton::Left)),
            ("[<0;10;5m", MouseEventKind::Up(MouseButton::Left)),
        ] {
            let stream = german_console_report(tail);
            for split in 0..stream.len() {
                let quiet: &[usize] = if split == 0 { &[] } else { &[split] };
                let seen = drive_on(true, &stream, quiet);
                assert_eq!(typed(&seen), "", "{tail} split after {split}");
                assert_eq!(mouse_kinds(&seen), vec![kind], "{tail} split after {split}");
            }
        }
    }

    /// And a paste's markers on that console: `~` is AltGr there too.
    #[test]
    fn a_paste_in_the_german_console_shape_arrives_whole() {
        let mut stream = german_console_report("[200~");
        stream.extend(chars("ab"));
        stream.extend(german_console_report("[201~"));
        assert_eq!(
            drive_on(true, &stream, &[]),
            vec![Event::Paste("ab".into())]
        );
    }

    /// **The report the Windows cell watched being typed into the composer.**
    /// Nothing in this file is platform-specific, so the platform's own event
    /// shape is covered here rather than on the one runner that can produce
    /// it: a release between every two characters, and `<` shifted. Split at
    /// every boundary, like the Unix stream above.
    #[test]
    fn a_report_in_the_windows_console_shape_is_never_typed() {
        for (tail, kind) in [
            ("[<65;101;28M", Some(MouseEventKind::ScrollDown)),
            ("[<64;1;2M", Some(MouseEventKind::ScrollUp)),
            ("[<0;10;5M", Some(MouseEventKind::Down(MouseButton::Left))),
            ("[<0;10;5m", Some(MouseEventKind::Up(MouseButton::Left))),
        ] {
            let stream = windows_report(tail);
            let expected: Vec<MouseEventKind> = kind.into_iter().collect();
            for split in 0..stream.len() {
                let quiet: &[usize] = if split == 0 { &[] } else { &[split] };
                let seen = drive(&stream, quiet);
                assert_eq!(typed(&seen), "", "{tail} split after {split}");
                assert_eq!(mouse_kinds(&seen), expected, "{tail} split after {split}");
                // The one cost silence is allowed: an Escape still alone when
                // the stream goes quiet is delivered as the key press it also
                // is. `split == 0` is the whole run read without a pause, and
                // "alone" spans the Escape's own press *and* the release that
                // follows it — so exactly the first two positions, and never
                // once a report character has been read.
                assert_eq!(
                    escapes(&seen),
                    usize::from((1..=2).contains(&split)),
                    "{tail} split after {split}"
                );
            }
        }
    }

    /// **A key release never leaves this file — and an unsent Enter is what
    /// it costs when one does.**
    ///
    /// Nothing downstream reads a release: `ui.rs` and `workbench::input`
    /// each drop one on arrival. What a queued release
    /// does instead is answer a question asked of the queue, and `ui.rs` asks
    /// one on every Enter — "is more input already behind this?", whose yes
    /// turns the Enter into a newline. On Windows crossterm emits a release
    /// after every press, so that question was answered yes by the Enter's
    /// own release, every Enter became a newline, and nothing typed on that
    /// platform was ever sent. The whole `tui_live` target was red on the
    /// Windows cell for it.
    ///
    /// Driven with the console's own stream shape rather than on the one
    /// runner that produces it, like the report shapes above.
    #[test]
    fn a_key_release_is_never_delivered() {
        let pressed = |code| {
            Event::Key(KeyEvent::new_with_kind(
                code,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))
        };
        let released = |code| {
            Event::Key(KeyEvent::new_with_kind(
                code,
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ))
        };
        for reassembles_pastes in [true, false] {
            let stream = vec![
                pressed(KeyCode::Char('h')),
                released(KeyCode::Char('h')),
                pressed(KeyCode::Enter),
                released(KeyCode::Enter),
            ];
            let seen = drive_on(reassembles_pastes, &stream, &[]);
            assert_eq!(
                seen,
                vec![pressed(KeyCode::Char('h')), pressed(KeyCode::Enter)],
                "a release survived the resolver (paste reassembly: {reassembles_pastes})"
            );
        }
    }

    /// Shift names the key that was struck, not the character it produced, and
    /// which characters carry it is a layout decision — `<` is shifted on a US
    /// keyboard and every digit is on AZERTY. Control and Alt do change what a
    /// press means, so they still end a run.
    #[test]
    fn shift_does_not_disqualify_a_report_character() {
        for character in ['<', 'M', '6', ';'] {
            let shifted = Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::SHIFT));
            assert_eq!(report_char(&shifted), Some(character), "{character}");
        }
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let held = Event::Key(KeyEvent::new(KeyCode::Char('<'), modifiers));
            assert_eq!(report_char(&held), None, "{modifiers:?}");
        }
    }

    /// The markers a terminal wraps a paste in become an `Event::Paste`, and
    /// not one character of them reaches the UI.
    ///
    /// This is the Windows path made testable on every host: crossterm reads
    /// console records there and produces no `Event::Paste` of its own, so
    /// without this the draft received `[200~`, the payload's newline
    /// submitted a turn in the middle of the paste, and `[201~` followed it.
    #[test]
    fn a_bracketed_paste_arrives_as_one_paste_event_however_it_is_split() {
        let mut stream = report("[200~");
        stream.extend(chars("first line"));
        stream.push(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        stream.extend(chars("second line"));
        stream.extend(report("[201~"));

        // Every read boundary, including one inside each marker.
        for quiet_after in [vec![], vec![0], vec![2], vec![4], vec![8, 20], vec![25, 27]] {
            let seen = drive_on(true, &stream, &quiet_after);
            assert_eq!(
                seen,
                vec![Event::Paste("first line\nsecond line".into())],
                "{quiet_after:?}"
            );
        }
    }

    /// A key release falls between every two characters on Windows, and a
    /// paste steps over them exactly as a report does.
    #[test]
    fn a_key_release_inside_a_paste_is_stepped_over() {
        let mut stream = report("[200~");
        stream.push(Event::Key(KeyEvent {
            kind: KeyEventKind::Release,
            ..KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
        }));
        stream.extend(chars("ab"));
        stream.extend(report("[201~"));
        assert_eq!(
            drive_on(true, &stream, &[]),
            vec![Event::Paste("ab".into())]
        );
    }

    /// An Escape run that opens like a paste and then is not one is released
    /// as the text it turned out to be, never held for a marker that is not
    /// coming.
    #[test]
    fn an_escape_run_that_is_not_a_paste_is_released() {
        let seen = drive_on(true, &report("[2J"), &[]);
        assert_eq!(escapes(&seen), 1);
        assert_eq!(
            seen.iter().filter_map(report_char).collect::<String>(),
            "[2J"
        );
    }

    /// Inside a paste, an Escape that does not begin the closing marker is
    /// payload's neighbour rather than a way out: the paste ends where the
    /// terminal said it ends and nowhere else.
    #[test]
    fn only_the_closing_marker_ends_a_paste() {
        let mut stream = report("[200~");
        stream.extend(chars("a"));
        stream.extend(report("[9z"));
        stream.extend(chars("b"));
        stream.extend(report("[201~"));
        assert_eq!(
            drive_on(true, &stream, &[]),
            vec![Event::Paste("a[9zb".into())]
        );
    }

    /// Raw terminal input on Windows delivers the markers as characters, so
    /// that console reassembles a paste exactly as the record path does.
    #[test]
    fn raw_terminal_input_reassembles_a_paste_too() {
        let mut input = TerminalInput::new(Console::VtInput);
        let mut stream = report("[200~");
        stream.extend(chars("ab"));
        stream.extend(report("[201~"));
        for event in stream {
            input.accept(event);
        }
        assert_eq!(
            input.ready.drain(..).collect::<Vec<_>>(),
            vec![Event::Paste("ab".into())]
        );
    }

    /// Where the terminal parses pastes itself the grammar is off, and an
    /// opener typed by hand — Escape, then `[200~` with gaps — is text that
    /// reaches the editor with everything typed after it: the dead keyboard
    /// `GH-PANE-WINDOWS-VERIFY` reproduced (finding 7) cannot happen there.
    #[test]
    fn an_opener_typed_by_hand_is_text_where_the_terminal_parses_pastes() {
        let mut stream = report("[200~");
        stream.extend(chars("hello"));
        let seen = drive_on(false, &stream, &[1, 3, 6]);
        assert_eq!(escapes(&seen), 1);
        assert_eq!(typed(&seen), "[200~hello");
    }

    /// And `[2` is released the instant the `2` arrives, as before the
    /// grammar existed (finding 8): only a report prefix is ever held there.
    #[test]
    fn an_escape_bracket_two_is_released_at_once_where_the_grammar_is_off() {
        let seen = drive_on(false, &report("[2"), &[]);
        assert_eq!(escapes(&seen), 1);
        assert_eq!(typed(&seen), "[2");
    }

    /// A paste carries no report character, so it can never be swallowed.
    #[test]
    fn a_paste_is_never_mistaken_for_a_report() {
        let stream = vec![escape(), Event::Paste("[<65;101;28M".into())];
        let seen = drive(&stream, &[]);
        assert_eq!(escapes(&seen), 1);
        assert!(matches!(seen.last(), Some(Event::Paste(text)) if text == "[<65;101;28M"));
    }
}
