//! One cell, from the model's TypeScript to the JavaScript V8 runs —
//! `runtime-contract.md` §1 and §2.
//!
//! **Types are erased in place, never re-printed.** Every TypeScript-only
//! span is overwritten with spaces (newlines kept), so the JavaScript handed
//! to V8 has the model's own program at the model's own line and column. That
//! is what lets `runtime-contract.md` §5's "the source line and column inside
//! the model's own program" be a subtraction rather than a source map, and it
//! is why this module uses `oxc`'s parser and not its transformer: a
//! transformer's output is correct JavaScript at the wrong positions.
//!
//! **The persistent scope is the context's global object, and a cell is a
//! function body.** The model's statements are wrapped in one `async function`
//! so that a top-level `return` is legal (§1) and a redeclaration in a later
//! cell is a fresh function scope rather than a `SyntaxError` (§2).
//!
//! **A binding persists with the value it holds when the cell ends**, which
//! is what `runtime-contract.md` §2 promises and a capture taken only where
//! the binding is made cannot give: a counter bumped in a loop body would
//! come back at its first value. So every top-level `let` and `const` is
//! rewritten to `var` — padded to the same width, so no column moves — and
//! the body runs inside one `try`/`finally`. `var` is function-scoped, so
//! the `finally` reads each name after a fall-off, a `return` or a throw and
//! captures what it holds then. The price is that `const` is not immutable
//! and there is no temporal dead zone *within* one cell: a REPL scope is not
//! a module scope, which §2 already says for redeclaration.
//!
//! **The declaration-line capture stays**, because it is all there is when
//! the `finally` never runs: a heap-limit termination, and an `await`
//! nothing can settle. It is also what a `class` gets, since a class binding
//! keeps its own block scope and no same-width rewrite hoists one. So
//! reassigning a top-level `class` or `function` name inside the cell that
//! declared it does not persist; every other binding is read again.

use crate::tools::registry;
use oxc::allocator::Allocator;
use oxc::ast::ast::{
    AccessorProperty, AssignmentTarget, BindingIdentifier, BindingPattern, Class, Expression,
    FormalParameter, Function, IdentifierReference, MethodDefinition, MethodDefinitionType,
    Program, PropertyDefinition, PropertyDefinitionType, Statement, TSAsExpression,
    TSClassImplements, TSEnumDeclaration, TSExternalModuleDeclaration, TSGlobalDeclaration,
    TSImportEqualsDeclaration, TSInstantiationExpression, TSInterfaceDeclaration,
    TSNamespaceDeclaration, TSNonNullExpression, TSSatisfiesExpression, TSType,
    TSTypeAliasDeclaration, TSTypeAnnotation, TSTypeAssertion, TSTypeParameterDeclaration,
    TSTypeParameterInstantiation, UnaryExpression, VariableDeclaration, VariableDeclarationKind,
    VariableDeclarator,
};
use oxc::ast_visit::{Visit, walk};
use oxc::parser::{ParseOptions, Parser};
use oxc::span::{GetSpan, SourceType, Span};
use oxc::syntax::scope::ScopeFlags;

/// The name every runtime-owned binding in a generated cell starts with. A
/// model program that declares one is refused rather than silently losing
/// its own bindings — see [`CellError::ReservedName`].
pub const RESERVED_PREFIX: &str = "__sterna_";

/// Names JavaScript binds in every function body without anyone declaring
/// them. **A cell is a function body** (the module header's second
/// invariant), so these are always defined inside one and are on no global
/// object — which is exactly the shape that looks like an undefined name to
/// a scan that only knows declarations and globals.
const IMPLICIT_FUNCTION_BINDINGS: [&str; 1] = ["arguments"];

/// The names the isolate puts on the persistent scope that are not tools: the
/// three handle functions, `yieldNow`, and the MCP and message doors.
///
/// `console` is deliberately absent: it is not a capability, shadowing it
/// costs the model only its own logging, and a program that assigns to it is
/// doing something it can undo.
const NON_TOOL_HOST_FUNCTIONS: [&str; 11] = [
    "keep", "free", "handles", "yieldNow", "answer", "ask", "mcp", "on", "off", "checks", "web",
];

/// Whether `name` is one the isolate puts on the persistent scope. A
/// top-level binding may not take one, because the capture that makes a
/// handle persist would overwrite it for the whole task.
///
/// **The tool half is asked of the registry rather than repeated here**, so a
/// tool the registry gains is protected by that alone. While this was one
/// hand-written list it named four tools of seven, and `const write = 1;`
/// compiled, ran the whole cell, and told the model afterwards that its
/// `globalThis` was frozen.
pub fn is_host_function(name: &str) -> bool {
    registry::lookup(name).is_some()
        || NON_TOOL_HOST_FUNCTIONS.contains(&name)
        || crate::abi::dialect::is_dialect_name(name)
}

/// The one runtime binding a generated cell carries: the host object whose
/// `s` captures a completed binding and whose `e` marks that the body ran
/// off its end. It is a parameter, not a global, so it is gone the moment
/// the cell's function returns.
const HOST: &str = "__sterna_cell";

/// The cell-local record of which top-level bindings the program actually
/// reached. `var` hoists, so without it the epilogue would read every
/// declaration a throw never reached as `undefined` and replace whatever
/// handle an earlier cell had made under that name. It is `__sterna_`-prefixed,
/// so [`CellError::ReservedName`] already keeps a program from touching it.
const MADE: &str = "__sterna_made";

/// How many lines the wrapper puts before the model's first line. The
/// prologue is exactly one line, so a V8 position maps back by subtracting
/// it and columns need no adjustment at all.
pub const LINE_OFFSET: u32 = 1;

