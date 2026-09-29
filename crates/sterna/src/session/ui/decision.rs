//! The decision prompts -- an approval, a question a cell asked, and the
//! words behind "another way" -- each drawn as a sheet (`workbench/sheet.rs`)
//! over everything else, and answered on purpose.
//!
//! **A key typed before a prompt was on screen is not an answer to it.** A
//! prompt arms only once it has been drawn and has seen no key for
//! [`ARMING`]; until then every key and paste is dropped, and the foot says
//! so. Someone typing a sentence when an approval appears mid-word is
//! typing the sentence: an `s` in it must never allow a call for the whole
//! session.
//!
//! Esc on an approval refuses this one call and remembers nothing: "Not
//! now". "Deny for this session", chosen on purpose, is remembered for the
//! session and listed on the Sandbox sheet with a way to forget it; the
//! sheet says the difference in one line. Ctrl-C cancels the call the way
//! it cancels a running one.
use crate::workbench::{Action, Answer, Geometry, Item, Outcome, Sheet, Tone, sheet};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect, widgets::Clear};
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

#[cfg(test)]
use crate::workbench::sheet::ARMING;

/// What the loop does after a prompt took an event.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Done {
    /// Nothing on screen changed.
    Nothing,
    Redraw,
    /// Ctrl-C: the prompt was cancelled, and the turn is to stop.
    Interrupt,
    /// "Always allow": the gate has let these hosts through; the loop
    /// saves them to the global settings, where the screen state is.
    KeepHosts(Vec<String>),
}

/// The words behind "another way": the call they refuse, and the field.
struct Redirect {
    request: crate::approval::Request,
    field: sheet::Field,
}

#[derive(Default)]
pub(super) struct Prompts {
    approvals: VecDeque<(u64, crate::approval::Request)>,
    asking: Option<(u64, crate::ask::Request)>,
    redirect: Option<Redirect>,
    /// Words typed for "another way" and put back with Esc, by the call they
    /// were for, so a second `a` finds them again.
    words: BTreeMap<String, String>,
    next: u64,
    sheet: Sheet,
    /// Which prompt the sheet shows: a new one gets a fresh sheet and a fresh
    /// arming window ([`Sheet::armed`]).
    front: Option<String>,
    /// The exact arguments instead of the readable body.
    raw: bool,
    /// Where the prompt was last drawn, for the mouse.
    geometry: Geometry,
    /// Where the pointer rests, for hover.
    hover: Option<(u16, u16)>,
}

impl Prompts {
    pub(super) fn active(&self) -> bool {
        self.redirect.is_some() || !self.approvals.is_empty() || self.asking.is_some()
    }

    pub(super) fn push_approval(&mut self, request: crate::approval::Request) {
        self.next += 1;
        self.approvals.push_back((self.next, request));
    }

    pub(super) fn ask(&mut self, request: crate::ask::Request) {
        self.next += 1;
        self.asking = Some((self.next, request));
    }

    /// Drops approvals whose call has stopped waiting; `true` when any went.
    pub(super) fn retain_pending(&mut self) -> bool {
        let before = self.approvals.len();
        self.approvals.retain(|(_, request)| request.is_pending());
        before != self.approvals.len()
    }

    /// A turn that ended takes its unanswered approvals with it.
    pub(super) fn clear_approvals(&mut self) {
        self.approvals.clear();
    }

    /// The prompt in front: its identity, and the sheet's rows.
    fn build(&mut self) -> Option<String> {
        if let Some(redirect) = &self.redirect {
            let id = format!("redirect:{}", redirect.request.action().summary());
            let items = redirect_items(&mut self.sheet, redirect);
            self.sheet.set_items(items);
            return Some(id);
        }
        if let Some((seq, request)) = self.approvals.front() {
            let id = format!("approval:{seq}");
            let items = approval_items(&mut self.sheet, request, self.approvals.len(), self.raw);
            self.sheet.set_items(items);
            return Some(id);
        }
        if let Some((seq, request)) = &self.asking {
            let id = format!("ask:{seq}");
            let items = ask_items(&mut self.sheet, request);
            self.sheet.set_items(items);
            return Some(id);
        }
        None
    }

