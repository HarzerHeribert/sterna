//! One table per provider family says what that family's models are shown,
//! and every row of it names a capability sterna already executes.
//!
//! `tool-abi.md` §4 asks for one descriptor per capability generating both
//! the provider schema and the cell binding, and §5 asks the façades to
//! differ where a family's learned prior differs. Both hold here because a
//! [`Shape`] is only a spelling: its `target` is a name
//! [`crate::tools::registry`] or the runtime already binds, so a dialect can
//! never introduce execution semantics of its own.

use serde_json::{Map, Value, json};

use crate::tools::registry;

/// A provider family, and therefore which spellings are shown.
///
/// Not a string: `tool-abi.md` §5 wants deliberately different façades, and
/// a typo in a string would silently produce an empty tool set rather than a
/// compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `Read`/`Grep`/`Glob`/`Edit`/`Write`/`Bash`/`RunTests`.
    Anthropic,
    /// `shell`/`apply_patch`/`run_tests`, preserving the shell-and-patch
    /// shape that family handles best.
    OpenAi,
}

impl Dialect {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        }
    }

    /// The dialect for a model identifier, defaulting to Anthropic because
    /// that is the family Sterna's own gateway serves.
    #[must_use]
    pub fn for_model(model: &str) -> Self {
        let lowered = model.to_ascii_lowercase();
        if lowered.contains("gpt") || lowered.contains("codex") || lowered.contains("o3") {
            Self::OpenAi
        } else {
            Self::Anthropic
        }
    }

    /// The shapes this dialect shows, in declaration order.
    #[must_use]
    pub fn shapes(self) -> &'static [Shape] {
        match self {
            Self::Anthropic => ANTHROPIC,
            Self::OpenAi => OPENAI,
        }
    }

    /// The shape named `name` in this dialect, or `None`.
    #[must_use]
    pub fn lookup(self, name: &str) -> Option<&'static Shape> {
        self.shapes()
            .iter()
            .find(|shape| shape.provider_name == name)
    }
}

/// What a shape actually runs.
///
/// The invariant this enum carries: a dialect row names an existing
/// capability and nothing else. There is no variant for "a new
/// implementation", which is how §1's prohibition on a second executor is
/// structural here rather than a rule someone has to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A registered tool, by its `registry::ALL` name.
    Tool(&'static str),
    /// A host global's method, as the JavaScript expression that calls it —
    /// `checks.run` is the only one today.
    HostCall(&'static str),
}

impl Target {
    /// The JavaScript callee a lowered cell writes.
    #[must_use]
    pub fn callee(self) -> &'static str {
        match self {
            Self::Tool(name) | Self::HostCall(name) => name,
        }
    }

    /// The capability identity the ledger records, which is the callee for
    /// both variants so a direct call and a cell call are one identity.
    #[must_use]
    pub fn capability_id(self) -> &'static str {
        self.callee()
    }
}

/// The type of one provider parameter, narrow on purpose: every supported
/// capability takes strings, string arrays or a boolean, so a wider type
/// would be a shape no row uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    Str,
    Bool,
    StrArray,
}

impl ParamType {
    fn schema(self) -> Value {
        match self {
            Self::Str => json!({"type": "string"}),
            Self::Bool => json!({"type": "boolean"}),
            Self::StrArray => json!({"type": "array", "items": {"type": "string"}}),
        }
    }
}

/// One parameter, in the provider's spelling and Sterna's.
///
/// The two names exist because §5 wants `file_path` shown to a family that
/// learned `file_path`, while `read` has always taken `path`. The rename is
/// the whole of the adapter's work for most rows.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub provider: &'static str,
    pub canonical: &'static str,
    pub required: bool,
    pub ty: ParamType,
    pub doc: &'static str,
}

impl Param {
    const fn required(
        provider: &'static str,
        canonical: &'static str,
        ty: ParamType,
        doc: &'static str,
    ) -> Self {
        Self {
            provider,
            canonical,
            required: true,
            ty,
            doc,
        }
    }