/// Every TypeScript construct sterna advertises as erasable, each with a
/// one-line program that returns `"ok"` when the construct erased correctly.
///
/// **This table is the contract.** `tests/typescript_contract.rs` compiles
/// every program, checks that no line changed width, and runs it in a real
/// isolate; a construct is advertised in `model-contract.md` only if its row
/// is here.
pub const ERASABLE_CONSTRUCTS: &[(&str, &str)] = &[
    (
        "type annotation on const",
        "const n: number = 1;\nreturn n === 1 ? \"ok\" : \"no\";\n",
    ),
    (
        "type annotation on let",
        "let s: string = \"o\";\ns = s + \"k\";\nreturn s;\n",
    ),
    (
        "type annotation on a parameter",
        "function f(x: string) { return x; }\nreturn f(\"ok\");\n",
    ),
    (
        "type annotation on a return",
        "function f(): string { return \"ok\"; }\nreturn f();\n",
    ),
    (
        "as const",
        "const t = { text: \"x\", status: \"ok\" as const };\nreturn t.status;\n",
    ),
    (
        "as a declared interface",
        "interface Row { v: string }\nconst r = { v: \"ok\" } as Row;\nreturn r.v;\n",
    ),
    (
        "as an undeclared type name",
        "const r = { v: \"ok\" } as Undeclared;\nreturn r.v;\n",
    ),
    (
        "satisfies",
        "const r = { v: \"ok\" } satisfies { v: string };\nreturn r.v;\n",
    ),
    (
        "<T>expr assertion",
        "const r = <{ v: string }>{ v: \"ok\" };\nreturn r.v;\n",
    ),
    (
        "generic call f<T>(x)",
        "function id(x) { return x; }\nreturn id<string>(\"ok\");\n",
    ),
    (
        "generic arrow",
        "const id = <T,>(x: T) => x;\nreturn id(\"ok\");\n",
    ),
    (
        "generic function",
        "function first<T>(xs: T[]): T { return xs[0]; }\nreturn first([\"ok\"]);\n",
    ),
    (
        "generic class",
        "class Box<T> { v: T; constructor(v: T) { this.v = v; } }\nreturn new Box(\"ok\").v;\n",
    ),
    (
        "non-null !",
        "const m = new Map<string, string>([[\"k\", \"ok\"]]);\nreturn m.get(\"k\")!;\n",
    ),
    (
        "optional parameter x?: T",
        "function f(x?: string) { return x ?? \"ok\"; }\nreturn f();\n",
    ),
    (
        "definite assignment let x!: T",
        "let x!: string;\nx = \"ok\";\nreturn x;\n",
    ),
    (
        "type alias",
        "type Marker = \"ok\";\nconst m: Marker = \"ok\";\nreturn m;\n",
    ),
    (
        "interface",
        "interface Row { v: string }\nconst r: Row = { v: \"ok\" };\nreturn r.v;\n",
    ),
    (
        "declare const",
        "declare const missing: number;\nreturn \"ok\";\n",
    ),
    (
        "declare function",
        "declare function foo(a: number): void;\nreturn \"ok\";\n",
    ),
    (
        "declare class",
        "declare class Z { n: number }\nreturn \"ok\";\n",
    ),
    (
        "abstract class with an abstract member",
        "abstract class Shape {\n  abstract name(): string;\n  describe(): string { return this.name(); }\n}\nclass Ok extends Shape { name() { return \"ok\"; } }\nreturn new Ok().describe();\n",
    ),
    (
        "readonly/public/private/protected/override members",
        "class Base { protected v = \"o\"; describe(): string { return this.v; } }\nclass K extends Base {\n  public readonly a = \"k\";\n  private b = \"\";\n  override describe(): string { return super.describe() + this.a; }\n}\nreturn new K().describe();\n",
    ),
    (
        "implements",
        "interface Named { name(): string }\nclass K implements Named { name() { return \"ok\"; } }\nreturn new K().name();\n",
    ),
];

/// Every TypeScript construct sterna refuses, with a one-line example and the
/// alternative [`CellError::NotErasable`]'s message names. `tsc` compiles
/// each of these by *emitting* code; a type eraser cannot, and running a
/// program that means something else is worse than refusing it.
///
/// `tests/typescript_contract.rs` runs every example and checks the refusal
/// arrives before execution, at the model's own line, naming the construct
/// and the alternative.
pub const NOT_ERASABLE_CONSTRUCTS: &[(&str, &str, &str)] = &[
    (
        "enum",
        "enum Colour { Red, Green }",
        "a plain object: `const Colour = { Red: 0, Green: 1 } as const`",
    ),
    (
        "namespace",
        "namespace N { export const x = 1; }",
        "a plain object: `const N = { x: 1 }`",
    ),
    (
        "parameter property",
        "class K { constructor(private x: number) {} }",
        "a plain parameter and a field assignment: `constructor(x: number) { this.x = x; }`",
    ),
    (
        "declare global",
        "declare global { const x: number; }",
        "a top-level `declare const x: T` for the one name you need; a cell has no global \
         scope to augment",
    ),
    (
        "declare module",
        "declare module \"m\" { export const x: number; }",
        "nothing: a cell has no modules to describe; `declare const x: T` names a binding an \
         earlier cell made",
    ),
    (
        "import =",
        "import fs = require(\"fs\");",
        "no import at all: every capability is already a global; `const alias = existing;` \
         makes an alias",
    ),
];

/// What [`CellError::NotErasable`]'s message tells the model to write
/// instead, from [`NOT_ERASABLE_CONSTRUCTS`].
fn alternative_for(construct: &str) -> Option<&'static str> {
    NOT_ERASABLE_CONSTRUCTS
        .iter()
        .find(|(name, _, _)| *name == construct)
        .map(|(_, _, alternative)| *alternative)
}

/// A cell's source, erased and wrapped, with everything the isolate needs to
/// map V8's answers back onto the model's own program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCell {
    /// The JavaScript V8 compiles: one parenthesised `async function`.
    pub javascript: String,
    /// Every top-level binding name the cell declares, in source order.
    pub declared: Vec<String>,
    /// The name V8 knows this script by, and the only script name whose
    /// stack frames reach the model (`runtime-contract.md` §5).
    pub script_name: String,
    /// Every name the cell **reads** and never binds anywhere in itself, with
    /// the byte offset of its first mention.
    ///
    /// Not yet an error: a free name is legal when the global object carries
    /// it — a handle from an earlier cell, a host function, a JavaScript
    /// built-in — and only the isolate knows that set. `Runtime::run_cell`
    /// asks it, and reports what remains before running anything.
    ///
    /// **Over-approximates what is bound, so it can only miss.** A name bound
    /// anywhere in the program counts as bound everywhere in it, which
    /// ignores scoping; the cost is a use-before-declare this does not
    /// notice, and the benefit is that no correct program is ever accused.
    /// That direction is deliberate: a false positive would have the model
    /// "fix" code that was right.
    pub free_names: Vec<(String, u32)>,
}

/// Why a cell could not be turned into JavaScript.
///
/// Each variant is a *value* the isolate turns into a throw in the model's
/// own turn slot; none of them is an error the runtime reports upwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellError {
    /// The cell reads a name it binds nowhere and the global object does not
    /// carry — a typo, or a handle that was freed or never existed. Caught
    /// before the cell runs, so the model is told in the turn that wrote it
    /// rather than in the next one.
    UndefinedName {
        name: String,
        line: u32,
        column: u32,
    },
    /// The TypeScript did not parse. The position is the model's own.
    Parse {
        message: String,
        line: u32,
        column: u32,
    },
    /// A TypeScript construct that has no JavaScript to erase to — an
    /// `enum`, a `namespace`, an `import =`, or a constructor parameter
    /// property. `tsc` compiles these by *emitting* code; a type eraser
    /// cannot, and pretending otherwise would run a program that means
    /// something else.
    NotErasable {
        construct: &'static str,
        line: u32,
        column: u32,
    },
    /// A top-level binding whose name is the runtime's own. The position is
    /// the declaration's, so `runtime-contract.md` §5's "the source line and
    /// column inside the model's own program" is one the program has.
    ReservedName {
        name: String,
        line: u32,
        column: u32,
    },
    /// A top-level binding whose name is one of the host functions. It would
    /// be captured onto the persistent scope and would shadow that function
    /// for **every later cell of the task**, with nothing the model could
    /// write to get it back.
    ShadowsHostFunction {
        name: String,
        line: u32,
        column: u32,
    },
}

