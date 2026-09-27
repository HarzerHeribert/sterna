//! A provider tool call becomes a canonical intent, and a canonical intent
//! becomes the same TypeScript a model would have written by hand.
//!
//! This module is where `tool-abi.md` §1's prohibition is enforced: there is
//! no execution here at all. Lowering produces cell source, the existing cell
//! executor runs it, and a direct call therefore cannot diverge from a
//! composed one because there is nothing for it to diverge into.

use serde_json::{Map, Value};

use super::dialect::{Dialect, ParamType, Shape, Target};

/// A canonical, provider-neutral operation — `tool-abi.md` §6.
///
/// The arguments are already renamed into Sterna's spelling and already
/// type-checked, so everything downstream sees one vocabulary whatever
/// façade produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    pub capability: Target,
    pub args: Map<String, Value>,
}

impl Intent {
    /// The capability identity the ledger records.
    #[must_use]
    pub fn capability_id(&self) -> &'static str {
        self.capability.capability_id()
    }

    /// Decodes one provider call into a canonical intent.
    ///
    /// Every rejection is a named canonical error rather than a dropped key:
    /// §22 wants the fact stable internally and the rendering free at the
    /// boundary, and a silently ignored argument is the one outcome that
    /// would make a familiar shape behave unfamiliarly.
    pub fn decode(dialect: Dialect, name: &str, input: &Value) -> Result<Self, AbiError> {
        let Some(shape) = dialect.lookup(name) else {
            return Err(AbiError::unknown_capability(name));
        };
        let Some(object) = input.as_object() else {
            return Err(AbiError {
                kind: ErrorKind::WrongParameterType,
                target: name.to_string(),
                detail: "input must be a JSON object".into(),
                recoverable: true,
            });
        };
        let mut args = Map::new();
        for (key, value) in object {
            let Some(param) = shape.param(key) else {
                return Err(AbiError::unknown_parameter(shape, key));
            };
            if !type_matches(param.ty, value) {
                return Err(AbiError {
                    kind: ErrorKind::WrongParameterType,
                    target: format!("{}.{}", shape.provider_name, key),
                    detail: format!("expected {}", type_name(param.ty)),
                    recoverable: true,
                });
            }
            args.insert(param.canonical.to_string(), value.clone());
        }
        for param in shape.params {
            if param.required && !args.contains_key(param.canonical) {
                return Err(AbiError {
                    kind: ErrorKind::MissingParameter,
                    target: format!("{}.{}", shape.provider_name, param.provider),
                    detail: "required".into(),
                    recoverable: true,
                });
            }
        }
        Ok(Self {
            capability: shape.target,
            args,
        })
    }

    /// The single statement this intent lowers to, binding its result so the
    /// handle mechanism holds the complete observation — §12's requirement
    /// that a provider-native result keep Sterna's handle advantage.
    #[must_use]
    pub fn statement(&self, binding: &str) -> String {
        format!(
            "const {binding} = await {}({});",
            self.capability.callee(),
            js_object_literal(&self.args)
        )
    }

    /// The same call, isolated: the binding is declared ahead of a guarded
    /// call, so a throw inside it stops nothing after it and leaves the name
    /// bound to `undefined` — which the runtime then frees rather than
    /// listing. One line, so a frame of N calls is N lines.
    #[must_use]
    pub fn guarded_statement(&self, binding: &str) -> String {
        format!(
            "let {binding}; try {{ {binding} = await {}({}); }} catch {{}}",
            self.capability.callee(),
            js_object_literal(&self.args)
        )
    }
}

fn type_matches(ty: ParamType, value: &Value) -> bool {
    match ty {
        ParamType::Str => value.is_string(),
        ParamType::Bool => value.is_boolean(),
        ParamType::StrArray => value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string)),
    }
}

fn type_name(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Str => "a string",
        ParamType::Bool => "a boolean",
        ParamType::StrArray => "an array of strings",
    }
}