    const fn optional(
        provider: &'static str,
        canonical: &'static str,
        ty: ParamType,
        doc: &'static str,
    ) -> Self {
        Self {
            provider,
            canonical,
            required: false,
            ty,
            doc,
        }
    }
}

/// One provider-visible tool: its spelling, what it runs, and its parameters.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub provider_name: &'static str,
    /// The named TypeScript input type, declared in [`super::types::PRELUDE`].
    pub input_type: &'static str,
    /// The named TypeScript result type, declared in the same prelude. It is
    /// a name rather than an inline shape so the `complete`/`count` fields
    /// arrive by inheriting `Bounded` instead of being repeated per row.
    pub result_type: &'static str,
    pub target: Target,
    pub params: &'static [Param],
    pub description: &'static str,
}

impl Shape {
    /// The provider-native tool definition, generated rather than written
    /// twice — §4's "one descriptor, every model-facing form".
    #[must_use]
    pub fn tool_definition(&self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for param in self.params {
            let mut schema = param.ty.schema();
            if let Some(object) = schema.as_object_mut() {
                object.insert("description".into(), json!(param.doc));
            }
            properties.insert(param.provider.to_string(), schema);
            if param.required {
                required.push(json!(param.provider));
            }
        }
        json!({
            "name": self.provider_name,
            "description": self.description,
            "input_schema": {
                "type": "object",
                "properties": Value::Object(properties),
                "required": Value::Array(required),
                "additionalProperties": false,
            },
        })
    }

    /// The TypeScript declaration for the same shape — §4, and the form the
    /// model is actually given: a named input type and a named result type,
    /// so the fields that decide correctness are read from the type rather
    /// than recalled from a sentence.
    #[must_use]
    pub fn declaration(&self) -> String {
        format!(
            "declare function {}(input: {}): Promise<{}>;",
            self.provider_name, self.input_type, self.result_type
        )
    }

    /// The parameter for a provider key, or `None` for a key this shape does
    /// not declare. An undeclared key is a decode error, never a silent drop.
    #[must_use]
    pub fn param(&self, provider_key: &str) -> Option<&'static Param> {
        self.params
            .iter()
            .find(|param| param.provider == provider_key)
    }
}