impl CellError {
    pub fn class(&self) -> &'static str {
        match self {
            // The class V8 would have used had the cell run, because that is
            // what the model is being spared: `ReferenceError` naming a name
            // that is not defined.
            CellError::UndefinedName { .. } => "ReferenceError",
            CellError::Parse { .. } => "SyntaxError",
            CellError::NotErasable { .. } => "TypeScriptNotErasable",
            CellError::ReservedName { .. } => "ReservedName",
            CellError::ShadowsHostFunction { .. } => "ShadowsHostFunction",
        }
    }

    pub fn message(&self) -> String {
        match self {
            CellError::UndefinedName { name, .. } => format!(
                "`{name}` is not defined: the cell reads it, binds it nowhere, and no handle or \
                 host function has that name. Nothing ran. `handles()` lists what you can address. \
                 If this name came from file or script text, use `write`/`edit` line arrays with \
                 one double-quoted JavaScript string per logical line; do not use a template \
                 literal or heredoc."
            ),
            CellError::Parse { message, .. } => message.clone(),
            CellError::NotErasable { construct, .. } => {
                let mut text = format!(
                    "`{construct}` is TypeScript that has no JavaScript to erase to; sterna strips \
                     types, it does not compile them."
                );
                if let Some(alternative) = alternative_for(construct) {
                    text.push_str(&format!(" Use {alternative} instead."));
                }
                text
            }
            CellError::ReservedName { name, .. } => format!(
                "`{name}` starts with `{RESERVED_PREFIX}`, which the runtime reserves for its own \
                 bindings"
            ),
            CellError::ShadowsHostFunction { name, .. } => format!(
                "`{name}` is a host function; binding it would replace it on the persistent scope \
                 for the rest of the task and nothing could put it back"
            ),
        }
    }

    pub fn position(&self) -> Option<(u32, u32)> {
        match self {
            CellError::UndefinedName { line, column, .. }
            | CellError::Parse { line, column, .. }
            | CellError::NotErasable { line, column, .. }
            | CellError::ReservedName { line, column, .. }
            | CellError::ShadowsHostFunction { line, column, .. } => Some((*line, *column)),
        }
    }
}

/// Erases `source`'s types, names its top-level bindings, and wraps it in the
/// one function the isolate runs.
pub fn compile(source: &str, cell: u64) -> Result<CompiledCell, CellError> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts())
        .with_options(ParseOptions {
            // A cell's top-level `return` ends the task (§1); it is the one
            // way out of a task and it is not an error here.
            allow_return_outside_function: true,
            ..ParseOptions::default()
        })
        .parse();
    if let Some(first) = parsed.diagnostics.iter().next() {
        let offset = first.labels.first().map_or(0, |label| label.offset());
        let (line, column) = line_and_column(source, offset);
        return Err(CellError::Parse {
            message: first.message.to_string(),
            line,
            column,
        });
    }

    let mut eraser = Eraser {
        source,
        blanks: Vec::new(),
        not_erasable: None,
    };
    eraser.visit_program(&parsed.program);
    if let Some((construct, offset)) = eraser.not_erasable {
        let (line, column) = line_and_column(source, offset);
        return Err(CellError::NotErasable {
            construct,
            line,
            column,
        });
    }
    let blanks = eraser.blanks;

    // Every name read but never bound. Collected here because the program is
    // already parsed and walked; judged in the isolate, which is the only
    // place that knows what the global object carries.
    let mut names = FreeNames::default();
    names.visit_program(&parsed.program);
    let free_names = names.into_free();

    let scan = top_level(&parsed.program, source);
    let declared: Vec<String> = scan
        .captures
        .iter()
        .flat_map(|capture| capture.names.iter().cloned())
        .collect();
    // The declaration's own position, so a refusal a model reads on its
    // first turn points at a line that exists. `find_declaration` answers
    // with the statement the name was bound by, which `top_level` has
    // already walked.
    if let Some(name) = declared.iter().find(|n| n.starts_with(RESERVED_PREFIX)) {
        let (line, column) = find_declaration(source, &scan, name);
        return Err(CellError::ReservedName {
            name: name.clone(),
            line,
            column,
        });
    }
    if let Some(name) = declared.iter().find(|n| is_host_function(n)) {
        let (line, column) = find_declaration(source, &scan, name);
        return Err(CellError::ShadowsHostFunction {
            name: name.clone(),
            line,
            column,
        });
    }

    let late: Vec<String> = scan
        .captures
        .iter()
        .filter(|capture| capture.late)
        .flat_map(|capture| capture.names.iter().cloned())
        .collect();
    let body = render(source, &blanks, &scan.captures, &scan.rewrites);
    Ok(CompiledCell {
        javascript: wrap(&body, &late),
        declared,
        script_name: script_name(cell),
        free_names,
    })
}

/// The name V8 gives the cell's script, and the only one whose frames are
/// shown to the model.
pub fn script_name(cell: u64) -> String {
    format!("sterna:cell:{cell}")
}

/// The generated program. Line 1 is the whole prologue, so the model's line
/// *n* is the script's line *n + [`LINE_OFFSET`]* and every column is
/// unchanged.
///
/// `late` is every name the epilogue may re-read for the value it holds when
/// the cell ends. Its calls pass a third argument the declaration lines do
/// not, which is how `bindings::capture` tells a re-read from a binding the
/// program made: a name the model freed and then bound again in this cell
/// must survive, and a name it declared and then freed must not come back. The `finally` runs on all three endings — a fall-off, a
/// `return`, and a throw — which is what makes §5's "the bindings made before
/// the throw persist" carry the *latest* value rather than the first. Each
/// read is guarded twice: by [`MADE`], so a declaration the program never
/// reached is not captured as `var`'s hoisted `undefined`, and by a `catch`,
/// so a name that turns out not to be in scope leaves the model's own error
/// standing rather than replacing it with a `ReferenceError`.
///
/// **The body ends by `return`ing what `e()` answers, and that value is the
/// whole of §1's two endings.** A host-side "it fell off the end" flag was
/// one line of the model's own program away from turning a `return` into a
/// yield (`__sterna_cell.e(); return "x"` yielded); the value `e()` mints
/// carries a private symbol no JavaScript operation can set, so a cell yields
/// exactly when the value its promise fulfils with is the one the host minted
/// for it.
fn wrap(body: &str, late: &[String]) -> String {
    let mut out = format!("(async function({HOST}){{var {MADE}={{__proto__:null}};try{{\n");
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    // A leading `;` so no statement of the model's can continue into the
    // epilogue, and `e()` last so its marker is what the promise fulfils
    // with exactly when the body fell off its end rather than returning.
    out.push_str(&format!(";return {HOST}.e();\n}}finally{{\n"));
    for name in late {
        out.push_str(&format!(
            "try{{if({MADE}.{name}){HOST}.s(\"{name}\",{name},true)}}catch{{}}\n"
        ));
    }
    out.push_str("}})");
    out
}

/// One top-level statement that binds a name, and where its capture call
/// goes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TopLevelCapture {
    names: Vec<String>,
    /// The byte offset the statement that binds these names starts at — the
    /// position a refusal of one of them reports (`CellError::ReservedName`,
    /// `CellError::ShadowsHostFunction`).
    at: u32,
    /// The byte offset the `s("name", name)` calls are inserted at: the end
    /// of the line the statement finishes on, so a program written one
    /// statement per line has every column exactly where the model put it.
    insert_at: u32,
    /// Whether the epilogue can read these names again when the cell ends.
    /// True for everything the rewrite below makes function-scoped; false for
    /// a `class` and for `using`, which keep a block scope the `finally` is
    /// outside of.
    late: bool,
}

