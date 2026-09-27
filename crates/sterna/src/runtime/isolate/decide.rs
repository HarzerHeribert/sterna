//! The `decide` global's arrival: the configuration that decides whether the
//! running program may ask the decision model anything at all.

use super::{Runtime, bindings};

impl Runtime {
    /// `[decisions]` for this runtime's cells.
    ///
    /// **`decide` is bound here and nowhere else, and only when `[decisions]
    /// model` names a model**: a session that configured none holds no
    /// `decide`, which is the same answer the Runtime block gives the model.
    /// A helper's narrowing withholds it whatever the configuration says —
    /// `helpers-and-subagents.md` makes a helper a leaf, and a leaf that could
    /// buy judgements is a second model loop wearing a helper's name.
    #[must_use]
    pub fn with_decisions(self, decisions: crate::config::DecisionsConfig) -> Self {
        let mut this = self;
        if let Some(config) = this.state.effective_config.borrow_mut().as_mut() {
            config.decisions = decisions.clone();
        }
        this.state.set_decisions(decisions);
        let configured = this.state.decisions_configured();
        if !this.state.decide_bound.get()
            && this
                .state
                .globals
                .installs_reaching("decide", false, configured)
        {
            v8::scope!(let handle_scope, &mut this.isolate);
            let context = v8::Local::new(handle_scope, &this.context);
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            bindings::install_decide(scope);
            this.state.decide_bound.set(true);
        }
        this
    }
}