/// Renders arguments as a JavaScript object literal.
///
/// JSON is a subset of JavaScript expression syntax for these value types,
/// so `serde_json`'s own escaping is the quoting discipline — there is no
/// hand-written string splicing for an argument to escape out of. The two
/// line separators are escaped beyond what JSON requires because they are
/// legal inside a JSON string and were once illegal inside a JavaScript one.
fn js_object_literal(args: &Map<String, Value>) -> String {
    let json = Value::Object(args.clone()).to_string();
    json.replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// One lowered call, and the binding whose handle holds its result.
#[derive(Debug, Clone, PartialEq)]
pub struct LoweredCall {
    /// The provider's own `tool_use` id, so the result correlates back.
    pub id: String,
    /// The provider spelling the model used, for the result envelope.
    pub provider_name: String,
    /// The handle name the result is reachable under.
    pub binding: String,
    /// The canonical capability, for the ledger.
    pub capability: &'static str,
    pub intent: Intent,
}

/// A whole turn's direct calls, lowered into one cell.
///
/// Several independent provider calls become one execution frame — §18's
/// fusion — which costs nothing here because lowering is text and the
/// executor already runs a sequence of statements as one cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Lowered {
    pub source: String,
    pub calls: Vec<LoweredCall>,
}

/// Lowers a turn's provider tool calls into one cell program.
///
/// `cell` is the ordinal the executor will give the frame; it makes every
/// binding name deterministic and unique per turn. The names are handed back
/// to the model in each result envelope, which is how this satisfies
/// `runtime-contract.md` §2 without a parallel naming system: the model is
/// never asked to guess a name it did not write, it is told the name.
///
/// **Provider-native parallel calls are independent, so the frame isolates
/// each one**: for two or more calls every statement is guarded, a denial or
/// throw in one stops none of the others, and the trajectory still records
/// every call in order. A single call keeps the plain statement, so its
/// failure throws and the one `tool_result` is an error.
pub fn lower(
    dialect: Dialect,
    calls: &[(String, String, Value)],
    cell: u64,
) -> Result<Lowered, AbiError> {
    let mut lowered = Vec::new();
    let mut source = String::new();
    let isolated = calls.len() >= 2;
    for (index, (id, name, input)) in calls.iter().enumerate() {
        let intent = Intent::decode(dialect, name, input)?;
        let binding = binding_name(intent.capability, cell, index + 1);
        source.push_str(&if isolated {
            intent.guarded_statement(&binding)
        } else {
            intent.statement(&binding)
        });
        source.push('\n');
        lowered.push(LoweredCall {
            id: id.clone(),
            provider_name: name.clone(),
            binding,
            capability: intent.capability_id(),
            intent,
        });
    }
    Ok(Lowered {
        source,
        calls: lowered,
    })
}

/// A deterministic, announced binding name for a lowered call.
///
/// The cell ordinal is in the name because the persistent REPL scope
/// (`runtime-contract.md` §2) would otherwise let one turn's lowered binding
/// replace an earlier one and take its handle with it.
#[must_use]
pub fn binding_name(capability: Target, cell: u64, index: usize) -> String {
    let base = capability.callee().replace('.', "_");
    format!("{base}_{cell}_{index}")
}

/// A canonical error kind — `tool-abi.md` §22.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    UnknownCapability,
    UnknownParameter,
    MissingParameter,
    WrongParameterType,
}

impl ErrorKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownCapability => "unknown_capability",
            Self::UnknownParameter => "unknown_parameter",
            Self::MissingParameter => "missing_parameter",
            Self::WrongParameterType => "wrong_parameter_type",
        }
    }
}

/// A canonical error: the fact, stable whatever dialect rendered the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiError {
    pub kind: ErrorKind,
    pub target: String,
    pub detail: String,
    pub recoverable: bool,
}

impl AbiError {
    fn unknown_capability(name: &str) -> Self {
        Self {
            kind: ErrorKind::UnknownCapability,
            target: name.to_string(),
            detail: "no such tool in this dialect".into(),
            recoverable: true,
        }
    }

    fn unknown_parameter(shape: &Shape, key: &str) -> Self {
        let declared = shape
            .params
            .iter()
            .map(|param| param.provider)
            .collect::<Vec<_>>()
            .join(", ");
        Self {
            kind: ErrorKind::UnknownParameter,
            target: format!("{}.{key}", shape.provider_name),
            detail: format!("declared parameters are: {declared}"),
            recoverable: true,
        }
    }