/// One keyword replaced by another of the **same character width**, so a
/// rewrite never moves a column. Today that is only `let`/`const` → `var`.
type Rewrite = (u32, u32, &'static str);

/// What one pass over a cell's top level found.
#[derive(Debug, Default)]
struct TopLevel {
    captures: Vec<TopLevelCapture>,
    rewrites: Vec<Rewrite>,
}

/// Every name a cell's top level binds, in source order, with the point each
/// is captured at and the keyword rewrites that make it readable again when
/// the cell ends.
///
/// `const`, `let`, `var`, `function` and `class` are §2's "top-level
/// binding". A bare top-level assignment to an identifier is included too:
/// it also survives into the next cell, because the wrapper's scope chain
/// ends at the global object, and a value that persists and is not in the
/// table is exactly the untracked object this contract exists to not have.
///
/// **An assignment binds only when its target is a plain identifier.**
/// `box.hits = 7`, `arr[0] = 1` and `a.b.c = d` bind nothing: they mutate an
/// object a binding already names. `oxc`'s own
/// `AssignmentTarget::get_identifier_name` answers a member expression with
/// its *property* name, which made `box.hits = 7` insert a capture of a
/// binding named `hits` that no scope has.
fn top_level(program: &Program<'_>, source: &str) -> TopLevel {
    let bounds: Vec<(u32, u32)> = program
        .body
        .iter()
        .map(|statement| {
            let span = statement.span();
            (span.start, span.end)
        })
        .collect();

    let mut found = TopLevel::default();
    let mut seen: Vec<String> = Vec::new();
    for (index, statement) in program.body.iter().enumerate() {
        let mut names: Vec<String> = Vec::new();
        let mut late = true;
        match statement {
            Statement::VariableDeclaration(declaration) => {
                if declaration.declare {
                    // `declare const x: number` states a type and emits no
                    // code; there is nothing to bind and nothing to rewrite.
                    continue;
                }
                match declaration.kind {
                    // `var` is already function-scoped.
                    VariableDeclarationKind::Var => {}
                    VariableDeclarationKind::Let | VariableDeclarationKind::Const => {
                        if let Some(rewrite) =
                            to_var(source, declaration.span.start, declaration.kind)
                        {
                            found.rewrites.push(rewrite);
                        }
                    }
                    // `using x = r` disposes `r` when its block ends;
                    // rewriting it to `var` would drop the disposal, so it
                    // keeps its scope and its declaration-line capture.
                    VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing => {
                        late = false;
                    }
                }
                for declarator in &declaration.declarations {
                    for ident in binding_names(&declarator.id) {
                        names.push(ident.to_string());
                    }
                }
            }
            Statement::FunctionDeclaration(function) => {
                if function.declare {
                    // `declare function f(): void` states a type and emits no
                    // code, so there is no `f` for a capture call to read —
                    // the generated `s("f", f)` threw `ReferenceError` from
                    // Sterna's own line rather than from the model's.
                    continue;
                }
                if let Some(id) = &function.id {
                    names.push(id.name.to_string());
                }
            }
            Statement::ClassDeclaration(class) if class.declare => continue,
            Statement::ClassDeclaration(class) => {
                // A `class` binding is block-scoped to the `try` and no
                // same-width rewrite hoists one, so the epilogue cannot read
                // the binding itself — only the copy the declaration line
                // already put on the persistent scope, which is the same
                // object. Re-reading it would buy nothing and would suggest a
                // later `C = other` had been seen, which it has not.
                late = false;
                if let Some(id) = &class.id {
                    names.push(id.name.to_string());
                }
            }
            Statement::ExpressionStatement(expression) => {
                if let Expression::AssignmentExpression(assignment) = &expression.expression
                    && let AssignmentTarget::AssignmentTargetIdentifier(ident) = &assignment.left
                {
                    names.push(ident.name.to_string());
                }
            }
            _ => continue,
        }
        names.retain(|name| {
            if seen.contains(name) {
                false
            } else {
                seen.push(name.clone());
                true
            }
        });
        if names.is_empty() {
            continue;
        }
        let end = bounds[index].1;
        let next_start = bounds.get(index + 1).map_or(source.len() as u32, |b| b.0);
        found.captures.push(TopLevelCapture {
            names,
            at: bounds[index].0,
            insert_at: insertion_point(source, end, next_start),
            late,
        });
    }
    found
}

/// Where the top-level statement that bound `name` starts, as the model's own
/// line and column. `(1, 0)` when no capture claims the name, which cannot
/// happen for a name `compile` read out of `scan.captures` — it is the answer
/// that reports the start of the program rather than inventing `line 0`, the
/// one position no program has.
fn find_declaration(source: &str, scan: &TopLevel, name: &str) -> (u32, u32) {
    scan.captures
        .iter()
        .find(|capture| capture.names.iter().any(|bound| bound == name))
        .map_or((1, 0), |capture| line_and_column(source, capture.at))
}

/// `let` → `var` and `const` → `var` plus two spaces, so the declarator that
/// follows keeps the column the model wrote it at. `None` when the keyword is
/// not where the span says it is, which leaves the declaration alone rather
/// than corrupting the program.
fn to_var(source: &str, start: u32, kind: VariableDeclarationKind) -> Option<Rewrite> {
    let keyword = kind.as_str();
    let end = start as usize + keyword.len();
    if source.get(start as usize..end)? != keyword {
        return None;
    }
    let padded: &'static str = match kind {
        VariableDeclarationKind::Let => "var",
        VariableDeclarationKind::Const => "var  ",
        _ => return None,
    };
    debug_assert_eq!(padded.len(), keyword.len());
    Some((start, end as u32, padded))
}

/// The end of the line `end` falls on, unless the next top-level statement
/// starts before it — in which case the capture goes immediately after the
/// statement, which shifts that line's later columns and is the price of two
/// statements sharing a line.
fn insertion_point(source: &str, end: u32, next_start: u32) -> u32 {
    let line_end = source[end as usize..]
        .find('\n')
        .map_or(source.len() as u32, |offset| end + offset as u32);
    line_end.min(next_start).max(end)
}

fn binding_names<'a>(pattern: &BindingPattern<'a>) -> Vec<&'a str> {
    pattern
        .get_binding_identifiers()
        .into_iter()
        .map(|ident| ident.name.as_str())
        .collect()
}

/// Rewrites `source` with every span in `blanks` replaced by spaces, one
/// space per character so a column never moves, `\n`/`\r` kept so a line
/// never moves either, each `rewrites` span replaced by its same-width
/// keyword, and each capture's `s(...)` calls spliced in at its own offset.
///
/// All three are applied in one pass, over the *original* offsets: blanking a
/// multi-byte character to one space changes byte positions, so a second
/// pass would splice in the wrong place.
///
/// A capture's splice always **begins** with `;<host>.s(`, so a reader — the
/// module's own tests included — can recover the model's line by cutting
/// there.
fn render(
    source: &str,
    blanks: &[Span],
    captures: &[TopLevelCapture],
    rewrites: &[Rewrite],
) -> String {
    let mut ranges: Vec<(u32, u32)> = blanks
        .iter()
        .filter(|span| span.end > span.start)
        .map(|span| (span.start, span.end))
        .collect();
    ranges.sort_unstable();

    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }

    let mut splices: Vec<(u32, String)> = captures
        .iter()
        .map(|capture| {
            let mut text = String::new();
            for name in &capture.names {
                text.push_str(&format!(";{HOST}.s(\"{name}\",{name})"));
                if capture.late {
                    text.push_str(&format!(";{MADE}.{name}=1"));
                }
            }
            text.push(';');
            (capture.insert_at, text)
        })
        .collect();
    splices.sort_by_key(|(at, _)| *at);

    let mut rewrites: Vec<Rewrite> = rewrites.to_vec();
    rewrites.sort_unstable();

    let mut out = String::with_capacity(source.len());
    let mut next = 0usize;
    let mut spliced = 0usize;
    let mut rewritten = 0usize;
    let mut skip_to = 0u32;
    for (offset, ch) in source.char_indices() {
        let offset = offset as u32;
        while spliced < splices.len() && splices[spliced].0 <= offset {
            out.push_str(&splices[spliced].1);
            spliced += 1;
        }
        while rewritten < rewrites.len() && rewrites[rewritten].1 <= offset {
            rewritten += 1;
        }
        if rewritten < rewrites.len() && rewrites[rewritten].0 == offset {
            out.push_str(rewrites[rewritten].2);
            skip_to = rewrites[rewritten].1;
        }
        if offset < skip_to {
            continue;
        }
        while next < merged.len() && offset >= merged[next].1 {
            next += 1;
        }
        let blanked = next < merged.len() && offset >= merged[next].0 && offset < merged[next].1;
        if blanked && ch != '\n' && ch != '\r' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    while spliced < splices.len() {
        out.push_str(&splices[spliced].1);
        spliced += 1;
    }
    out
}