    /// Draws the prompt in front over everything else. The first frame a
    /// prompt is drawn on starts its arming window.
    pub(super) fn draw(&mut self, f: &mut Frame<'_>, theme: crate::tui::Theme) {
        let identity = self.front_identity();
        if identity != self.front {
            // "Another way" is the same decision, reworded: it keeps the
            // approval's arming, so words typed straight after `a` are kept.
            let redirecting = identity
                .as_deref()
                .is_some_and(|id| id.starts_with("redirect:"))
                && self
                    .front
                    .as_deref()
                    .is_some_and(|id| id.starts_with("approval:"));
            let shown = self.sheet.shown.filter(|_| redirecting);
            self.front = identity;
            self.sheet = Sheet::default();
            self.sheet.shown = shown;
            self.raw = false;
        }
        if self.build().is_none() {
            return;
        }
        let a = f.area();
        let width = a.width.saturating_sub(4).min(100);
        // As tall as what it holds, and never taller than the screen.
        let wanted =
            crate::workbench::sheet::wanted_height(&self.sheet, width.saturating_sub(4)) + 2;
        let height = a.height.saturating_sub(2).min(30).min(wanted.max(8));
        let area = Rect::new(
            a.x + a.width.saturating_sub(width) / 2,
            a.y + a.height.saturating_sub(height) / 2,
            width,
            height,
        );
        // A modal owns the whole frame: nothing behind it shows through.
        f.render_widget(Clear, a);
        let mut g = Geometry {
            local: Some(area),
            ..Geometry::default()
        };
        let framed = area.width >= 12 && area.height >= 6;
        if framed {
            crate::workbench::frame(f, area, Tone::Accent, theme);
        }
        let inner = if framed {
            Rect::new(area.x + 2, area.y + 1, area.width - 4, area.height - 2)
        } else {
            area
        };
        let drawn = crate::workbench::sheet::draw(
            f,
            &mut g,
            inner,
            &mut self.sheet,
            theme,
            None,
            self.hover,
        );
        if let (true, Some(scroll)) = (framed, drawn.scroll) {
            crate::workbench::sheet::scrollbar(f, area.right() - 1, scroll, theme);
        }
        crate::workbench::glow(f, &g.hits, self.hover, theme);
        self.geometry = g;
    }

    fn front_identity(&self) -> Option<String> {
        if let Some(redirect) = &self.redirect {
            return Some(format!("redirect:{}", redirect.request.action().summary()));
        }
        if let Some((seq, _)) = self.approvals.front() {
            return Some(format!("approval:{seq}"));
        }
        self.asking.as_ref().map(|(seq, _)| format!("ask:{seq}"))
    }

    /// Holds back an event that came before the prompt was armed, and says
    /// so. `true` when it was held back. The prompt in front must be the one
    /// on screen: when an answer has just taken the one before it, a second
    /// key in the same burst is held until the next one has been drawn and
    /// armed -- never an answer to a call nobody saw.
    fn hold_back(&mut self, now: Instant) -> bool {
        if self.front_identity() != self.front {
            self.sheet.notice = "Typing was held back: choose below when you are ready.".into();
            return true;
        }
        self.sheet.hold_back(now)
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> Done {
        let now = Instant::now();
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.cancel();
        }
        if self.hold_back(now) {
            return Done::Redraw;
        }
        let outcome = self.sheet.key(key);
        self.outcome(outcome)
    }

    pub(super) fn paste(&mut self, text: &str) -> Done {
        if self.hold_back(Instant::now()) {
            return Done::Redraw;
        }
        let outcome = self.sheet.paste(text);
        self.outcome(outcome)
    }

    pub(super) fn click(&mut self, x: u16, y: u16) -> Done {
        if self.hold_back(Instant::now()) {
            return Done::Redraw;
        }
        match self.geometry.hit(x, y) {
            Some(Action::Sheet(hit)) => {
                let outcome = self.sheet.click(&hit);
                self.outcome(outcome)
            }
            // A decision is never answered by a click beside it.
            _ => Done::Nothing,
        }
    }

    /// The pointer moved: a frame is owed only when the target under it
    /// changed. Hover never answers anything.
    pub(super) fn hover(&mut self, x: u16, y: u16) -> Done {
        let before = self.hover.and_then(|(x, y)| self.geometry.hit_rect(x, y));
        self.hover = Some((x, y));
        if self.geometry.hit_rect(x, y) == before {
            Done::Nothing
        } else {
            Done::Redraw
        }
    }