/// The Anthropic-oriented façade — `tool-abi.md` §5's first-class local
/// coding surface.
pub static ANTHROPIC: &[Shape] = &[
    Shape {
        provider_name: "Read",
        input_type: "ReadInput",
        result_type: "ReadResult",
        target: Target::Tool("read"),
        params: &[Param::required(
            "file_path",
            "path",
            ParamType::Str,
            "Path to the file to read, inside the project.",
        )],
        description: "Read a file inside the project. Returns exact content for a modest file, and a bounded exact excerpt plus a handle to the complete observation for a large one.",
    },
    Shape {
        provider_name: "Grep",
        input_type: "SearchInput",
        result_type: "SearchResult",
        target: Target::Tool("grep"),
        params: &[
            Param::required(
                "pattern",
                "pattern",
                ParamType::Str,
                "Regular expression to search for.",
            ),
            Param::optional(
                "path",
                "path",
                ParamType::Str,
                "Directory or file to search; the project root when omitted.",
            ),
        ],
        description: "Search the project for a regular expression. Complete exact matches are retained behind a handle when the result is larger than is useful inline.",
    },
    Shape {
        provider_name: "Glob",
        input_type: "GlobInput",
        result_type: "GlobResult",
        target: Target::Tool("glob"),
        params: &[Param::required(
            "pattern",
            "pattern",
            ParamType::Str,
            "Glob pattern, matched against paths inside the project.",
        )],
        description: "List project paths matching a glob pattern. May include directories, so select a file before reading it.",
    },
    Shape {
        provider_name: "Edit",
        input_type: "EditInput",
        result_type: "EditResult",
        target: Target::Tool("edit"),
        params: &[
            Param::required(
                "file_path",
                "path",
                ParamType::Str,
                "Path to the file to edit.",
            ),
            Param::optional(
                "old_string",
                "old",
                ParamType::Str,
                "Exact text to replace; must match exactly once. Give old_string/new_string or old_strings/new_strings.",
            ),
            Param::optional(
                "new_string",
                "replacement",
                ParamType::Str,
                "Replacement text.",
            ),
            Param::optional(
                "old_strings",
                "olds",
                ParamType::StrArray,
                "Several exact texts to replace, each matching exactly once; applied together or not at all.",
            ),
            Param::optional(
                "new_strings",
                "replacements",
                ParamType::StrArray,
                "Replacement for each old_strings entry, same length and order.",
            ),
        ],
        description: "Replace one exact, uniquely matching string in a file, or several at once with old_strings/new_strings as one atomic edit. Stale, ambiguous, missing, overlapping and no-op edits are refused without writing.",
    },
    Shape {
        provider_name: "Write",
        input_type: "WriteInput",
        result_type: "WriteResult",
        target: Target::Tool("write"),
        params: &[
            Param::required(
                "file_path",
                "path",
                ParamType::Str,
                "Path to write, creating parent directories.",
            ),
            Param::optional(
                "content",
                "content",
                ParamType::Str,
                "Whole file content. Give exactly one of content or lines.",
            ),
            Param::optional(
                "lines",
                "lines",
                ParamType::StrArray,
                "Whole file as logical lines; sterna supplies the separators.",
            ),
        ],
        description: "Replace one whole file, or create a new one. Prefer lines for script or literal text so quoting survives intact.",
    },
    Shape {
        provider_name: "Bash",
        input_type: "CommandInput",
        result_type: "CommandResult",
        target: Target::Tool("bash"),
        params: &[Param::required(
            "command",
            "command",
            ParamType::Str,
            "Command line to run under the session's sandbox grant.",
        )],
        description: "Run a command line under the sandbox grant. Inspect exit_code before treating the result as successful; stdout and stderr remain exact evidence.",
    },
    Shape {
        provider_name: "RunTests",
        input_type: "CheckInput",
        result_type: "CheckResult",
        target: Target::HostCall("checks.run"),
        params: &[
            Param::required(
                "name",
                "name",
                ParamType::Str,
                "Configured check name from .sterna/checks.toml.",
            ),
            Param::optional(
                "force",
                "force",
                ParamType::Bool,
                "Force a fresh execution instead of reusing an unchanged-input observation.",
            ),
        ],
        description: "Run a configured named check. The result says whether it executed freshly or reused an unchanged-input observation, and never claims a reused result ran.",
    },
];