/// The 1-based line and 0-based column of a byte offset, counting columns in
/// characters so they agree with what V8 reports for the same position.
pub fn line_and_column(source: &str, offset: u32) -> (u32, u32) {
    let offset = offset as usize;
    let mut line = 1u32;
    let mut column = 0u32;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// Collects the spans that are TypeScript and nothing else.
/// Collects what a cell reads and what it binds, so the difference can be
/// checked against the global object before the cell runs.
///
/// The invariant: **`referenced` holds only names used as variables.** oxc
/// spells a member access's property, an object key and a label as their own
/// node types, so `a.b`, `{b: 1}` and `outer:` never reach
/// `visit_identifier_reference` and cannot be mistaken for a free variable.
#[derive(Default)]
struct FreeNames {
    referenced: Vec<(String, u32)>,
    bound: std::collections::BTreeSet<String>,
    /// Where a `typeof` operand sits, by span start.
    ///
    /// **`typeof x` is the one place an undefined name is legal**, and it is
    /// how JavaScript asks "does this name exist" — `return typeof temp` is
    /// exactly what a cell writes to check whether a handle it freed is
    /// gone. Recorded by span rather than by name so that a name used once
    /// under `typeof` and once for real is still caught at the real use.
    typeof_operands: std::collections::BTreeSet<u32>,
}

impl FreeNames {
    /// The referenced names that are bound nowhere in the program, first
    /// mention only and in source order.
    fn into_free(self) -> Vec<(String, u32)> {
        let mut seen = std::collections::BTreeSet::new();
        self.referenced
            .iter()
            .filter(|(_, offset)| !self.typeof_operands.contains(offset))
            .filter(|(name, _)| !self.bound.contains(name))
            // **The wrapper's own bindings are defined when the cell runs.**
            // `__sterna_cell` is the generated epilogue's handle, a local of the
            // function this cell becomes, so it is on no global object and
            // would look free here. Reaching for it is refused -- by
            // `refuse_host_name` and the epilogue guard, at runtime, which is
            // where two security tests check it. Refusing it here instead
            // would report the right outcome for the wrong reason and stop
            // those tests exercising the guard at all.
            .filter(|(name, _)| !name.starts_with(RESERVED_PREFIX))
            .filter(|(name, _)| !IMPLICIT_FUNCTION_BINDINGS.contains(&name.as_str()))
            .filter(|(name, _)| seen.insert(name.clone()))
            .cloned()
            .collect()
    }
}

impl<'a> Visit<'a> for FreeNames {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.referenced.push((it.name.to_string(), it.span.start));
    }

    /// Every binding form funnels through here — `var`/`let`/`const`, a
    /// function or class name, a parameter, a catch parameter, a destructured
    /// element — so one method covers what would otherwise be a dozen.
    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.bound.insert(it.name.to_string());
    }

    fn visit_unary_expression(&mut self, it: &UnaryExpression<'a>) {
        if it.operator.is_typeof()
            && let oxc::ast::ast::Expression::Identifier(name) = &it.argument
        {
            self.typeof_operands.insert(name.span.start);
        }
        walk::walk_unary_expression(self, it);
    }

    // **A type position neither reads nor binds a value-level name**, and
    // the eraser blanks every one of them before V8 sees the program. oxc
    // spells a type's name as an `IdentifierReference` all the same, so
    // `"active" as const` used to record a read of `const`, `x as Foo` a
    // read of `Foo`, and `type T = …` a binding of `T` that hid a real free
    // `T` at value level. Every `TSType` is one node, so the annotation,
    // the `as`/`satisfies`/`<T>` operand, the generic argument and the
    // alias or interface body all stop here; the entry points that hold a
    // `BindingIdentifier` or a type name outside a `TSType` are the
    // remaining overrides.
    fn visit_ts_type(&mut self, _: &TSType<'a>) {}

    fn visit_ts_type_parameter_declaration(&mut self, _: &TSTypeParameterDeclaration<'a>) {}

    fn visit_ts_type_parameter_instantiation(&mut self, _: &TSTypeParameterInstantiation<'a>) {}

    fn visit_ts_type_alias_declaration(&mut self, _: &TSTypeAliasDeclaration<'a>) {}

    fn visit_ts_interface_declaration(&mut self, _: &TSInterfaceDeclaration<'a>) {}

    fn visit_ts_class_implements(&mut self, _: &TSClassImplements<'a>) {}

    /// `declare const x: T` erases to nothing, so it binds nothing: a later
    /// read of `x` is free unless an earlier cell made the handle.
    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'a>) {
        if it.declare {
            return;
        }
        walk::walk_variable_declaration(self, it);
    }

    /// A function without a body — a `declare function`, an overload
    /// signature, an `abstract` method — erases to nothing and binds nothing.
    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        if it.body.is_none() {
            return;
        }
        walk::walk_function(self, it, flags);
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        if it.declare {
            return;
        }
        walk::walk_class(self, it);
    }
}

struct Eraser<'s> {
    source: &'s str,
    blanks: Vec<Span>,
    not_erasable: Option<(&'static str, u32)>,
}

impl<'s> Eraser<'s> {
    fn blank(&mut self, span: Span) {
        self.blanks.push(span);
    }

    /// Blanks the tail of `whole` that follows `inner` — the ` as Foo`, the
    /// `!`, the `<T>` of an instantiation.
    fn blank_tail(&mut self, whole: Span, inner: Span) {
        if whole.end > inner.end {
            self.blank(Span::new(inner.end, whole.end));
        }
    }

    /// Blanks a single-character marker (`?` or `!`) that follows `after`.
    fn blank_marker(&mut self, after: u32, marker: char) {
        let rest = &self.source[after as usize..];
        if let Some(index) = rest.find(marker) {
            let at = after + index as u32;
            // Only when nothing but whitespace separates the two: a `?` any
            // further away belongs to something else.
            if rest[..index].chars().all(char::is_whitespace) {
                self.blank(Span::new(at, at + 1));
            }
        }
    }

    /// Blanks each of `keywords` found in `[from, to)`.
    fn blank_keywords(&mut self, from: u32, to: u32, keywords: &[&str]) {
        if to <= from || to as usize > self.source.len() {
            return;
        }
        let window = &self.source[from as usize..to as usize];
        for keyword in keywords {
            let mut search = 0usize;
            while let Some(index) = window[search..].find(keyword) {
                let at = search + index;
                let before_ok = at == 0
                    || !window[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$');
                let after = at + keyword.len();
                let after_ok = window[after..]
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'));
                if before_ok && after_ok {
                    self.blank(Span::new(from + at as u32, from + after as u32));
                }
                search = after;
            }
        }
    }

    /// Blanks a whole class member and the `;` that terminates it.
    ///
    /// An `abstract` member has to go **entirely**: blanking only its
    /// modifier and its signature leaves the bare key behind, and a bare name
    /// in a class body is a field declaration initialised `undefined` that
    /// shadows the subclass's prototype method. `tsc` emits nothing for one.
    fn blank_member(&mut self, span: Span) {
        self.blank(span);
        let rest = &self.source[span.end as usize..];
        let gap = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if rest[gap..].starts_with(';') {
            let at = span.end + gap as u32;
            self.blank(Span::new(at, at + 1));
        }
    }

    /// Blanks a `declare` keyword standing immediately before `start`, for
    /// the shapes whose own span begins after it. Blanking a span twice is a
    /// no-op — [`render`] merges the ranges — so this is safe to call
    /// wherever the span may or may not already cover the keyword.
    fn blank_declare_before(&mut self, start: u32) {
        let before = self.source[..start as usize].trim_end();
        let Some(at) = before.len().checked_sub("declare".len()) else {
            return;
        };
        if !before[at..].starts_with("declare") {
            return;
        }
        let word = before[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'));
        if word {
            self.blank(Span::new(at as u32, before.len() as u32));
        }
    }

    fn refuse(&mut self, construct: &'static str, span: Span) {
        if self.not_erasable.is_none() {
            self.not_erasable = Some((construct, span.start));
        }
    }
}

/// The modifiers that are TypeScript-only on a class member. `static` and
/// `accessor` are deliberately absent: both are JavaScript.
const CLASS_MEMBER_MODIFIERS: [&str; 6] = [
    "public",
    "private",
    "protected",
    "readonly",
    "abstract",
    "override",
];

impl<'a> Visit<'a> for Eraser<'_> {
    fn visit_ts_type_annotation(&mut self, it: &TSTypeAnnotation<'a>) {
        // The span starts at the `:`, so the whole annotation goes.
        self.blank(it.span);
    }

    fn visit_ts_type_parameter_declaration(&mut self, it: &TSTypeParameterDeclaration<'a>) {
        self.blank(it.span);
    }

    fn visit_ts_type_parameter_instantiation(&mut self, it: &TSTypeParameterInstantiation<'a>) {
        self.blank(it.span);
    }

    fn visit_ts_type_alias_declaration(&mut self, it: &TSTypeAliasDeclaration<'a>) {
        self.blank(it.span);
    }

    fn visit_ts_interface_declaration(&mut self, it: &TSInterfaceDeclaration<'a>) {
        self.blank(it.span);
    }

    fn visit_ts_as_expression(&mut self, it: &TSAsExpression<'a>) {
        self.blank_tail(it.span, it.expression.span());
        self.visit_expression(&it.expression);
    }

    fn visit_ts_satisfies_expression(&mut self, it: &TSSatisfiesExpression<'a>) {
        self.blank_tail(it.span, it.expression.span());
        self.visit_expression(&it.expression);
    }

    fn visit_ts_non_null_expression(&mut self, it: &TSNonNullExpression<'a>) {
        self.blank_tail(it.span, it.expression.span());
        self.visit_expression(&it.expression);
    }

    fn visit_ts_instantiation_expression(&mut self, it: &TSInstantiationExpression<'a>) {
        self.blank_tail(it.span, it.expression.span());
        self.visit_expression(&it.expression);
    }

    fn visit_ts_type_assertion(&mut self, it: &TSTypeAssertion<'a>) {
        // `<T>expr`: the assertion is everything before the expression.
        if it.expression.span().start > it.span.start {
            self.blank(Span::new(it.span.start, it.expression.span().start));
        }
        self.visit_expression(&it.expression);
    }

    /// `declare const x: number` states a type and emits nothing at all —
    /// keeping the keyword left V8 with `declare const …` and a
    /// `SyntaxError` at the model's line for a program that is valid
    /// TypeScript. [`top_level`] already declines to bind its names.
    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'a>) {
        if it.declare {
            self.blank(it.span);
            self.blank_declare_before(it.span.start);
            return;
        }
        walk::walk_variable_declaration(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if it.definite {
            self.blank_marker(it.id.span().end, '!');
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        if it.accessibility.is_some() || it.readonly || it.r#override {
            // `constructor(private x: T)` declares a field as a side effect
            // of the parameter list; erasing the modifier would silently
            // drop the assignment.
            self.refuse("parameter property", it.span);
            return;
        }
        if it.optional {
            self.blank_marker(it.pattern.span().end, '?');
        }
        walk::walk_formal_parameter(self, it);
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        if it.body.is_none() {
            // An overload signature or a `declare function`: declaration
            // only, no code.
            self.blank(it.span);
            if it.declare {
                self.blank_declare_before(it.span.start);
            }
            return;
        }
        walk::walk_function(self, it, flags);
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        if it.declare {
            // `declare class Z { n: number }` declares a type, not a class.
            self.blank(it.span);
            self.blank_declare_before(it.span.start);
            return;
        }
        if it.r#abstract {
            self.blank_keywords(it.span.start, it.body.span.start, &["abstract"]);
        }
        if let (Some(first), Some(last)) = (it.implements.first(), it.implements.last()) {
            let keyword = self.source[..first.span.start as usize].rfind("implements");
            let from = keyword.map_or(first.span.start, |at| at as u32);
            self.blank(Span::new(from, last.span.end));
        }
        walk::walk_class(self, it);
    }

    fn visit_property_definition(&mut self, it: &PropertyDefinition<'a>) {
        if it.declare || it.r#type == PropertyDefinitionType::TSAbstractPropertyDefinition {
            self.blank_member(it.span);
            return;
        }
        self.blank_keywords(it.span.start, it.key.span().start, &CLASS_MEMBER_MODIFIERS);
        if it.optional {
            self.blank_marker(it.key.span().end, '?');
        }
        if it.definite {
            self.blank_marker(it.key.span().end, '!');
        }
        walk::walk_property_definition(self, it);
    }

    fn visit_method_definition(&mut self, it: &MethodDefinition<'a>) {
        if it.r#type == MethodDefinitionType::TSAbstractMethodDefinition {
            self.blank_member(it.span);
            return;
        }
        self.blank_keywords(it.span.start, it.key.span().start, &CLASS_MEMBER_MODIFIERS);
        if it.optional {
            self.blank_marker(it.key.span().end, '?');
        }
        walk::walk_method_definition(self, it);
    }

    fn visit_accessor_property(&mut self, it: &AccessorProperty<'a>) {
        self.blank_keywords(it.span.start, it.key.span().start, &CLASS_MEMBER_MODIFIERS);
        walk::walk_accessor_property(self, it);
    }

    fn visit_ts_enum_declaration(&mut self, it: &TSEnumDeclaration<'a>) {
        self.refuse("enum", it.span);
    }

    fn visit_ts_namespace_declaration(&mut self, it: &TSNamespaceDeclaration<'a>) {
        self.refuse("namespace", it.span);
    }

    fn visit_ts_global_declaration(&mut self, it: &TSGlobalDeclaration<'a>) {
        self.refuse("declare global", it.span);
    }

    fn visit_ts_external_module_declaration(&mut self, it: &TSExternalModuleDeclaration<'a>) {
        self.refuse("declare module", it.span);
    }

    fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'a>) {
        self.refuse("import =", it.span);
    }
}