    pub(super) fn wheel(&mut self, up: bool) -> Done {
        match self.sheet.wheel(up) {
            Outcome::Nothing => Done::Nothing,
            _ => Done::Redraw,
        }
    }

    /// Ctrl-C: the call is cancelled the way a running one is, and the turn
    /// stops. A question is left for Sterna to decide.
    fn cancel(&mut self) -> Done {
        if let Some(redirect) = self.redirect.take() {
            redirect.request.respond(crate::approval::Decision::Cancel);
        } else if let Some((_, request)) = self.approvals.pop_front() {
            request.respond(crate::approval::Decision::Cancel);
        } else if let Some((_, request)) = self.asking.take() {
            request.respond(crate::ask::Answer::dismissed());
        }
        Done::Interrupt
    }

    fn outcome(&mut self, outcome: Outcome) -> Done {
        match outcome {
            Outcome::Nothing => Done::Nothing,
            Outcome::Act(Action::Answer(answer)) => self.answer(answer),
            Outcome::Act(Action::FieldEdited(index)) => {
                if let (Some(redirect), Some(item)) =
                    (self.redirect.as_mut(), self.sheet.items.get(index))
                    && let crate::workbench::ItemKind::Field(field) = &item.kind
                {
                    redirect.field = field.clone();
                }
                Done::Redraw
            }
            Outcome::Back => self.escape(),
            _ => Done::Redraw,
        }
    }

    /// Esc answers what the header chip says it answers.
    fn escape(&mut self) -> Done {
        if let Some(redirect) = self.redirect.take() {
            // Back to the call, keeping the words for it.
            self.words.insert(
                redirect.request.action().summary(),
                redirect.field.text.clone(),
            );
            self.next += 1;
            self.approvals.push_front((self.next, redirect.request));
            return Done::Redraw;
        }
        if let Some((_, request)) = self.approvals.pop_front() {
            request.respond(crate::approval::Decision::DenyOnce);
            return Done::Redraw;
        }
        if let Some((_, request)) = self.asking.take() {
            request.respond(crate::ask::Answer::dismissed());
        }
        Done::Redraw
    }

    fn answer(&mut self, answer: Answer) -> Done {
        use crate::approval::Decision;
        if let Some(redirect) = self.redirect.take() {
            if answer == Answer::Send {
                self.words.remove(&redirect.request.action().summary());
                redirect
                    .request
                    .respond(Decision::Redirect(redirect.field.text.trim().to_string()));
            } else {
                self.redirect = Some(redirect);
            }
            return Done::Redraw;
        }
        if !self.approvals.is_empty() {
            if answer == Answer::Raw {
                self.raw = !self.raw;
                return Done::Redraw;
            }
            let Some((_, request)) = self.approvals.pop_front() else {
                return Done::Nothing;
            };
            let hosts = request.hosts().to_vec();
            let decision = match answer {
                Answer::AllowOnce => Decision::AllowOnce,
                Answer::AllowForSession => Decision::AllowForSession,
                Answer::AllowHost if !hosts.is_empty() => Decision::AllowHostSession,
                Answer::AlwaysAllowHost if !hosts.is_empty() => {
                    request.respond(Decision::AllowHostAlways);
                    return Done::KeepHosts(hosts);
                }
                Answer::Deny => Decision::Deny,
                Answer::AnotherWay => {
                    let text = self
                        .words
                        .get(&request.action().summary())
                        .cloned()
                        .unwrap_or_default();
                    self.redirect = Some(Redirect {
                        request,
                        field: sheet::Field {
                            cursor: text.len(),
                            text,
                            ..sheet::Field::default()
                        },
                    });
                    return Done::Redraw;
                }
                _ => {
                    self.approvals.push_front((self.next, request));
                    return Done::Nothing;
                }
            };
            request.respond(decision);
            return Done::Redraw;
        }
        if let (Answer::Choice(index), Some((_, request))) = (answer, self.asking.take()) {
            let choice = request.question().choices.get(index).cloned();
            request.respond(crate::ask::Answer {
                choice,
                by: crate::ask::AnsweredBy::Person,
            });
        }
        Done::Redraw
    }
}

