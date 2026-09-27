//! Running a filter a model wrote, in an isolate that can do nothing else.
//!
//! The filter is a pure function of text to text and this is the whole of
//! what it is given: a bare V8 context with the standard built-ins and no
//! host globals at all. There is nothing to reach — no tool is bound, no
//! `bindings::install` runs, and the bootstrap that shapes a cell's global
//! object is not evaluated. A filter that wanted to read a file has no name
//! to call.
//!
//! **The text is an argument, never source.** Interpolating a 66 KB log into
//! a program is wasteful and puts an escaping bug in the one place this
//! package exists to make correct: a filter that selects lines is only safe
//! because the lines it selects are the ones it was given, and a mangled
//! quote inside the source is a line it was not. So the filter compiles as a
//! function expression and the log arrives as a `v8::String` parameter.
//!
//! **Every way a filter can go wrong is a validation failure.** It throws, it
//! runs too long, it returns a number, it returns nothing: each of those is
//! an answer the model is shown and asked to correct, never a panic and
//! never a hang the caller inherits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a filter may run before it is stopped.
///
/// A filter is a `split`, a `filter` and a `join` over a few thousand lines,
/// which is single-digit milliseconds; this is three orders of magnitude of
/// headroom and still ends a runaway inside one turn. It is deliberately far
/// below a cell's own limit — a filter is not the model's program, and the
/// caller is a helper the parent is already waiting on.
const FILTER_WALL_CLOCK: Duration = Duration::from_millis(2_000);

/// How often the stop is re-issued once the deadline passes.
///
/// `terminate_execution` is observed at V8's own interrupt checks and there
/// are stretches with none, so a single request can be missed entirely —
/// measured in `isolate.rs`, a cell allocating through `Array.prototype.fill`
/// never saw one. Asking again turns a lost request into a short delay.
const TERMINATE_RETRY: Duration = Duration::from_millis(50);

/// The largest string a filter may return. It must be smaller than its input
/// to pass validation anyway; this stops a filter that returns
/// `text.repeat(10000)` from being the thing that allocates.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// Why a filter produced no usable output.
///
/// Each variant is rendered into the sentence the model sees, for the same
/// reason [`super::reduce_filter::Rejected`] is: a retry that is not told
/// what to do differently is a second identical answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterError {
    /// It did not compile, or it did not evaluate to a function.
    NotAFunction(String),
    /// It threw.
    Threw(String),
    /// It ran past the filter wall clock and was stopped.
    TimedOut,
    /// It returned something that was not a string.
    NotAString,
    /// It returned more than a filter is allowed to return.
    TooLong(usize),
}

impl FilterError {
    /// The sentence the model is shown on its retry.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::NotAFunction(why) => format!(
                "your filter is not a function of the text: {why}. Send one JavaScript \
                 function that takes the whole output as its only argument and returns \
                 the lines to keep."
            ),
            Self::Threw(message) => format!(
                "your filter threw: {message}. It runs against the whole output, so it \
                 must cope with every line the sample showed you."
            ),
            Self::TimedOut => format!(
                "your filter ran for more than {} ms and was stopped. Select lines with \
                 one pass over them; a filter needs no loop over pairs of lines.",
                FILTER_WALL_CLOCK.as_millis()
            ),
            Self::NotAString => "your filter returned something other than a string. \
                 Return the kept lines joined by newlines."
                .to_string(),
            Self::TooLong(bytes) => format!(
                "your filter returned {} bytes, which is more than it was given to \
                 select from. A filter removes lines; it never repeats them.",
                super::preview::thousands(*bytes as u64),
            ),
        }
    }
}

