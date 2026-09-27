//! Task-scoped standing programs. Callables stay in the original isolate; source is
//! retained only for the rollout, never used to replay a completed execution.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerInfo {
    pub name: String,
    pub runs: u64,
    pub drained: u64,
    pub error: Option<String>,
    pub active: bool,
}

pub(crate) struct Handler {
    pub registered: u64,
    pub id: String,
    /// Independent of the display string: the first binding may equal `id`.
    pub name_bound: bool,
    pub info: HandlerInfo,
    pub kind: Option<String>,
    pub source_filter: Option<String>,
    pub source: String,
    pub program: Option<v8::Global<v8::Function>>,
}

#[derive(Default)]
pub(crate) struct Handlers {
    pub entries: RefCell<Vec<Handler>>,
    pub running: Cell<bool>,
    pub notices: RefCell<Vec<String>>,
    pub processed: Cell<bool>,
    pub delivery: Cell<u64>,
}
impl Handlers {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }
    pub fn off(&self, name: &str, error: Option<String>) -> bool {
        let id = self
            .entries
            .borrow()
            .iter()
            .rev()
            .find(|h| h.info.name == name)
            .map(|h| h.id.clone());
        id.is_some_and(|id| self.off_id(&id, error))
    }
    pub fn off_id(&self, id: &str, error: Option<String>) -> bool {
        let mut entries = self.entries.borrow_mut();
        let Some(handler) = entries.iter_mut().find(|h| h.id == id) else {
            return false;
        };
        if !handler.info.active {
            return true;
        }
        handler.info.active = false;
        handler.program.take();
        handler.source.clear();
        if let Some(error) = error {
            let class: String = error
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(64)
                .collect();
            let class = if class.is_empty() {
                "Error".into()
            } else {
                class
            };
            self.notices.borrow_mut().push(format!(
                "handler {} disabled: {class}",
                handler.info.name.chars().take(80).collect::<String>()
            ));
            handler.info.error = Some(class);
        }
        true
    }
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.notices.borrow_mut().clear();
    }
}
