//! The `web` global's arrival: the broker, and the binding that exists only
//! when the configuration reaches something (map 2658).

use super::{Runtime, bindings};

impl Runtime {
    /// Installs host-owned brokered web access without granting shell network access.
    pub fn with_web(self, config: crate::web::WebConfig) -> Result<Self, String> {
        if let Some(effective) = self.state.effective_config.borrow_mut().as_mut() {
            effective.web = config.clone();
        }
        self.state.mcp.borrow_mut().configure_web(config.clone())?;
        Ok(self.with_web_broker(crate::web::WebBroker::new(config)?))
    }

    /// A host embedding may provide its own broker transport; JavaScript cannot.
    ///
    /// **`web` is bound here and nowhere else, and only when the configuration
    /// names a domain or an endpoint** (map 2658, [`crate::web::WebConfig::configured`]):
    /// a session that configured nothing holds no `web`, which is the same
    /// answer the Runtime block gives the model
    /// ([`crate::prompt::render_runtime_reaching`]). A helper's narrowing
    /// still withholds it whatever the configuration says.
    pub fn with_web_broker(self, broker: crate::web::WebBroker) -> Self {
        let mut this = self;
        let configured = broker.config().configured();
        *this.state.web.borrow_mut() = Some(broker);
        if !this.state.web_bound.get() && this.state.globals.installs_with("web", configured) {
            v8::scope!(let handle_scope, &mut this.isolate);
            let context = v8::Local::new(handle_scope, &this.context);
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            bindings::install_web(scope);
            this.state.web_bound.set(true);
        }
        this
    }
}