/// The OpenAI-oriented façade — §5's shell-and-patch surface.
///
/// `shell` targets the same `bash` capability the Anthropic dialect calls
/// `Bash`; the two spellings are one execution path, which is what §5 means
/// by a skin.
pub static OPENAI: &[Shape] = &[
    Shape {
        provider_name: "shell",
        input_type: "CommandInput",
        result_type: "CommandResult",
        target: Target::Tool("bash"),
        params: &[Param::required(
            "command",
            "command",
            ParamType::Str,
            "Command line to run under the session's sandbox grant.",
        )],
        description: "Run a shell command under the sandbox grant. A mechanically recognised search or read form is lowered to the equivalent Sterna capability with identical semantics; anything else executes faithfully as a sandboxed process.",
    },
    Shape {
        provider_name: "apply_patch",
        input_type: "EditInput",
        result_type: "EditResult",
        target: Target::Tool("edit"),
        params: &[
            Param::required(
                "file_path",
                "path",
                ParamType::Str,
                "Path the patch applies to.",
            ),
            Param::optional(
                "old_string",
                "old",
                ParamType::Str,
                "Exact text the patch replaces; must match exactly once. Give old_string/new_string or old_strings/new_strings.",
            ),
            Param::optional(
                "new_string",
                "replacement",
                ParamType::Str,
                "Text the patch installs.",
            ),
            Param::optional(
                "old_strings",
                "olds",
                ParamType::StrArray,
                "Several exact texts the patch replaces, each matching exactly once; applied together or not at all.",
            ),
            Param::optional(
                "new_strings",
                "replacements",
                ParamType::StrArray,
                "Text installed for each old_strings entry, same length and order.",
            ),
        ],
        description: "Apply one exact replacement to a file, or several hunks at once with old_strings/new_strings as one atomic patch. A patch whose target state does not match is rejected without writing, and the observed diff is recorded.",
    },
    Shape {
        provider_name: "run_tests",
        input_type: "CheckInput",
        result_type: "CheckResult",
        target: Target::HostCall("checks.run"),
        params: &[
            Param::required(
                "name",
                "name",
                ParamType::Str,
                "Configured check name from .sterna/checks.toml.",
            ),
            Param::optional(
                "force",
                "force",
                ParamType::Bool,
                "Force a fresh execution instead of reusing an unchanged-input observation.",
            ),
        ],
        description: "Run a configured named check. The result distinguishes a fresh execution from a reused unchanged-input observation.",
    },
];

/// Every dialect, for a caller that must cover all of them.
pub static ALL: [Dialect; 2] = [Dialect::Anthropic, Dialect::OpenAi];

/// Whether `target` names something that actually exists to run.
///
/// The predicate behind §1's prohibition: a dialect row that named no
/// registered tool and no bound host call would be a second implementation
/// waiting to be written, and `tests/abi.rs` fails on it rather than letting
/// it ship.
#[must_use]
pub fn target_exists(target: Target) -> bool {
    match target {
        Target::Tool(name) => registry::lookup(name).is_some(),
        Target::HostCall(name) => HOST_CALLS.contains(&name),
    }
}

/// Host-global methods a dialect row may target. Written down because a
/// host call is bound in the runtime rather than declared in the registry,
/// so there is no table to look it up in.
pub static HOST_CALLS: [&str; 1] = ["checks.run"];

/// Every `(alias, registry name)` pair, across every dialect.
///
/// Both dialects' spellings are bound inside a cell, not just the active
/// one, and that is deliberate: an alias is a second name for a capability
/// the session already grants, so it widens nothing, and a model reaching
/// for the other family's spelling then works instead of failing on a name
/// it was confident about. §14 asks for the parent's own dialect in the
/// cell; binding both is that promise kept twice over.
pub fn tool_aliases() -> impl Iterator<Item = (&'static str, &'static str)> {
    ALL.into_iter().flat_map(|dialect| {
        dialect
            .shapes()
            .iter()
            .filter_map(|shape| match shape.target {
                Target::Tool(name) => Some((shape.provider_name, name)),
                Target::HostCall(_) => None,
            })
    })
}

/// Every alias that targets `host_call`, for the runtime that binds it.
#[must_use]
pub fn host_call_aliases(host_call: &str) -> Vec<&'static str> {
    ALL.into_iter()
        .flat_map(|dialect| dialect.shapes().iter())
        .filter(|shape| matches!(shape.target, Target::HostCall(name) if name == host_call))
        .map(|shape| shape.provider_name)
        .collect()
}

/// Whether `name` is a dialect spelling the runtime binds.
///
/// `crate::runtime::cell::is_host_function` reads this so a top-level
/// binding cannot take an alias's name. Its doc comment records what a
/// hand-written list of protected names once cost: `const write = 1;`
/// compiled and the whole cell ran. An alias is exactly the same hazard
/// under a different spelling, so it is answered from the same table rather
/// than from a second list.
#[must_use]
pub fn is_dialect_name(name: &str) -> bool {
    lookup_any(name).is_some()
}