/// Stops the isolate when the deadline passes, and keeps stopping it.
///
/// A smaller relative of `isolate::Watchdog`: this one has no cancellation
/// token, no wait clock and no hard deadline, because the isolate it watches
/// is built for one filter and dropped after it. What it keeps is the retry
/// loop, which is the part that is not optional.
struct Stopper {
    done: Arc<(Mutex<bool>, Condvar)>,
    fired: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stopper {
    fn arm(handle: v8::IsolateHandle, limit: Duration) -> Self {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let fired = Arc::new(AtomicBool::new(false));
        let armed = Instant::now();
        let thread = {
            let done = Arc::clone(&done);
            let fired = Arc::clone(&fired);
            std::thread::spawn(move || {
                let (lock, finished) = &*done;
                let mut guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
                while !*guard {
                    let left = limit.saturating_sub(armed.elapsed());
                    if left.is_zero() {
                        break;
                    }
                    let (next, _) = finished
                        .wait_timeout_while(guard, left, |done| !*done)
                        .unwrap_or_else(PoisonError::into_inner);
                    guard = next;
                }
                if *guard {
                    return;
                }
                // Ordered before the terminate, so `disarm` — which cannot
                // run until this thread drops the lock — sees the verdict.
                fired.store(true, Ordering::SeqCst);
                handle.terminate_execution();
                loop {
                    let (next, _) = finished
                        .wait_timeout_while(guard, TERMINATE_RETRY, |done| !*done)
                        .unwrap_or_else(PoisonError::into_inner);
                    guard = next;
                    if *guard {
                        return;
                    }
                    handle.terminate_execution();
                }
            })
        };
        Self {
            done,
            fired,
            thread: Some(thread),
        }
    }

    fn disarm(mut self) -> bool {
        self.stop();
        self.fired.load(Ordering::SeqCst)
    }

    fn stop(&mut self) {
        {
            let (lock, finished) = &*self.done;
            *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
            finished.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A panic between arming and disarming would leave a thread terminating
/// whatever the isolate runs next.
impl Drop for Stopper {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The two spellings a filter may arrive in, in the order they are tried.
///
/// A model asked for "a function of the text" writes `(text) => …` or
/// `function (text) { … }` — both are expressions once parenthesised. One
/// asked the same question sometimes writes the *body* instead, and refusing
/// that costs a whole retry to teach something the host can simply accept.
/// Both spellings are still one function expression taking one argument, so
/// nothing about what runs is widened.
fn candidates(source: &str) -> [String; 2] {
    [
        format!("({source})"),
        format!("(function (text) {{\n{source}\n}})"),
    ]
}

/// Run `source` against `input` and answer with what it returned.
pub fn run_filter(source: &str, input: &str) -> Result<String, FilterError> {
    super::isolate::initialize_v8_for_filter();
    let mut isolate = v8::Isolate::new(v8::CreateParams::default());
    let handle = isolate.thread_safe_handle();
    let stopper = Stopper::arm(handle, FILTER_WALL_CLOCK);

    let outcome = call(&mut isolate, source, input);

    let timed_out = stopper.disarm();
    // Unconditional: the deadline can pass in the instant between the call
    // returning and the disarm taking the lock, and a termination nobody
    // cancelled would be honoured by the next thing this isolate ran. It is
    // dropped immediately below, but a cancel with nothing pending is free.
    isolate.cancel_terminate_execution();
    if timed_out {
        return Err(FilterError::TimedOut);
    }
    outcome
}

/// The V8 half, kept apart so `run_filter` owns the stopper's lifetime and
/// nothing in here can return without it being disarmed.
fn call(isolate: &mut v8::Isolate, source: &str, input: &str) -> Result<String, FilterError> {
    v8::scope!(let handle_scope, isolate);
    let context = v8::Context::new(handle_scope, v8::ContextOptions::default());
    let scope = &mut v8::ContextScope::new(handle_scope, context);

    let mut last = String::from("it did not compile");
    for candidate in candidates(source) {
        v8::tc_scope!(let try_catch, scope);
        let Some(text) = v8::String::new(try_catch, &candidate) else {
            last = "it is too large to compile".to_string();
            continue;
        };
        let Some(script) = v8::Script::compile(try_catch, text, None) else {
            if try_catch.has_terminated() {
                return Err(FilterError::TimedOut);
            }
            let caught = try_catch.exception();
            last =
                message_of(try_catch, caught).unwrap_or_else(|| "it did not compile".to_string());
            continue;
        };
        let Some(value) = script.run(try_catch) else {
            if try_catch.has_terminated() {
                return Err(FilterError::TimedOut);
            }
            let caught = try_catch.exception();
            last =
                message_of(try_catch, caught).unwrap_or_else(|| "it did not evaluate".to_string());
            continue;
        };
        let Ok(function) = v8::Local::<v8::Function>::try_from(value) else {
            last = "it evaluated to something that is not a function".to_string();
            continue;
        };

        // The call itself, inside the candidate's own `TryCatch` so a throw
        // is caught rather than left pending on the isolate.
        let Some(argument) = v8::String::new(try_catch, input) else {
            return Err(FilterError::TooLong(input.len()));
        };
        let receiver = v8::undefined(try_catch).into();
        let Some(returned) = function.call(try_catch, receiver, &[argument.into()]) else {
            if try_catch.has_terminated() {
                return Err(FilterError::TimedOut);
            }
            let caught = try_catch.exception();
            return Err(FilterError::Threw(
                message_of(try_catch, caught)
                    .unwrap_or_else(|| "an error with no message".to_string()),
            ));
        };
        return read_output(try_catch, returned);
    }
    Err(FilterError::NotAFunction(last))
}

/// What the filter returned, if it is a string small enough to be one.
fn read_output(
    scope: &mut v8::PinScope,
    returned: v8::Local<'_, v8::Value>,
) -> Result<String, FilterError> {
    if !returned.is_string() {
        return Err(FilterError::NotAString);
    }
    let Some(string) = returned.to_string(scope) else {
        return Err(FilterError::NotAString);
    };
    let length = string.utf8_length(scope);
    if length > MAX_OUTPUT_BYTES {
        return Err(FilterError::TooLong(length));
    }
    Ok(string.to_rust_string_lossy(scope))
}

/// The exception's message, bounded so a filter cannot answer its own
/// refusal with a page of text.
///
/// The exception is read out of the `TryCatch` by the caller, so this takes
/// an ordinary scope — the same shape `isolate.rs`'s `caught_error` has, and
/// for the same reason.
fn message_of(
    scope: &mut v8::PinScope,
    exception: Option<v8::Local<'_, v8::Value>>,
) -> Option<String> {
    const MAX: usize = 300;
    let message = exception?.to_rust_string_lossy(scope);
    let mut cut = message.len().min(MAX);
    while cut > 0 && !message.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(message[..cut].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "alpha\nbeta\ngamma\n";

    #[test]
    fn an_arrow_function_selects_lines() {
        let filter = "(text) => text.split('\\n').filter(l => l === 'beta').join('\\n')";
        assert_eq!(run_filter(filter, LOG), Ok("beta".to_string()));
    }

    #[test]
    fn a_bare_body_is_accepted_as_the_function_it_obviously_is() {
        let filter = "return text.split('\\n').filter(l => l.startsWith('g')).join('\\n');";
        assert_eq!(run_filter(filter, LOG), Ok("gamma".to_string()));
    }

    #[test]
    fn the_text_arrives_whole_however_it_is_quoted() {
        // Every character that would have to be escaped in a source literal.
        let awkward = "a 'single' line\nback\\slash \"double\" `tick`\n${not_a_template}\n";
        let filter = "(text) => text";
        let back = run_filter(filter, awkward).expect("an identity filter returns its input");
        assert_eq!(back, awkward, "an argument needs no escaping");
    }

    #[test]
    fn a_filter_that_throws_is_an_answer_and_not_a_crash() {
        let Err(FilterError::Threw(message)) = run_filter("(t) => t.nope()", LOG) else {
            panic!("a throw is reported");
        };
        assert!(message.contains("nope"), "{message}");
    }

    /// Nothing that is a function under *either* reading is refused as one.
    /// `42` is not, but `(function (text) { 42 })` is — a body that returns
    /// nothing — so the honest refusal for it is [`FilterError::NotAString`],
    /// which is the test below. This is the case neither reading rescues.
    #[test]
    fn a_filter_that_is_not_a_function_is_refused_with_what_it_was() {
        let Err(FilterError::NotAFunction(why)) = run_filter("] not javascript [", LOG) else {
            panic!("source that is not a function under either reading is refused");
        };
        assert!(!why.is_empty(), "the refusal says what it was");
    }

    #[test]
    fn a_body_that_returns_nothing_is_refused_for_that_and_not_for_its_shape() {
        assert_eq!(run_filter("42", LOG), Err(FilterError::NotAString));
    }

    #[test]
    fn a_filter_returning_a_number_is_refused() {
        assert_eq!(
            run_filter("(t) => t.length", LOG),
            Err(FilterError::NotAString)
        );
    }

    /// **The bound is tested, not assumed.** `--no-turbofan` is shipped
    /// because the optimising tier elides the interrupt check a termination
    /// is observed at, and a filter is the second place in this crate that
    /// runs a model's JavaScript. If the flag ever stops being set this test
    /// hangs rather than passing quietly.
    #[test]
    fn a_filter_that_never_returns_is_stopped() {
        let started = Instant::now();
        assert_eq!(
            run_filter("(t) => { for (;;) {} }", LOG),
            Err(FilterError::TimedOut)
        );
        assert!(
            started.elapsed() < FILTER_WALL_CLOCK * 4,
            "it stopped at its limit, not eventually: {:?}",
            started.elapsed()
        );
    }

    /// The allocating shape, which is the one a single termination request
    /// can be missed for entirely — `isolate.rs` measured exactly this cell
    /// never stopping at all under TurboFan. Growing one array instead would
    /// prove nothing: V8 ends that itself with `RangeError: Invalid array
    /// length` long before any deadline, which is a throw and not a stop.
    #[test]
    fn an_allocating_runaway_is_stopped_too() {
        assert_eq!(
            run_filter(
                "(t) => { for (;;) { const x = new Array(100).fill('y'); } }",
                LOG
            ),
            Err(FilterError::TimedOut)
        );
    }

    #[test]
    fn a_filter_has_no_host_globals_to_reach() {
        for name in ["bash", "read", "write", "fetch", "require", "process"] {
            let filter = format!("(t) => typeof {name}");
            assert_eq!(
                run_filter(&filter, LOG),
                Ok("undefined".to_string()),
                "{name} must not exist in a filter's world",
            );
        }
    }

    #[test]
    fn the_standard_library_a_filter_needs_is_there() {
        let filter = "(t) => JSON.stringify(t.split('\\n').length) + typeof RegExp";
        assert_eq!(run_filter(filter, LOG), Ok("4function".to_string()));
    }
}
