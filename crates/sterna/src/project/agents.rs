//! Project agent templates are guidance and routing defaults, never grants.
//! A runtime loads this catalog once; edits cannot change a running task.
use crate::sandbox::profile::{Access, Profile};
use crate::wire::Effort;
use std::collections::BTreeMap;
use std::io::Read;

const MAX_BYTES: u64 = 64 * 1024;
const MAX_AGENTS: usize = 128;

#[derive(Debug, Clone)]
pub struct Definition {
    pub instructions: String,
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

impl Definition {
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: toml::Value =
            toml::from_str(text).map_err(|_| "invalid agent TOML".to_string())?;
        let table = value
            .as_table()
            .ok_or("agent definition must be a TOML table")?;
        if table
            .keys()
            .any(|key| !matches!(key.as_str(), "instructions" | "model" | "effort"))
        {
            return Err("agent definition permits only instructions, model, and effort; templates cannot grant tools or permissions".into());
        }
        let string = |key: &str| -> Result<Option<String>, String> {
            table
                .get(key)
                .map(|value| {
                    value
                        .as_str()
                        .filter(|text| !text.trim().is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| format!("agent {key} must be a nonempty string"))
                })
                .transpose()
        };
        let instructions = string("instructions")?.ok_or("agent instructions are required")?;
        let model = string("model")?;
        if let Some(model) = &model {
            crate::config::validate_parent_model(model)?;
        }
        let effort = string("effort")?
            .map(|value| {
                Effort::parse(&value).ok_or_else(|| {
                    "agent effort must be auto, low, medium, high, xhigh, or max".to_string()
                })
            })
            .transpose()?;
        Ok(Self {
            instructions,
            model,
            effort,
        })
    }

    pub fn task(&self, task: &str) -> String {
        format!(
            "## Agent role instructions\n{}\n\n## Delegated task\n{task}",
            self.instructions
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    definitions: BTreeMap<String, Result<Definition, String>>,
    /// Definition files the [`MAX_AGENTS`] bound stopped this catalogue from
    /// loading.
    ///
    /// **A bound that fires is reported where it is felt.** Without this, a
    /// profile whose file exists but sits past the limit resolves to *unknown
    /// agent profile; define `.sterna/agents/NAME.toml`* — an instruction to
    /// create a file that is already there. The count turns that lie into the
    /// truth.
    omitted: usize,
}

impl Catalog {
    pub fn load(profile: &Profile) -> Self {
        let mut catalog = Self::default();
        let Ok(root) = profile.root().canonicalize() else {
            return catalog;
        };
        let directory = root.join(".sterna/agents");
        let Ok(resolved) = directory.canonicalize() else {
            return catalog;
        };
        if !resolved.starts_with(&root) {
            return catalog;
        }
        let Ok(entries) = std::fs::read_dir(&resolved) else {
            return catalog;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        paths.sort();
        catalog.omitted = paths.len().saturating_sub(MAX_AGENTS);
        for path in paths.into_iter().take(MAX_AGENTS) {
            let Some(name) = path
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| valid_name(name))
            else {
                continue;
            };
            let definition = (|| {
                let resolved = path
                    .canonicalize()
                    .map_err(|_| "cannot resolve agent file")?;
                if !resolved.starts_with(&root) {
                    return Err("agent file escapes project root".into());
                }
                let resolved = profile
                    .check("read", Access::Read, &resolved)
                    .map_err(|_| "agent definition read denied by session profile")?;
                if !resolved.is_file() {
                    return Err("agent definition is not a regular file".into());
                }
                let file = std::fs::File::open(&resolved).map_err(|_| "cannot open agent file")?;
                let mut text = String::new();
                file.take(MAX_BYTES + 1)
                    .read_to_string(&mut text)
                    .map_err(|_| "cannot read agent file as UTF-8")?;
                if text.len() as u64 > MAX_BYTES {
                    return Err("agent definition exceeds 64 KiB".into());
                }
                Definition::parse(&text)
            })();
            catalog.definitions.insert(name.into(), definition);
        }
        catalog
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.definitions.keys().map(String::as_str)
    }

    /// How many definition files the catalogue's own bound left unloaded.
    #[must_use]
    pub fn omitted(&self) -> usize {
        self.omitted
    }

    pub fn resolve(&self, name: &str) -> Result<&Definition, String> {
        if !valid_name(name) {
            return Err(
                "agent profile name must contain only letters, digits, hyphens, or underscores"
                    .into(),
            );
        }
        self.definitions
            .get(name)
            .ok_or_else(|| {
                let mut message =
                    format!("unknown agent profile {name}; define .sterna/agents/{name}.toml");
                if self.omitted > 0 {
                    message.push_str(&format!(
                        " ({} further definition file(s) in .sterna/agents were not loaded: this \
                         catalogue holds at most {MAX_AGENTS})",
                        self.omitted
                    ));
                }
                message
            })?
            .as_ref()
            .map_err(Clone::clone)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