/// An approval: what the call is and why it asks, the four answers with
/// their letters, and then the call itself in words -- the exact arguments
/// one switch away.
fn approval_items(
    sheet: &mut Sheet,
    request: &crate::approval::Request,
    queued: usize,
    raw: bool,
) -> Vec<Item> {
    let action = request.action();
    let hosts = request.hosts();
    sheet.title = if !hosts.is_empty() {
        "Reach a new host"
    } else if request.leaves_sandbox() {
        "Leave the sandbox"
    } else {
        "Approve"
    }
    .into();
    sheet.crumbs = vec![action.label()];
    if queued > 1 {
        sheet.crumbs.push(format!("1 of {queued}"));
    }
    sheet.decision = true;
    sheet.esc = Some("Not now".into());
    let confirmation = action.confirmation();
    let mut items = Vec::new();
    if let Some(reason) = request.reason() {
        items.push(Item::info("Why this asks").shows(reason));
    }
    if let Some(hint) = request.hint_line() {
        items.push(Item::info("Jev").shows(if hint.fits >= 0.5 {
            "this looks like part of what you asked"
        } else {
            "this looks unrelated to what you asked"
        }));
    }
    let too_large = (!confirmation.complete)
        .then(|| "Too large to confirm here: deny it and ask for a smaller call.".to_string());
    // A refused host is offered first: letting one host through keeps the
    // command inside the sandbox, where letting the command out does not.
    if !hosts.is_empty() {
        let names = hosts.join(", ");
        items.push(Item::info(format!("The sandbox refused {names}.")).tone(Tone::Strong));
        items.push(
            Item::run(
                "allow-host",
                format!("Allow {names} for this session"),
                Action::Answer(Answer::AllowHost),
            )
            .key('h')
            .inline()
            .detail("Runs the command again inside the sandbox, with this host let through.")
            .disabled(too_large.clone()),
        );
        items.push(
            Item::run(
                "always-allow-host",
                format!("Always allow {names}"),
                Action::Answer(Answer::AlwaysAllowHost),
            )
            .key('w')
            .inline()
            .detail("The same, and the host is allowed in every session from now on.")
            .disabled(too_large.clone()),
        );
    }
    items.push(
        Item::run(
            "allow-once",
            if hosts.is_empty() {
                "Allow once"
            } else {
                "Allow once, outside the sandbox"
            },
            Action::Answer(Answer::AllowOnce),
        )
        .key('o')
        .inline()
        .detail(if hosts.is_empty() {
            "Runs this call. The next identical call asks again."
        } else {
            "Runs the command outside the sandbox, this once."
        })
        .disabled(too_large.clone()),
    );
    if hosts.is_empty() {
        items.push(
            Item::run(
                "allow-session",
                "Allow this call for the session",
                Action::Answer(Answer::AllowForSession),
            )
            .key('s')
            .inline()
            .detail("Runs this call, and every identical call this session without asking.")
            .disabled(too_large.clone()),
        );
    }
    items.push(
        Item::open(
            "another-way",
            "Another way",
            Action::Answer(Answer::AnotherWay),
        )
        .key('a')
        .inline()
        .detail("Refuses the call and tells Sterna what to do instead.")
        .disabled(too_large),
    );
    items.push(
        Item::run(
            "deny",
            "Deny for this session",
            Action::Answer(Answer::Deny),
        )
        .key('d')
        .inline()
        // The two refusals look alike; this one is remembered.
        .detail(
            "Refuses this call and every identical one without asking, until you forget it \
             on the Sandbox sheet. Esc refuses only this one.",
        ),
    );
    items.push(Item::toggle(
        "raw",
        "Show the exact arguments",
        raw,
        Action::Answer(Answer::Raw),
    ));
    if raw {
        items.extend(
            confirmation
                .text
                .lines()
                .map(|line| Item::info(line).tone(Tone::Code)),
        );
    } else {
        items.extend(body(action));
    }
    items
}

/// The call in words: a command as the shell will see it, a file's new
/// text as lines, and every other argument on a line of its own.
fn body(action: &crate::approval::Action) -> Vec<Item> {
    let mut items = vec![Item::heading("The call")];
    for (name, value) in action.arguments() {
        match name.as_str() {
            "path" => continue,
            "command" => {
                for line in value.lines() {
                    items.push(Item::info(format!("$ {line}")).tone(Tone::Code));
                }
            }
            _ if value.contains('\n') || name == "content" => {
                let lines = value.lines().count();
                items.push(
                    Item::info(format!(
                        "{name}: {lines} line{} · {} bytes",
                        if lines == 1 { "" } else { "s" },
                        value.len()
                    ))
                    .tone(Tone::Muted),
                );
                for line in value.lines().take(400) {
                    items.push(Item::info(format!("│ {line}")).tone(Tone::Code));
                }
            }
            _ => {
                let short: String = value.chars().take(200).collect();
                items.push(Item::info(format!("{name}: {short}")));
            }
        }
    }
    items
}