/// One compile failure as the value a cell throws.
///
/// Pure, and about `CellError` rather than about an isolate, so it lives
/// with the error it renders. Moved out of `isolate.rs` when that file met
/// the Phase 59 ratchet; every caller is unchanged but for the path.
pub fn error_value(error: &CellError) -> crate::runtime::preview::ErrorValue {
    let (line, column) = error
        .position()
        .map_or((None, None), |(l, c)| (Some(l), Some(c)));
    crate::runtime::preview::ErrorValue {
        class: error.class().to_string(),
        message: error.message(),
        line,
        column,
        stack: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model's own lines back out of the generated program: the
    /// prologue dropped, the trailing `e()` dropped, and each line's capture
    /// splice — which is always appended at the end — cut off again.
    fn body_of(javascript: &str) -> String {
        javascript
            .lines()
            .skip(1)
            .take_while(|line| *line != ";return __sterna_cell.e();")
            .map(|line| line.split(";__sterna_cell.s(").next().unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_type_annotation_is_erased_where_it_stood() {
        let source = "const n: number = 1;\nconst s: string = \"x\";\n";
        let compiled = compile(source, 1).unwrap();
        let body = body_of(&compiled.javascript);
        assert_eq!(
            body, "var   n         = 1;\nvar   s         = \"x\";",
            "{body:?}"
        );
        assert_eq!(compiled.declared, vec!["n", "s"]);
    }

    #[test]
    fn every_erasure_keeps_the_line_and_the_column() {
        let source = "const a = 1;\nfunction f(x: number, y?: string): void {\n  return;\n}\nconst b = f as unknown;\n";
        let compiled = compile(source, 3).unwrap();
        for (original, erased) in source.lines().zip(body_of(&compiled.javascript).lines()) {
            assert_eq!(
                original.chars().count(),
                erased.chars().count(),
                "a line changed width: {original:?} -> {erased:?}"
            );
        }
    }

    #[test]
    fn a_destructuring_declaration_names_every_binding_it_makes() {
        let compiled = compile("const { a, b: c } = obj;\nconst [d, ...e] = arr;\n", 1).unwrap();
        assert_eq!(compiled.declared, vec!["a", "c", "d", "e"]);
    }

    #[test]
    fn functions_classes_and_bare_assignments_are_top_level_bindings() {
        let compiled = compile("function f() {}\nclass K {}\nlet v = 1;\nw = 2;\n", 1).unwrap();
        assert_eq!(compiled.declared, vec!["f", "K", "v", "w"]);
    }

    #[test]
    fn an_interface_and_a_type_alias_leave_nothing_behind() {
        let compiled = compile(
            "interface I { a: number }\ntype T = I | null;\nconst x = 1;\n",
            1,
        )
        .unwrap();
        let body = body_of(&compiled.javascript);
        assert!(!body.contains("interface"), "{body:?}");
        assert!(!body.contains("type T"), "{body:?}");
        assert_eq!(compiled.declared, vec!["x"]);
    }

    #[test]
    fn generics_and_assertions_are_erased() {
        let compiled = compile(
            "const m = new Map<string, number>();\nconst y = (m as any)!;\n",
            1,
        )
        .unwrap();
        let body = body_of(&compiled.javascript);
        assert!(body.contains("new Map"), "{body:?}");
        assert!(!body.contains("<string, number>"), "{body:?}");
        assert!(!body.contains(" as any"), "{body:?}");
        assert!(!body.contains("!;"), "{body:?}");
    }

    #[test]
    fn an_enum_is_refused_rather_than_mis_erased() {
        let error = compile("enum E { A, B }\n", 1).unwrap_err();
        assert!(matches!(
            error,
            CellError::NotErasable {
                construct: "enum",
                line: 1,
                ..
            }
        ));
    }

    #[test]
    fn a_namespace_and_a_parameter_property_are_refused() {
        assert!(matches!(
            compile("namespace N { export const x = 1; }\n", 1).unwrap_err(),
            CellError::NotErasable {
                construct: "namespace",
                ..
            }
        ));
        assert!(matches!(
            compile("class K { constructor(private x: number) {} }\n", 1).unwrap_err(),
            CellError::NotErasable {
                construct: "parameter property",
                ..
            }
        ));
    }

    #[test]
    fn a_binding_may_not_take_a_host_functions_name() {
        let error = compile("const read = 1;\n", 1).unwrap_err();
        assert!(
            matches!(
                error,
                CellError::ShadowsHostFunction {
                    ref name,
                    line: 1,
                    column: 0
                } if name == "read"
            ),
            "{error:?}"
        );
        // Every host name is guarded — every registered tool and every
        // non-tool door — and `console` is not.
        for name in registry::names().into_iter().chain(NON_TOOL_HOST_FUNCTIONS) {
            assert!(
                compile(&format!("const {name} = 1;\n"), 1).is_err(),
                "`{name}` may be shadowed"
            );
        }
        assert!(compile("const console = 1;\n", 1).is_ok());
    }

    #[test]
    fn a_runtime_reserved_name_is_refused() {
        let error = compile("const __sterna_cell = 1;\n", 1).unwrap_err();
        assert!(matches!(error, CellError::ReservedName { .. }), "{error:?}");
    }

    #[test]
    fn a_parse_failure_reports_the_models_own_position() {
        let error = compile("const a = 1;\nconst = ;\n", 1).unwrap_err();
        match error {
            CellError::Parse { line, .. } => assert_eq!(line, 2),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn the_prologue_is_exactly_one_line() {
        let compiled = compile("const a = 1;\n", 1).unwrap();
        let first = compiled.javascript.lines().next().unwrap();
        assert_eq!(
            first,
            "(async function(__sterna_cell){var __sterna_made={__proto__:null};try{"
        );
        assert_eq!(LINE_OFFSET, 1);
        assert!(
            compiled
                .javascript
                .lines()
                .nth(1)
                .unwrap()
                .starts_with("var   a = 1;"),
            "{}",
            compiled.javascript
        );
    }

    /// The capture goes at the end of the line the declaration finishes on,
    /// so every column before it is the model's own.
    #[test]
    fn a_capture_is_appended_where_the_declaration_ends() {
        let compiled = compile("const a = 1;\nlet b = 2;\n", 1).unwrap();
        let lines: Vec<&str> = compiled.javascript.lines().collect();
        assert_eq!(
            lines[1],
            "var   a = 1;;__sterna_cell.s(\"a\",a);__sterna_made.a=1;"
        );
        assert_eq!(
            lines[2],
            "var b = 2;;__sterna_cell.s(\"b\",b);__sterna_made.b=1;"
        );
        assert_eq!(compiled.declared, vec!["a", "b"]);
    }

    /// A binding made before a throw has already been captured when the
    /// throw happens — `runtime-contract.md` §5's third item, as a property
    /// of where the call sits rather than of a `finally`.
    #[test]
    fn a_capture_precedes_a_later_statement_that_could_throw() {
        let compiled = compile("const before = 1;\nthrow new Error(\"x\");\n", 1).unwrap();
        let capture = compiled.javascript.find("s(\"before\"").unwrap();
        let throw = compiled.javascript.find("throw new Error").unwrap();
        assert!(capture < throw, "{}", compiled.javascript);
    }

    /// The other half of the same claim: the epilogue reads the name again
    /// when the cell ends, which is the only way a value assigned after the
    /// declaration line reaches the next cell.
    #[test]
    fn the_epilogue_reads_every_hoisted_binding_again() {
        let compiled = compile("let n = 0;\nn = 5;\nclass K {}\n", 1).unwrap();
        let javascript = &compiled.javascript;
        assert!(
            javascript.contains("try{if(__sterna_made.n)__sterna_cell.s(\"n\",n,true)}catch{}"),
            "{javascript}"
        );
        // A `class` keeps its block scope, so the epilogue must not name it.
        assert!(!javascript.contains("__sterna_made.K"), "{javascript}");
        assert_eq!(compiled.declared, vec!["n", "K"]);
    }

    /// `oxc` answers a member expression's `get_identifier_name` with the
    /// *property* name, so `box.hits = 7` used to insert a capture of a
    /// binding named `hits` that no scope has, and the cell threw
    /// `ReferenceError: hits is not defined`.
    #[test]
    fn a_member_assignment_binds_nothing() {
        let compiled = compile(
            "const box = { hits: 0 };\nbox.hits = 7;\nbox[\"k\"] = 1;\nbox.a.b = 2;\n",
            1,
        )
        .unwrap();
        assert_eq!(compiled.declared, vec!["box"]);
        assert!(
            !compiled.javascript.contains("\"hits\""),
            "{}",
            compiled.javascript
        );
    }

    /// An `abstract` member erases to **nothing**. Blanking only its
    /// modifier and its signature left the bare key behind, and a bare name
    /// in a class body is an own field initialised `undefined` that shadows
    /// the subclass's prototype method — so the program ran and meant
    /// something else.
    #[test]
    fn an_abstract_member_erases_to_nothing() {
        let source = "abstract class Shape {\n  abstract area(): number;\n  abstract n: number;\n                        describe(): string { return \"area=\" + this.area(); }\n}\n";
        let compiled = compile(source, 1).unwrap();
        let body = body_of(&compiled.javascript);
        assert!(!body.contains("abstract"), "{body:?}");
        // Nothing but whitespace is left of either member's own line — not
        // the key, not the signature, not the `;`.
        for line in body.lines().skip(1).take(2) {
            assert!(line.trim().is_empty(), "a member survived: {line:?}");
        }
        // And the concrete method is untouched, at its own column.
        for (original, erased) in source.lines().zip(body.lines()) {
            assert_eq!(
                original.chars().count(),
                erased.chars().count(),
                "a line changed width: {original:?} -> {erased:?}"
            );
        }
        assert!(body.contains("this.area()"), "{body:?}");
    }

    /// `declare` states that something exists elsewhere and emits no code, so
    /// it erases to nothing and binds nothing. Keeping the keyword gave V8
    /// `declare const …` and a `SyntaxError` on valid TypeScript; generating a
    /// capture for a `declare function` threw `ReferenceError` from Sterna's own
    /// generated column, past the end of the model's line.
    #[test]
    fn a_declare_erases_to_nothing_and_binds_nothing() {
        for source in [
            "declare const missing: number;\n",
            "declare let missing: number;\n",
            "declare var missing: number;\n",
            "declare class Z { n: number }\n",
            "declare function foo(a: number): void;\n",
        ] {
            let compiled = compile(source, 1).unwrap();
            let body = body_of(&compiled.javascript);
            assert!(
                body.trim().chars().all(|c| c == ';'),
                "{source:?} left {body:?}"
            );
            assert!(
                compiled.declared.is_empty(),
                "{source:?} declared {:?}",
                compiled.declared
            );
            assert!(
                !compiled.javascript.contains("__sterna_cell.s("),
                "{source:?} generated a capture: {}",
                compiled.javascript
            );
            for (original, erased) in source.lines().zip(body.lines()) {
                assert_eq!(
                    original.chars().count(),
                    erased.chars().count(),
                    "a line changed width: {original:?} -> {erased:?}"
                );
            }
        }
    }

    /// The rewrite is the one thing that could move a column, so it is
    /// width-preserving by construction and checked here for both keywords.
    #[test]
    fn the_var_rewrite_keeps_every_column() {
        let source = "const { a, b } = obj;\nlet   c = 1;\nvar d = 2;\nc = 3;\n";
        let compiled = compile(source, 1).unwrap();
        for (original, erased) in source.lines().zip(body_of(&compiled.javascript).lines()) {
            assert_eq!(
                original.chars().count(),
                erased.chars().count(),
                "a line changed width: {original:?} -> {erased:?}"
            );
        }
        let body = body_of(&compiled.javascript);
        assert!(body.starts_with("var   { a, b } = obj;"), "{body:?}");
        assert_eq!(compiled.declared, vec!["a", "b", "c", "d"]);
    }

    /// Every free name of `source`, so a test can say "none" outright.
    fn free(source: &str) -> Vec<String> {
        compile(source, 1)
            .unwrap()
            .free_names
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// `"active" as const` parses as a type reference named `const`, and the
    /// scan used to report `const` as undefined — two Terminal-Bench cells
    /// were refused before execution on exactly that. Every type position is
    /// here: the operand of `as`/`satisfies`/`<T>`, a generic argument, an
    /// annotation, a type parameter, `implements`, a type alias or interface
    /// body, and a `declare` shape.
    #[test]
    fn a_type_position_contributes_no_free_name() {
        let source = "const x = 1;\n\
            const a = { s: \"active\" as const };\n\
            const b = x as Foo;\n\
            const c = x satisfies Bar;\n\
            const d = <Baz>x;\n\
            const m = new Map<Qux, Quux>();\n\
            function g(p: P1, q?: P2): P3 { return p; }\n\
            const id = <T,>(v: T): T => v;\n\
            let y: typeof undeclared;\n\
            class K<U> implements Iface { v!: U; }\n\
            type A<P> = P | Alias;\n\
            interface I extends Base { n: Num }\n\
            declare const dc: Dt;\n\
            declare function df(a: Da): Dr;\n\
            declare class Dc { n: Dn }\n";
        assert_eq!(free(source), vec!["Map"]);
    }

    /// The other direction of the same rule: a type-only declaration binds
    /// nothing at value level, so a value-level read of its name is still
    /// free and is still reported before the cell runs.
    #[test]
    fn a_type_only_name_does_not_hide_a_value_level_free_variable() {
        assert_eq!(free("type T = string;\nreturn T;\n"), vec!["T"]);
        assert_eq!(free("interface I { n: number }\nreturn I;\n"), vec!["I"]);
        assert_eq!(
            free("function f<U>(x: U) { return x; }\nreturn U;\n"),
            vec!["U"]
        );
        assert_eq!(free("declare const dc: number;\nreturn dc;\n"), vec!["dc"]);
    }

    /// The advertised half of the contract, at the compiler: every row of
    /// [`ERASABLE_CONSTRUCTS`] compiles and its erasure moves no column.
    /// `tests/typescript_contract.rs` runs the same rows in an isolate.
    #[test]
    fn every_advertised_construct_compiles_and_keeps_every_column() {
        for (construct, program) in ERASABLE_CONSTRUCTS {
            let compiled =
                compile(program, 1).unwrap_or_else(|error| panic!("{construct}: {error:?}"));
            let body = body_of(&compiled.javascript);
            assert_eq!(program.lines().count(), body.lines().count(), "{construct}");
            for (original, erased) in program.lines().zip(body.lines()) {
                assert_eq!(
                    original.chars().count(),
                    erased.chars().count(),
                    "{construct}: a line changed width: {original:?} -> {erased:?}"
                );
            }
        }
    }

    /// The refused half: every row of [`NOT_ERASABLE_CONSTRUCTS`] is refused
    /// under the construct name the row carries, at the model's own line,
    /// with a message that names the construct and the alternative.
    #[test]
    fn every_refused_construct_is_refused_by_name_with_an_alternative() {
        for (construct, example, alternative) in NOT_ERASABLE_CONSTRUCTS {
            let error = compile(&format!("const ok = 1;\n{example}\n"), 1).expect_err(construct);
            match &error {
                CellError::NotErasable {
                    construct: found,
                    line,
                    ..
                } => {
                    assert_eq!(found, construct);
                    assert_eq!(*line, 2, "{construct}");
                }
                other => panic!("{construct}: {other:?}"),
            }
            let message = error.message();
            assert!(message.contains(construct), "{message}");
            assert!(message.contains(alternative), "{message}");
            assert_eq!(error.class(), "TypeScriptNotErasable");
        }
    }
}