    /// The one-sentence form a provider result carries. Familiar phrasing at
    /// the boundary, one canonical fact underneath — §22.
    #[must_use]
    pub fn message(&self) -> String {
        format!("{}: {} ({})", self.kind.as_str(), self.target, self.detail)
    }
}

impl std::fmt::Display for AbiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_read_call_lowers_to_the_same_statement_a_model_would_write() {
        let calls = vec![(
            "call-1".to_string(),
            "Read".to_string(),
            json!({"file_path": "src/runtime.rs"}),
        )];
        let lowered = lower(Dialect::Anthropic, &calls, 7).unwrap();
        assert_eq!(
            lowered.source,
            "const read_7_1 = await read({\"path\":\"src/runtime.rs\"});\n"
        );
        assert_eq!(lowered.calls[0].capability, "read");
        assert_eq!(lowered.calls[0].binding, "read_7_1");
    }

    #[test]
    fn the_provider_parameter_is_renamed_into_sternas_spelling() {
        let intent = Intent::decode(
            Dialect::Anthropic,
            "Edit",
            &json!({"file_path": "a.rs", "old_string": "x", "new_string": "y"}),
        )
        .unwrap();
        assert_eq!(intent.args.get("path").unwrap(), &json!("a.rs"));
        assert_eq!(intent.args.get("old").unwrap(), &json!("x"));
        assert_eq!(intent.args.get("replacement").unwrap(), &json!("y"));
        assert!(intent.args.get("old_string").is_none());
    }

    #[test]
    fn independent_calls_fuse_into_one_frame_in_order() {
        let calls = vec![
            (
                "a".to_string(),
                "Read".to_string(),
                json!({"file_path": "one.rs"}),
            ),
            (
                "b".to_string(),
                "Grep".to_string(),
                json!({"pattern": "Session"}),
            ),
        ];
        let lowered = lower(Dialect::Anthropic, &calls, 3).unwrap();
        let lines: Vec<_> = lowered.source.lines().collect();
        assert_eq!(lines.len(), 2);
        // Each call is isolated: its binding is declared ahead of a guarded
        // call, so a failure in one leaves the other's statement to run.
        assert_eq!(
            lines[0],
            "let read_3_1; try { read_3_1 = await read({\"path\":\"one.rs\"}); } catch {}"
        );
        assert!(lines[1].starts_with("let grep_3_2; try { grep_3_2 = await grep("));
        assert!(lines[1].ends_with("} catch {}"));
        assert_eq!(lowered.calls.len(), 2);
        assert_eq!(lowered.calls[1].id, "b");
    }

    /// The isolating shape must not lean on the runtime's own names: a
    /// `__sterna_` declaration is refused by the compiler and `__sterna_cell` is
    /// the epilogue's private handle.
    #[test]
    fn an_isolated_frame_declares_no_reserved_name() {
        let calls = vec![
            (
                "a".to_string(),
                "Read".to_string(),
                json!({"file_path": "a"}),
            ),
            (
                "b".to_string(),
                "Read".to_string(),
                json!({"file_path": "b"}),
            ),
            ("c".to_string(), "Glob".to_string(), json!({"pattern": "*"})),
        ];
        let lowered = lower(Dialect::Anthropic, &calls, 2).unwrap();
        assert!(!lowered.source.contains("__sterna_"), "{}", lowered.source);
        assert_eq!(lowered.source.lines().count(), 3);
        for (line, call) in lowered.source.lines().zip(&lowered.calls) {
            assert!(line.starts_with(&format!("let {}; try {{", call.binding)));
        }
    }

    /// One call keeps the plain statement, so its failure still throws and
    /// the session answers the one result as an error.
    #[test]
    fn a_single_call_is_not_guarded() {
        let calls = vec![(
            "a".to_string(),
            "Read".to_string(),
            json!({"file_path": "one.rs"}),
        )];
        let lowered = lower(Dialect::Anthropic, &calls, 3).unwrap();
        assert_eq!(
            lowered.source,
            "const read_3_1 = await read({\"path\":\"one.rs\"});\n"
        );
    }

    /// A direct multi-hunk `Edit` lowers to the same `olds`/`replacements`
    /// call a cell would write, arrays intact.
    #[test]
    fn a_multi_hunk_edit_lowers_with_its_arrays_intact() {
        let calls = vec![(
            "e".to_string(),
            "Edit".to_string(),
            json!({
                "file_path": "a.rs",
                "old_strings": ["fn a() {}", "fn b() {}"],
                "new_strings": ["fn a() { 1 }", "fn b() { 2 }"]
            }),
        )];
        let lowered = lower(Dialect::Anthropic, &calls, 4).unwrap();
        let intent = &lowered.calls[0].intent;
        assert_eq!(
            intent.args.get("olds").unwrap(),
            &json!(["fn a() {}", "fn b() {}"])
        );
        assert_eq!(
            intent.args.get("replacements").unwrap(),
            &json!(["fn a() { 1 }", "fn b() { 2 }"])
        );
        assert!(intent.args.get("old_strings").is_none());
        assert!(intent.args.get("old").is_none());
        assert!(
            lowered
                .source
                .starts_with("const edit_4_1 = await edit({\"olds\":[\"fn a() {}\",\"fn b() {}\"]"),
            "{}",
            lowered.source
        );
    }

    #[test]
    fn an_undeclared_parameter_is_refused_and_names_what_is_declared() {
        let error = Intent::decode(
            Dialect::Anthropic,
            "Read",
            &json!({"path": "src/runtime.rs"}),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnknownParameter);
        assert!(error.detail.contains("file_path"));
    }

    #[test]
    fn a_missing_required_parameter_is_refused() {
        let error =
            Intent::decode(Dialect::Anthropic, "Write", &json!({"content": "x"})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::MissingParameter);
        assert_eq!(error.target, "Write.file_path");
    }

    #[test]
    fn a_wrong_type_is_refused_with_the_expected_type_named() {
        let error =
            Intent::decode(Dialect::Anthropic, "Read", &json!({"file_path": 7})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::WrongParameterType);
        assert!(error.detail.contains("a string"));
    }

    #[test]
    fn a_tool_from_another_dialect_is_unknown_here() {
        let error =
            Intent::decode(Dialect::Anthropic, "shell", &json!({"command": "ls"})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnknownCapability);
    }

    #[test]
    fn an_argument_cannot_escape_the_generated_literal() {
        let intent = Intent::decode(
            Dialect::Anthropic,
            "Bash",
            &json!({"command": "echo \"});\nawait write({path:'x',content:'pwned'});//\""}),
        )
        .unwrap();
        let statement = intent.statement("bash_1_1");
        // One statement, and the whole argument is one faithful literal: the
        // decisive property is that it parses back to exactly the arguments,
        // so no byte of it was read as syntax.
        assert_eq!(statement.lines().count(), 1);
        let open = statement.find('(').unwrap();
        let close = statement.rfind(')').unwrap();
        let literal = &statement[open + 1..close];
        let parsed: Value = serde_json::from_str(literal).unwrap();
        assert_eq!(parsed, Value::Object(intent.args.clone()));
        assert!(statement.contains("\\\"});\\nawait write("));
    }

    #[test]
    fn a_line_separator_is_escaped_beyond_what_json_requires() {
        let intent = Intent::decode(
            Dialect::Anthropic,
            "Bash",
            &json!({"command": "echo \u{2028} done"}),
        )
        .unwrap();
        let statement = intent.statement("bash_1_1");
        assert!(statement.contains("\\u2028"));
        assert!(!statement.contains('\u{2028}'));
    }

    #[test]
    fn a_host_call_binding_name_is_a_valid_identifier() {
        let name = binding_name(Target::HostCall("checks.run"), 4, 2);
        assert_eq!(name, "checks_run_4_2");
        assert!(!name.contains('.'));
    }

    #[test]
    fn both_dialects_lower_a_command_to_the_same_capability() {
        let anthropic =
            Intent::decode(Dialect::Anthropic, "Bash", &json!({"command": "ls"})).unwrap();
        let openai = Intent::decode(Dialect::OpenAi, "shell", &json!({"command": "ls"})).unwrap();
        assert_eq!(anthropic.capability, openai.capability);
        assert_eq!(anthropic.args, openai.args);
        assert_eq!(
            anthropic.statement("bash_1_1"),
            openai.statement("bash_1_1")
        );
    }
}