/// A question a cell asked: the question, each choice with its digit and
/// the decision model's share of it, and Esc to let Sterna decide.
fn ask_items(sheet: &mut Sheet, request: &crate::ask::Request) -> Vec<Item> {
    let question = request.question();
    sheet.title = "Sterna asks".into();
    sheet.crumbs.clear();
    sheet.decision = true;
    sheet.esc = Some("Let Sterna decide".into());
    let mut items = vec![Item::info(question.question.clone()).tone(Tone::Strong)];
    for (index, choice) in question.choices.iter().enumerate() {
        let mut item = Item::choice(
            format!("choice:{index}"),
            choice.clone(),
            false,
            Action::Answer(Answer::Choice(index)),
        );
        if let Some(digit) = char::from_digit(index as u32 + 1, 10).filter(|_| index < 9) {
            item = item.key(digit);
        }
        if let Some(weight) = request
            .weights()
            .and_then(|weights| weights.probabilities.get(index))
        {
            item = item.shows(format!("Sterna's guess {:.0}%", weight * 100.0));
        }
        items.push(item);
    }
    // What Esc does is said under the choices: the cell goes on either way.
    sheet.status = "Esc lets Sterna decide: it picks one and tells you what it assumed.".into();
    // The decision model's own pick is where the sheet opens.
    sheet.prefer = request.weights().and_then(|weights| {
        question
            .choices
            .iter()
            .position(|choice| *choice == weights.choice)
            .map(|index| format!("choice:{index}"))
    });
    items
}

