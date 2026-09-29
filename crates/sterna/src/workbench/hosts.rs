//! The allowed hosts: one switch per package ecosystem and the person's own
//! hosts, which commands reach through Sterna's proxy.
//!
//! Both lists are global settings (`sandbox.ecosystems`, `sandbox.hosts`),
//! read when the sheet opens and saved at every change. When the session
//! runs a proxy, a change reaches its live list too, so the next command
//! sees it; otherwise it applies from the next session, and the sheet says
//! which.
use super::Action;
use super::sheet::{Field, Item, Sheet};
use crate::sandbox::proxy::{ECOSYSTEMS, valid_host};
use crate::tui::ScreenState;

pub const ECOSYSTEMS_KEY: &str = "sandbox.ecosystems";
pub const HOSTS_KEY: &str = "sandbox.hosts";

/// What the sheet shows and changes.
#[derive(Debug, Clone, Default)]
pub struct HostsSheet {
    /// The ecosystems switched on, by settings word, in table order.
    on: Vec<String>,
    /// The person's own hosts, in the order they were added.
    hosts: Vec<String>,
    /// The host being typed.
    pub(super) draft: Field,
}

impl HostsSheet {
    /// The lists as the global settings hold them: unset ecosystems are all
    /// of them, and unset hosts are none.
    pub fn open(s: &ScreenState) -> Self {
        let list = |key: &str| super::facts::global_list(s, key);
        Self {
            on: list(ECOSYSTEMS_KEY)
                .map(|names| {
                    ECOSYSTEMS
                        .iter()
                        .filter(|e| names.iter().any(|n| n == e.name))
                        .map(|e| e.name.to_string())
                        .collect()
                })
                .unwrap_or_else(|| ECOSYSTEMS.iter().map(|e| e.name.to_string()).collect()),
            hosts: list(HOSTS_KEY).unwrap_or_default(),
            draft: Field::default(),
        }
    }

    /// Whether `host` is still wanted by a switch that is on or by the
    /// person's own list -- what keeps it on the live list.
    fn still_wanted(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
            || ECOSYSTEMS
                .iter()
                .filter(|e| self.on.iter().any(|n| n == e.name))
                .any(|e| e.hosts.contains(&host))
    }

    /// Switches one ecosystem; returns the notice.
    pub fn toggle(&mut self, name: &str, s: &ScreenState) -> String {
        let Some(ecosystem) = ECOSYSTEMS.iter().find(|e| e.name == name) else {
            return format!("There is no ecosystem called {name}.");
        };
        let on = !self.on.iter().any(|n| n == name);
        if on {
            self.on = ECOSYSTEMS
                .iter()
                .filter(|e| e.name == name || self.on.iter().any(|n| n == e.name))
                .map(|e| e.name.to_string())
                .collect();
        } else {
            self.on.retain(|n| n != name);
        }
        if let Some(allowed) = &s.allowed {
            for host in ecosystem.hosts {
                if on {
                    allowed.add(host);
                } else if !self.still_wanted(host) {
                    allowed.remove(host);
                }
            }
        }
        let what = format!(
            "{} is {}",
            ecosystem.label,
            if on { "allowed" } else { "off" }
        );
        self.saved(s, ECOSYSTEMS_KEY, &self.on.clone(), what)
    }

    /// Takes the typed host onto the list; returns the notice.
    pub fn add(&mut self, s: &ScreenState) -> String {
        let host = self.draft.text.trim().to_string();
        if host.is_empty() {
            return "Type a host first, like api.example.com.".into();
        }
        if !valid_host(&host) {
            return format!(
                "{host} is not a host name. Write only the name, like api.example.com or \
                 *.example.com, with no https:// and no path."
            );
        }
        if self.hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
            return format!("{host} is already on the list.");
        }
        self.hosts.push(host.clone());
        self.draft = Field::default();
        if let Some(allowed) = &s.allowed {
            allowed.add(&host);
        }
        self.saved(
            s,
            HOSTS_KEY,
            &self.hosts.clone(),
            format!("{host} is allowed"),
        )
    }

    /// Takes one host off the list; returns the notice.
    pub fn remove(&mut self, host: &str, s: &ScreenState) -> String {
        let before = self.hosts.len();
        self.hosts.retain(|h| h != host);
        if self.hosts.len() == before {
            return format!("{host} is not on the list.");
        }
        if let Some(allowed) = &s.allowed
            && !self.still_wanted(host)
        {
            allowed.remove(host);
        }
        self.saved(
            s,
            HOSTS_KEY,
            &self.hosts.clone(),
            format!("{host} is removed"),
        )
    }

    /// Saves `key` and says what happened and when it applies.
    fn saved(&self, s: &ScreenState, key: &str, values: &[String], what: String) -> String {
        match super::facts::save_list(s, key, values) {
            Ok(()) if s.allowed.is_some() => format!("{what}, from the next command on."),
            Ok(()) => format!("{what} from the next session."),
            Err(error) => format!("{what} on this sheet, but it was not saved: {error}"),
        }
    }
}

/// The sheet's rows.
pub(super) fn items(sheet: &mut Sheet, h: &HostsSheet, s: &ScreenState) -> Vec<Item> {
    sheet.title = "Allowed hosts".into();
    sheet.crumbs.clear();
    sheet.status = if s.allowed.is_some() {
        "Changes apply to the next command."
    } else {
        "Changes apply from the next session."
    }
    .into();
    let mut items = vec![Item::heading("Ecosystems")];
    for ecosystem in ECOSYSTEMS {
        items.push(
            Item::toggle(
                format!("eco:{}", ecosystem.name),
                ecosystem.label,
                h.on.iter().any(|n| n == ecosystem.name),
                Action::Ecosystem(ecosystem.name.into()),
            )
            .detail(format!(
                "{} · {}",
                ecosystem.clients,
                ecosystem.hosts.join(" ")
            )),
        );
    }
    items.push(Item::heading("Your hosts"));
    for host in &h.hosts {
        items.push(Item::info(host.clone()));
        items.push(
            Item::run(
                format!("host:{host}"),
                "Remove",
                Action::RemoveHost(host.clone()),
            )
            .detail("Commands reach it in every project.")
            .trail(),
        );
    }
    items.push(
        Item::field("host:new", "Add a host", h.draft.clone())
            .act(Action::AddHost)
            .detail("api.example.com, or *.example.com for every name under it."),
    );
    items.push(Item::run("host:add", "Add", Action::AddHost).trail());
    // What the live list holds beyond the settings: `--allow-host` and the
    // hosts allowed from a prompt, which end with the session.
    let session_only: Vec<String> = s
        .allowed
        .as_ref()
        .map(|allowed| {
            allowed
                .hosts()
                .into_iter()
                .filter(|host| !h.still_wanted(host))
                .collect()
        })
        .unwrap_or_default();
    if !session_only.is_empty() {
        items.push(Item::heading("Allowed for this session only"));
        for host in session_only {
            items.push(Item::info(host));
        }
    }
    items
}
