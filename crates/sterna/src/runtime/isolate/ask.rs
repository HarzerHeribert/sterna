//! Whether this runtime's cells may put a question to the person.
//!
//! The answer is the session's to give, and it is a *sentence* rather than a
//! flag: `ask` throws it, and "nobody is here", "you switched this off" and
//! "this request changes nothing" are three different things a person would
//! want to be told apart.

use super::Runtime;

impl Runtime {
    /// Lets this runtime's cells ask, or says why they cannot.
    ///
    /// **Refused by default.** A runtime is built long before anyone knows
    /// whether a person is watching it, so the safe answer is the one that
    /// throws: a subagent, the ruler and `--task` all inherit it by doing
    /// nothing, and only a live session hands over the gate that lifts it.
    #[must_use]
    pub fn with_ask(self, refusal: Option<String>) -> Self {
        *self.state.ask_refusal.borrow_mut() = refusal;
        self
    }
}