/// "Another way": the words, sent with Enter, and Esc back to the call with
/// them kept.
fn redirect_items(sheet: &mut Sheet, redirect: &Redirect) -> Vec<Item> {
    sheet.title = "Another way".into();
    sheet.crumbs = vec![redirect.request.action().label()];
    sheet.decision = true;
    sheet.esc = Some("Back to the call".into());
    sheet.status = "Empty asks Sterna to propose one.".into();
    vec![
        Item::info("The call is refused either way. What should Sterna do instead?"),
        Item::field("words", "Instead", redirect.field.clone()).act(Action::Answer(Answer::Send)),
        Item::run("send", "Send", Action::Answer(Answer::Send)).trail(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{Admission, Gate};
    use crate::permissions::{Level, LiveLevel};
    use std::time::Duration;

    /// A write the Every call rung puts to the person, and the thread
    /// waiting on the answer.
    fn asked(
        gate: &Gate,
        requests: &std::sync::mpsc::Receiver<crate::approval::Request>,
    ) -> (crate::approval::Request, std::thread::JoinHandle<Admission>) {
        let mut arguments = std::collections::BTreeMap::new();
        arguments.insert("path".to_string(), "/tmp/root/a.txt".to_string());
        arguments.insert("content".to_string(), "one\ntwo".to_string());
        let action =
            crate::approval::Action::new("write", std::path::Path::new("/tmp/root"), arguments);
        let waiting = gate.clone();
        let admitted = std::thread::spawn(move || waiting.admit(action, || false));
        let request = requests
            .recv_timeout(Duration::from_secs(5))
            .expect("the person is asked");
        (request, admitted)
    }

    /// **What a person approves is what runs**: a command's indentation, its
    /// runs of spaces and every character of a line too long for the sheet
    /// are on the sheet, in order.
    #[test]
    fn an_approval_shows_the_call_exactly() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let command = "printf '%s  %s' a  b\n    cd  build && make   -j4 target-with-a-long-name-that-cannot-fit-on-one-row-of-a-narrow-sheet";
        let mut arguments = std::collections::BTreeMap::new();
        arguments.insert("command".to_string(), command.to_string());
        let action =
            crate::approval::Action::new("bash", std::path::Path::new("/tmp/root"), arguments);
        let waiting = gate.clone();
        let admitted = std::thread::spawn(move || waiting.admit(action, || false));
        let request = requests
            .recv_timeout(Duration::from_secs(5))
            .expect("the person is asked");
        let mut prompts = Prompts::default();
        prompts.push_approval(request);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 40)).unwrap();
        terminal
            .draw(|f| prompts.draw(f, crate::tui::Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        let screen = rows.join("\n");
        assert!(
            screen.contains("$ printf '%s  %s' a  b"),
            "spaces collapsed:\n{screen}"
        );
        assert!(
            screen.contains("$     cd  build && make   -j4"),
            "indentation lost:\n{screen}"
        );
        // The long line, read back across its rows, is whole.
        let start = rows
            .iter()
            .position(|row| row.contains("$     cd"))
            .expect("the second line");
        let (left, _) = rows[start].split_once("$     cd").unwrap();
        let column = left.chars().count();
        let joined: String = rows[start..start + 3]
            .iter()
            .map(|row| {
                // Up to the sheet's own right edge.
                let inside: String = row.chars().skip(column).collect();
                inside.trim_end_matches([' ', '│']).to_string()
            })
            .collect();
        assert!(
            joined.contains("target-with-a-long-name-that-cannot-fit-on-one-row-of-a-narrow-sheet"),
            "{joined}"
        );
        prompts.clear_approvals();
        assert_eq!(admitted.join().unwrap(), Admission::Denied);
    }

    /// A prompt that has been on screen, quietly, long enough to take a key.
    fn armed_with(request: crate::approval::Request) -> Prompts {
        let mut prompts = Prompts::default();
        prompts.push_approval(request);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| prompts.draw(f, crate::tui::Theme::default()))
            .unwrap();
        prompts.sheet.shown = Instant::now().checked_sub(ARMING * 2);
        prompts
    }

    fn press(prompts: &mut Prompts, code: KeyCode) -> Done {
        prompts.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// **Esc is "not now"**: it refuses this call and remembers nothing, so
    /// the same call asks again. Deny, chosen on purpose, is remembered and
    /// is on the list the Sandbox sheet shows.
    #[test]
    fn esc_denies_this_call_once_and_deny_is_remembered_visibly() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(request);
        press(&mut prompts, KeyCode::Esc);
        assert_eq!(admitted.join().unwrap(), Admission::Denied);
        assert!(gate.memory().entries().is_empty(), "Esc was remembered");

        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(request);
        press(&mut prompts, KeyCode::Char('d'));
        assert_eq!(admitted.join().unwrap(), Admission::Denied);
        let remembered = gate.memory().entries();
        assert_eq!(remembered.len(), 1);
        assert!(!remembered[0].allowed);
        assert_eq!(remembered[0].label, "write a.txt");
    }

    /// Ctrl-C at an approval cancels the call the way it cancels a running
    /// one, and is never remembered as a refusal.
    #[test]
    fn ctrl_c_cancels_the_call_and_remembers_nothing() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(request);
        let done = prompts.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(done, Done::Interrupt);
        assert_eq!(admitted.join().unwrap(), Admission::Cancelled);
        assert!(gate.memory().entries().is_empty());
    }

    /// Enter answers with the focused chip, and the sheet opens on Allow
    /// once -- never on the session-wide chip or on Deny.
    #[test]
    fn enter_allows_once_from_where_the_prompt_opens() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(request);
        assert_eq!(prompts.sheet.focused().unwrap().id, "allow-once");
        press(&mut prompts, KeyCode::Enter);
        assert_eq!(admitted.join().unwrap(), Admission::Allowed);
        assert!(gate.memory().entries().is_empty(), "once is not remembered");
    }

    /// A denial that stays is on the Sandbox sheet, and Forget there takes it
    /// back: the next identical call is asked again.
    #[test]
    fn a_remembered_denial_is_listed_on_the_ask_sheet_and_forgotten_there() {
        use crate::workbench::{Source, Workbench, sheet::Hit};
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(request);
        press(&mut prompts, KeyCode::Char('d'));
        assert_eq!(admitted.join().unwrap(), Admission::Denied);
        let mut s = crate::tui::ScreenState {
            memory: Some(gate.memory()),
            ..Default::default()
        };
        let mut u = Workbench::default();
        u.open(Source::Sandbox);
        let draw = |u: &mut Workbench, s: &crate::tui::ScreenState| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
            terminal
                .draw(|f| {
                    crate::workbench::render(
                        f,
                        &crate::contract::Conversation::default(),
                        &crate::tui::Notebook::default(),
                        s,
                        &crate::contract::ServedBy::default(),
                        u,
                    )
                })
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let text = draw(&mut u, &s);
        assert!(text.contains("Denied for this session"), "{text}");
        assert!(text.contains("write a.txt"), "{text}");
        assert!(text.contains("⟨ Forget ⟩"), "{text}");
        let index = u
            .top()
            .unwrap()
            .sheet
            .items
            .iter()
            .position(|item| item.id.starts_with("forget:"))
            .unwrap();
        let (rect, _) = u
            .geometry
            .hits
            .iter()
            .find(|(_, action)| *action == Action::Sheet(Hit::Item(index)))
            .cloned()
            .unwrap();
        for kind in [
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        ] {
            let click = crossterm::event::Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            });
            u.event(&click, &mut s, &crate::tui::Notebook::default(), false);
        }
        assert!(gate.memory().entries().is_empty(), "Forget kept the denial");
        let (request, admitted) = asked(&gate, &requests);
        drop(request);
        assert_eq!(
            admitted.join().unwrap(),
            Admission::Denied,
            "the same call reached the person again"
        );
    }

    /// The prompt draws at every size without a panic, and at a usable one
    /// it names the call and every answer.
    #[test]
    fn the_prompt_draws_at_every_size() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = Prompts::default();
        prompts.push_approval(request);
        for (width, height) in [(100, 30), (20, 8), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| prompts.draw(f, crate::tui::Theme::default()))
                .unwrap();
            if width == 100 {
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                for shown in [
                    "APPROVE",
                    "write a.txt",
                    "o · Allow once",
                    "d · Deny for this session",
                    "Esc · Not now",
                ] {
                    assert!(text.contains(shown), "{shown} is missing:\n{text}");
                }
                // As tall as what it holds: a short call leaves the screen's
                // top rows to the conversation behind it.
                assert!(
                    text.chars().take(3 * 100).all(|c| c == ' '),
                    "the prompt fills the screen:\n{text}"
                );
                // The two refusals look alike: the strip says which one is
                // remembered while Deny has the focus.
                let deny = prompts.sheet.items.iter().find(|item| item.id == "deny");
                assert!(
                    deny.is_some_and(|item| item.detail.contains("every identical one")),
                    "{deny:?}"
                );
            }
        }
        prompts.clear_approvals();
        assert_eq!(admitted.join().unwrap(), Admission::Denied);
    }

    /// A command asking to leave the sandbox, on a gate whose proxy refused
    /// `hosts` just before it.
    fn leaving(
        hosts: &[&str],
    ) -> (
        crate::approval::Request,
        std::thread::JoinHandle<Admission>,
        crate::sandbox::proxy::Allowed,
    ) {
        let allowed = crate::sandbox::proxy::Allowed::new(&[], &[]);
        let refused = std::sync::Mutex::new(hosts.iter().map(|h| h.to_string()).collect());
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Sandboxed));
        let gate = gate.with_hosts(crate::approval::Hosts::new(allowed.clone(), move || {
            std::mem::take(&mut *refused.lock().unwrap())
        }));
        let mut arguments = std::collections::BTreeMap::new();
        arguments.insert("command".to_string(), "npm install".to_string());
        arguments.insert(
            crate::permissions::OUTSIDE.to_string(),
            "needs the registry".to_string(),
        );
        let action =
            crate::approval::Action::new("bash", std::path::Path::new("/tmp/root"), arguments);
        let admitted = std::thread::spawn(move || gate.admit(action, || false));
        let request = requests
            .recv_timeout(Duration::from_secs(5))
            .expect("the person is asked");
        (request, admitted, allowed)
    }

    fn screen(prompts: &mut Prompts) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| prompts.draw(f, crate::tui::Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// A refused host turns "leave the sandbox" into "reach a new host":
    /// allowing the host is offered first and opens focused, and "always"
    /// hands the hosts to the loop to save.
    #[test]
    fn a_refused_host_is_offered_before_leaving_the_sandbox() {
        let (request, admitted, allowed) = leaving(&["registry.example.org"]);
        let mut prompts = armed_with(request);
        let text = screen(&mut prompts);
        for shown in [
            "REACH A NEW HOST",
            "h · Allow registry.example.org for this session",
            "w · Always allow registry.example.org",
            "o · Allow once, outside the sandbox",
        ] {
            assert!(text.contains(shown), "{shown} is missing:\n{text}");
        }
        assert!(!text.contains("Allow this call for the session"), "{text}");
        assert_eq!(prompts.sheet.focused().unwrap().id, "allow-host");
        assert_eq!(
            press(&mut prompts, KeyCode::Char('w')),
            Done::KeepHosts(vec!["registry.example.org".to_string()])
        );
        assert_eq!(admitted.join().unwrap(), Admission::HostsAllowed);
        assert!(allowed.permits("registry.example.org"));
    }

    /// With no refused host the same call is "leave the sandbox", with the
    /// answers every approval has.
    #[test]
    fn leaving_with_no_refused_host_is_titled_so() {
        let (request, admitted, _) = leaving(&[]);
        let mut prompts = armed_with(request);
        let text = screen(&mut prompts);
        assert!(text.contains("LEAVE THE SANDBOX"), "{text}");
        assert!(!text.contains("Always allow"), "{text}");
        assert_eq!(press(&mut prompts, KeyCode::Char('h')), Done::Nothing);
        press(&mut prompts, KeyCode::Char('o'));
        assert_eq!(admitted.join().unwrap(), Admission::Allowed);
    }

    /// Keys before the prompt has been on screen for [`ARMING`], and keys
    /// while someone is still typing, are held back; a key after a quiet
    /// half second answers.
    #[test]
    fn a_prompt_arms_only_after_a_quiet_half_second() {
        let mut sheet = Sheet::default();
        sheet.decision = true;
        let now = Instant::now();
        assert!(!sheet.armed(now), "not drawn yet");
        sheet.shown = Some(now);
        assert!(!sheet.armed(now + ARMING / 2));
        assert!(sheet.armed(now + ARMING));
        assert!(
            sheet.hold_back(now + ARMING / 2),
            "a key before it is armed"
        );
        assert!(
            !sheet.armed(now + ARMING),
            "a key typed while unarmed restarts the wait"
        );
        assert!(sheet.armed(now + ARMING / 2 + ARMING));
    }

    /// Two approvals queued, two presses in one burst with no frame between
    /// them: the first answers the approval on screen, and the second is
    /// held -- it never answers the one behind it, which nobody has seen.
    #[test]
    fn a_second_press_never_answers_the_approval_behind_it() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (first, first_admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(first);
        let (second, second_admitted) = asked(&gate, &requests);
        prompts.push_approval(second);
        press(&mut prompts, KeyCode::Char('o'));
        assert_eq!(first_admitted.join().unwrap(), Admission::Allowed);
        assert_eq!(press(&mut prompts, KeyCode::Char('o')), Done::Redraw);
        assert!(prompts.active(), "the second approval is still waiting");
        prompts.cancel();
        assert_eq!(second_admitted.join().unwrap(), Admission::Cancelled);
    }

    /// "Another way" is the same decision reworded: words typed straight
    /// after `a` land in its field rather than being held back again.
    #[test]
    fn words_typed_straight_after_another_way_are_kept() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(request);
        press(&mut prompts, KeyCode::Char('a'));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| prompts.draw(f, crate::tui::Theme::default()))
            .unwrap();
        for c in "use git".chars() {
            press(&mut prompts, KeyCode::Char(c));
        }
        let redirect = prompts.redirect.as_ref().expect("the words sheet is up");
        assert_eq!(redirect.field.text, "use git");
        prompts.cancel();
        assert_eq!(admitted.join().unwrap(), Admission::Cancelled);
    }

    /// The arrows walk a prompt's answers and answer nothing; a digit
    /// answers nothing either.
    #[test]
    fn arrows_and_digits_never_answer_a_prompt() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let (request, admitted) = asked(&gate, &requests);
        let mut prompts = armed_with(request);
        let start = prompts.sheet.focused().unwrap().id.clone();
        press(&mut prompts, KeyCode::Right);
        assert_ne!(prompts.sheet.focused().unwrap().id, start, "→ moves on");
        press(&mut prompts, KeyCode::Left);
        assert_eq!(prompts.sheet.focused().unwrap().id, start, "← moves back");
        press(&mut prompts, KeyCode::Left);
        for digit in '1'..='9' {
            press(&mut prompts, KeyCode::Char(digit));
        }
        assert!(prompts.active(), "an arrow or a digit answered the prompt");
        prompts.cancel();
        assert_eq!(admitted.join().unwrap(), Admission::Cancelled);
    }
}