/// The shape named `name` in any dialect.
///
/// Used where a spelling arrives without its dialect — inside a cell, where
/// both families' names are bound. A name in two dialects would resolve to
/// the first; none is, and `no_spelling_means_two_things` fails the gate if
/// one ever does.
#[must_use]
pub fn lookup_any(name: &str) -> Option<&'static Shape> {
    ALL.into_iter().find_map(|dialect| dialect.lookup(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dialect_row_targets_a_capability_that_exists() {
        for dialect in ALL {
            for shape in dialect.shapes() {
                assert!(
                    target_exists(shape.target),
                    "{} row {} targets {:?}, which nothing implements",
                    dialect.as_str(),
                    shape.provider_name,
                    shape.target
                );
            }
        }
    }

    #[test]
    fn both_dialects_reach_the_same_capability_for_a_shell_command() {
        let anthropic = Dialect::Anthropic.lookup("Bash").unwrap();
        let openai = Dialect::OpenAi.lookup("shell").unwrap();
        assert_eq!(anthropic.target, openai.target);
        assert_eq!(
            anthropic.target.capability_id(),
            openai.target.capability_id()
        );
    }

    #[test]
    fn a_generated_definition_declares_only_its_own_parameters() {
        let shape = Dialect::Anthropic.lookup("Edit").unwrap();
        let definition = shape.tool_definition();
        let properties = definition["input_schema"]["properties"]
            .as_object()
            .unwrap();
        assert_eq!(properties.len(), 5);
        assert!(properties.contains_key("old_string"));
        assert!(properties.contains_key("old_strings"));
        assert_eq!(properties["old_strings"]["type"], json!("array"));
        assert_eq!(
            definition["input_schema"]["additionalProperties"],
            json!(false)
        );
        // Only the path is required: a call gives one of the two pairs, and
        // the kernel refuses a call that gives neither or both.
        let required = definition["input_schema"]["required"].as_array().unwrap();
        assert_eq!(required, &vec![json!("file_path")]);
    }

    #[test]
    fn an_optional_parameter_is_not_required_in_the_schema() {
        let shape = Dialect::Anthropic.lookup("Grep").unwrap();
        let definition = shape.tool_definition();
        let required = definition["input_schema"]["required"].as_array().unwrap();
        assert_eq!(required, &vec![json!("pattern")]);
    }

    #[test]
    fn every_declaration_names_types_the_prelude_declares() {
        for dialect in ALL {
            for shape in dialect.shapes() {
                let declaration = shape.declaration();
                assert!(declaration.contains(shape.input_type));
                assert!(declaration.contains(shape.result_type));
                for ty in [shape.input_type, shape.result_type] {
                    assert!(
                        super::super::types::PRELUDE.contains(&format!("type {ty} ")),
                        "{} names {ty}, which the prelude does not declare",
                        shape.provider_name
                    );
                }
            }
        }
    }

    #[test]
    fn the_schema_still_declares_every_parameter_by_its_provider_name() {
        for dialect in ALL {
            for shape in dialect.shapes() {
                let definition = shape.tool_definition();
                let properties = definition["input_schema"]["properties"]
                    .as_object()
                    .unwrap();
                assert_eq!(properties.len(), shape.params.len());
                for param in shape.params {
                    assert!(properties.contains_key(param.provider));
                }
            }
        }
    }

    #[test]
    fn a_model_identifier_selects_its_family() {
        assert_eq!(Dialect::for_model("claude-opus-4"), Dialect::Anthropic);
        assert_eq!(Dialect::for_model("gpt-6-astra"), Dialect::OpenAi);
        assert_eq!(Dialect::for_model("codex-mini"), Dialect::OpenAi);
    }

    #[test]
    fn an_unknown_provider_tool_name_resolves_to_nothing() {
        assert!(Dialect::Anthropic.lookup("Frobnicate").is_none());
        assert!(Dialect::Anthropic.lookup("shell").is_none());
    }
}
