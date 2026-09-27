//! Deterministic, bounded source context assembled without subprocesses.
use crate::sandbox::profile::{Access, Profile};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{self, File},
    io::Read,
    path::Path,
};

/// The largest source file `context` will pack.
///
/// **One number with `exact_edit`'s `MAX_FILE_BYTES`, deliberately.** Any
/// file `edit` will write, `context` can pack: the two disagreed by sixteen
/// times, and because `edit` requires a delivered `context` first, every
/// file between the two numbers was writable in principle and unreachable in
/// practice. Raising this to meet the writer closes that, rather than
/// lowering the writer and taking away something that worked.
pub const SOURCE_CAP: u64 = 16 * 1024 * 1024;
const SMALL: usize = 16_384;
const DEF_CAP: usize = 24_000;
const RENDER_CAP: usize = 30_000;
const SUPPORT_CAP: usize = 18;
const VISIT_CAP: usize = 2_048;
const OUTLINE_CAP: usize = 40;
const SCAN_CAP: u64 = 131_072;
/// How many unscanned files the reference note names before counting the
/// rest: enough to recognise the shape of what was skipped, bounded so the
/// note cannot itself become the omission.
const UNSCANNED_SHOWN: usize = 5;
const SKIP: &[&str] = &[
    ".git",
    ".sterna",
    ".worktrees",
    "target",
    "node_modules",
    "vendor",
    "build",
    "dist",
    ".venv",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRole {
    CompleteFile,
    TargetDefinition,
    /// The file's declaration lines, given when the requested symbol is not
    /// in it. Never a slice of the file: each line carries its own number.
    Outline,
    Import,
    NearbyDefinition,
    Caller,
    Test,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceExcerpt {
    pub role: ContextRole,
    pub path: String,
    pub range: LineRange,
    pub text: String,
    pub complete: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceContext {
    pub path: String,
    pub sha256: String,
    pub language: String,
    pub symbol: Option<String>,
    pub target: SourceExcerpt,
    pub supporting: Vec<SourceExcerpt>,
    pub complete: bool,
    pub omissions: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextError(pub String);
impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ContextError {}

impl SourceContext {
    pub fn render(&self) -> String {
        let mut o = format!(
            "## Source context\npath: {}\nlanguage: {}\nsymbol: {}\nversion: {}\ncomplete: {}\n",
            self.path,
            self.language,
            self.symbol.as_deref().unwrap_or("(whole file)"),
            &self.sha256[..12],
            self.complete
        );
        render_one(&mut o, &self.target);
        for e in &self.supporting {
            render_one(&mut o, e)
        }
        for x in &self.omissions {
            o.push_str(&format!("omission: {x}\n"))
        }
        o
    }

    /// Sheds lowest-ranked supporting excerpts until `fits` is satisfied,
    /// answering whether it became satisfiable.
    ///
    /// `supporting` is assembled in rank order -- imports, then nearby
    /// definitions, then callers and tests -- so its tail is always the
    /// least valuable thing still present, and a caller needing room takes
    /// it from there without deciding anything itself.
    ///
    /// **One omission carrying a count, never a line per excerpt.** A note
    /// per shed is about eighty characters, so shedding a dozen short
    /// excerpts can add more text than it removes and the loop stops
    /// converging. The single note is rewritten in place as the count grows,
    /// which also keeps it inside every `fits` measurement after the first
    /// shed rather than arriving as a surprise at the end.
    ///
    /// **The target is never shed.** It is what was asked for and what an
    /// `edit` binds to byte-exact, so a context carries it whole or is not
    /// delivered at all; there is no narrowing that leaves half a definition
    /// behind for something to edit against.
    fn shed_until(&mut self, note: fn(usize) -> String, fits: impl Fn(&Self) -> bool) -> bool {
        let mut shed = 0usize;
        let mut at: Option<usize> = None;
        while !fits(self) {
            if self.supporting.pop().is_none() {
                return false;
            }
            shed += 1;
            match at {
                Some(index) => self.omissions[index] = note(shed),
                None => {
                    at = Some(self.omissions.len());
                    self.omissions.push(note(shed));
                }
            }
        }
        true
    }

    pub fn narrow_to(&mut self, budget: usize) -> bool {
        self.shed_until(
            |shed| {
                format!(
                    "{shed} lower-ranked supporting excerpt(s) omitted to fit this turn's feedback budget"
                )
            },
            |context| context.render().chars().count() <= budget,
        )
    }
}

pub fn pack(
    profile: &Profile,
    path: &Path,
    symbol: Option<&str>,
) -> Result<SourceContext, ContextError> {
    let path = profile
        .check("source context", Access::Read, path)
        .map_err(|e| ContextError(format!("refused: {}", e.rule)))?;
    let text = read(&path, SOURCE_CAP)?;
    let lines: Vec<&str> = text.lines().collect();
    let lang = Lang::of(&path);
    let rel = relative(profile, &path);
    let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
    let mut omissions = vec![];
    let inferred = (text.len() > SMALL && symbol.is_none())
        .then(|| infer_incomplete_symbol(&lines, lang))
        .flatten();
    let selected_symbol = symbol.or(inferred.as_deref());
    if symbol.is_none() && inferred.is_some() {
        omissions.push("target symbol inferred from the file's unique incomplete marker".into());
    }
    // A body the miss branch builds itself, because an outline is drawn from
    // the whole file rather than sliced out of one range.
    let mut drawn: Option<String> = None;
    let (role, range, complete) = if text.len() <= SMALL {
        (ContextRole::CompleteFile, (0, lines.len()), true)
    } else if let Some(name) = selected_symbol {
        if let Some(r) = definition(&lines, name, lang) {
            (ContextRole::TargetDefinition, r, true)
        } else if let Some(i) = lines.iter().position(|l| has_ident(l, name)) {
            omissions.push(
                "complete definition boundary unavailable; target is a language-agnostic window"
                    .into(),
            );
            (
                ContextRole::TargetDefinition,
                (i.saturating_sub(20), (i + 21).min(lines.len())),
                false,
            )
        } else {
            // **A name this file does not hold is an answer, not a throw.**
            // The read succeeded; only the guess was wrong, and a caller that
            // asked for several things at once loses the ones that worked when
            // one guess throws. So the miss is reported as what the file does
            // define, which is what the next guess needs.
            let (body, found, dropped) = outline(&lines, lang);
            match found {
                0 => omissions.push(format!(
                    "symbol `{name}` is not in this file, and no declaration was recognised in it;                      target is the file's first lines"
                )),
                found => omissions.push(format!(
                    "symbol `{name}` is not in this file; target is its outline of {found}                      declaration(s), each prefixed by its line number"
                )),
            }
            if dropped > 0 {
                omissions.push(format!(
                    "{dropped} further declaration(s) omitted at the {OUTLINE_CAP}-name cap"
                ));
            }
            if found == 0 {
                let n = bounded_prefix(&lines, 120, DEF_CAP);
                (ContextRole::Outline, (0, n), false)
            } else {
                drawn = Some(body);
                (ContextRole::Outline, (0, lines.len()), false)
            }
        }
    } else {
        let n = bounded_prefix(&lines, 500, DEF_CAP);
        if n < lines.len() {
            omissions.push(format!(
                "target is an incomplete first {n} of {} lines; supply `symbol` for a complete editing target",
                lines.len()
            ))
        }
        (ContextRole::CompleteFile, (0, n), n == lines.len())
    };
    // **A definition larger than the cap is delivered short, never refused.**
    // The file was opened, the boundary was found and the body is in hand;
    // returning `Err` here threw into the program, and inside a `Promise.all`
    // that took every sibling context down with it. Worse, it could not be
    // recovered from: `edit` requires a delivered `context`, so every retry
    // threw identically and a definition past this cap could never be edited
    // at all. What is given back instead is the head of it, `complete: false`
    // so nothing certifies it for an `expected_sha256` edit, and an omission
    // carrying both numbers so the next request can be aimed.
    let sliced = drawn.is_none();
    let mut body = drawn.unwrap_or_else(|| slice(&lines, range));
    let mut range = range;
    let mut complete = complete;
    if body.len() > DEF_CAP {
        let total = body.lines().count();
        let kept = bounded_body_lines(&body, DEF_CAP);
        omissions.push(format!(
            "target is {} bytes and the {DEF_CAP} byte cap holds {kept} of its {total} lines; it is delivered incomplete, so no edit binds to it -- name an inner symbol for a complete target",
            body.len()
        ));
        body = body.lines().take(kept).collect::<Vec<_>>().join("\n");
        if sliced {
            range = (range.0, range.0 + kept);
        }
        complete = false;
    }
    let target = make(role, rel.clone(), range, body, complete);
    let mut supporting = vec![];
    if text.len() > SMALL {
        supporting.extend(imports(&lines, &rel, lang));
        if complete {
            supporting.extend(nearby(&lines, &rel, range, lang))
        }
    }
    if let Some(name) = selected_symbol {
        let (mut refs, notes) = references(profile, &path, name);
        supporting.append(&mut refs);
        omissions.extend(notes)
    }
    if supporting.len() > SUPPORT_CAP {
        omissions.push(format!(
            "{} lower-ranked supporting excerpts omitted at the {SUPPORT_CAP}-excerpt cap",
            supporting.len() - SUPPORT_CAP
        ));
        supporting.truncate(SUPPORT_CAP);
    }
    let mut result = SourceContext {
        path: rel,
        sha256: hash,
        language: lang.name().into(),
        symbol: selected_symbol.map(str::to_owned),
        target,
        supporting,
        complete,
        omissions,
    };
    // Bytes here, characters in `narrow_to`: this cap bounds one rendered
    // context and the turn budget counts what the console keeps. Which
    // excerpt goes, and what that says, is `shed_until`'s either way, so the
    // two measures cannot disagree about the order.
    //
    // **Shedding everything and still not fitting is an omission, not a
    // refusal.** The target alone is bounded by `DEF_CAP` above, so the only
    // way past this line is a header and omissions larger than the slack
    // between the two caps -- and a caller that asked for a context is
    // better served by an oversized one it can read than by a throw that
    // costs it the cell.
    result.shed_until(
        |shed| format!("{shed} lower-ranked supporting excerpt(s) omitted to fit the delivery cap"),
        |context| context.render().len() <= RENDER_CAP,
    );
    let rendered = result.render().len();
    if rendered > RENDER_CAP {
        result.omissions.push(format!(
            "context renders {rendered} bytes against the {RENDER_CAP} byte delivery cap with nothing left to shed; it is delivered over the cap rather than withheld"
        ));
    }
    Ok(result)
}

/// Returns the one definition a broad source read can safely promote to an
/// editing context. Ordinary files and ambiguous TODOs stay ordinary reads.
pub fn infer_incomplete_target(
    profile: &Profile,
    path: &Path,
) -> Result<Option<String>, ContextError> {
    let path = profile
        .check("source context", Access::Read, path)
        .map_err(|error| ContextError(format!("refused: {}", error.rule)))?;
    let text = read(&path, SOURCE_CAP)?;
    if text.len() <= SMALL {
        return Ok(None);
    }
    let lines: Vec<&str> = text.lines().collect();
    Ok(infer_incomplete_symbol(&lines, Lang::of(&path)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Py,
    Rs,
    Js,
    Jsx,
    Ts,
    Tsx,
    Go,
    Java,
    Other,
}
impl Lang {
    fn of(p: &Path) -> Self {
        match p.extension().and_then(|x| x.to_str()) {
            Some("py") => Self::Py,
            Some("rs") => Self::Rs,
            Some("js" | "mjs" | "cjs") => Self::Js,
            Some("jsx") => Self::Jsx,
            Some("ts" | "mts" | "cts") => Self::Ts,
            Some("tsx") => Self::Tsx,
            Some("go") => Self::Go,
            Some("java") => Self::Java,
            _ => Self::Other,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Py => "python",
            Self::Rs => "rust",
            Self::Js | Self::Jsx => "javascript",
            Self::Ts | Self::Tsx => "typescript",
            Self::Go => "go",
            Self::Java => "java",
            Self::Other => "other",
        }
    }
}
fn read(p: &Path, cap: u64) -> Result<String, ContextError> {
    let m = fs::metadata(p)
        .map_err(|e| ContextError(format!("could not inspect {}: {e}", p.display())))?;
    if !m.is_file() {
        return Err(ContextError(format!(
            "{} is not a regular file",
            p.display()
        )));
    }
    if m.len() > cap {
        return Err(ContextError(format!(
            "{} exceeds the {cap} byte source cap",
            p.display()
        )));
    }
    let mut b = vec![];
    File::open(p)
        .and_then(|f| f.take(cap + 1).read_to_end(&mut b))
        .map_err(|e| ContextError(format!("could not read {}: {e}", p.display())))?;
    if b.len() as u64 > cap {
        return Err(ContextError(format!(
            "{} grew beyond the {cap} byte source cap",
            p.display()
        )));
    }
    String::from_utf8(b).map_err(|_| ContextError(format!("{} is not UTF-8 source", p.display())))
}
fn definition(l: &[&str], n: &str, g: Lang) -> Option<(usize, usize)> {
    match g {
        Lang::Py => py_def(l, n),
        Lang::Rs => rs_def(l, n),
        Lang::Js | Lang::Jsx | Lang::Ts | Lang::Tsx => js_def(l, n, g),
        Lang::Go => go_def(l, n),
        Lang::Java => java_def(l, n),
        _ => None,
    }
}

fn js_def(lines: &[&str], name: &str, lang: Lang) -> Option<(usize, usize)> {
    use oxc::{
        allocator::Allocator,
        ast::ast::{
            Class, Function, MethodDefinition, TSEnumDeclaration, TSInterfaceDeclaration,
            TSTypeAliasDeclaration, VariableDeclaration,
        },
        ast_visit::{Visit, walk},
        parser::Parser,
        span::{SourceType, Span},
    };

    struct Definitions<'n> {
        name: &'n str,
        spans: Vec<Span>,
    }
    impl<'a> Visit<'a> for Definitions<'_> {
        fn visit_function(
            &mut self,
            function: &Function<'a>,
            flags: oxc::syntax::scope::ScopeFlags,
        ) {
            if function.body.is_some()
                && function.id.as_ref().is_some_and(|id| id.name == self.name)
            {
                self.spans.push(function.span);
            }
            walk::walk_function(self, function, flags);
        }

        fn visit_class(&mut self, class: &Class<'a>) {
            if class.id.as_ref().is_some_and(|id| id.name == self.name) {
                self.spans.push(class.span);
            }
            walk::walk_class(self, class);
        }

        fn visit_method_definition(&mut self, method: &MethodDefinition<'a>) {
            if method.value.body.is_some() && method.key.is_specific_static_name(self.name) {
                self.spans.push(method.span);
            }
            walk::walk_method_definition(self, method);
        }

        fn visit_variable_declaration(&mut self, declaration: &VariableDeclaration<'a>) {
            if declaration.declarations.iter().any(|item| {
                item.id
                    .get_binding_identifier()
                    .is_some_and(|id| id.name == self.name)
            }) {
                self.spans.push(declaration.span);
            }
            walk::walk_variable_declaration(self, declaration);
        }

        fn visit_ts_enum_declaration(&mut self, declaration: &TSEnumDeclaration<'a>) {
            if declaration.id.name == self.name {
                self.spans.push(declaration.span);
            }
            walk::walk_ts_enum_declaration(self, declaration);
        }

        fn visit_ts_interface_declaration(&mut self, declaration: &TSInterfaceDeclaration<'a>) {
            if declaration.id.name == self.name {
                self.spans.push(declaration.span);
            }
            walk::walk_ts_interface_declaration(self, declaration);
        }

        fn visit_ts_type_alias_declaration(&mut self, declaration: &TSTypeAliasDeclaration<'a>) {
            if declaration.id.name == self.name {
                self.spans.push(declaration.span);
            }
            walk::walk_ts_type_alias_declaration(self, declaration);
        }
    }

    let source = lines.join("\n");
    let source_type = match lang {
        Lang::Js => SourceType::unambiguous(),
        Lang::Jsx => SourceType::jsx(),
        Lang::Ts => SourceType::ts(),
        Lang::Tsx => SourceType::tsx(),
        _ => return None,
    };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let mut definitions = Definitions {
        name,
        spans: vec![],
    };
    definitions.visit_program(&parsed.program);
    definitions.spans.sort_by_key(|span| (span.start, span.end));
    definitions
        .spans
        .into_iter()
        .next()
        .map(|span| span_lines(lines, span.start as usize, span.end as usize))
        .map(|range| (leading_definition_start(lines, range.0, true), range.1))
}

fn span_lines(lines: &[&str], start: usize, end: usize) -> (usize, usize) {
    let mut offset = 0;
    let mut start_line = 0;
    let mut end_line = lines.len();
    for (index, line) in lines.iter().enumerate() {
        let next = offset + line.len() + 1;
        if offset <= start && start < next {
            start_line = index;
        }
        if end <= next {
            end_line = index + 1;
            break;
        }
        offset = next;
    }
    (start_line, end_line)
}

fn leading_definition_start(lines: &[&str], mut start: usize, decorators: bool) -> usize {
    let mut in_block_comment = false;
    while start > 0 {
        let previous = lines[start - 1].trim();
        let belongs = if previous.ends_with("*/") {
            in_block_comment = !previous.starts_with("/*");
            true
        } else if in_block_comment {
            if previous.starts_with("/*") {
                in_block_comment = false;
            }
            true
        } else {
            previous.starts_with("//")
                || previous.starts_with("///")
                || previous.starts_with("//! ")
                || (decorators && previous.starts_with('@'))
        };
        if !belongs {
            break;
        }
        start -= 1;
    }
    start
}

fn go_def(lines: &[&str], name: &str) -> Option<(usize, usize)> {
    let declarations = declaration_lines(lines, BraceLanguage::Go)?;
    declarations.iter().enumerate().find_map(|(line, source)| {
        let column = go_decl(source, name)?;
        if source.trim_start().starts_with("func ")
            && !go_function_preamble_is_unambiguous(&declarations, line, column)
        {
            return None;
        }
        let (_, end) = brace_definition(lines, line, column, BraceLanguage::Go)?;
        Some((leading_definition_start(lines, line, false), end))
    })
}

fn go_decl(line: &str, name: &str) -> Option<usize> {
    let code = line.split("//").next().unwrap_or("");
    let leading = code.len() - code.trim_start().len();
    let trimmed = code.trim_start();
    if let Some(rest) = trimmed.strip_prefix("type ") {
        let offset = rest.len() - rest.trim_start().len();
        let rest = rest.trim_start();
        if rest.strip_prefix(name).is_some_and(|tail| {
            tail.starts_with(char::is_whitespace)
                && matches!(tail.split_whitespace().next(), Some("struct" | "interface"))
        }) {
            return Some(leading + "type ".len() + offset);
        }
    }
    let mut rest = trimmed.strip_prefix("func ")?;
    let mut consumed = leading + "func ".len();
    let whitespace = rest.len() - rest.trim_start().len();
    rest = rest.trim_start();
    consumed += whitespace;
    if rest.starts_with('(') {
        let mut depth = 0usize;
        let mut receiver_end = None;
        for (index, byte) in rest.bytes().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        receiver_end = Some(index + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let receiver_end = receiver_end?;
        consumed += receiver_end;
        rest = &rest[receiver_end..];
        let whitespace = rest.len() - rest.trim_start().len();
        rest = rest.trim_start();
        consumed += whitespace;
    }
    rest.strip_prefix(name)
        .filter(|tail| {
            tail.starts_with('(') || tail.starts_with('[') || tail.starts_with(char::is_whitespace)
        })
        .map(|_| consumed)
}

fn go_function_preamble_is_unambiguous(lines: &[String], start: usize, column: usize) -> bool {
    let mut preamble = String::new();
    let mut parens = 0usize;
    let mut brackets = 0usize;
    for (line, source) in lines.iter().enumerate().skip(start).take(32) {
        let source = if line == start {
            &source[column..]
        } else {
            source
        };
        let open = source.find('{');
        let before_open = open.map_or(source, |index| &source[..index]);
        for byte in before_open.bytes() {
            match byte {
                b'(' => parens += 1,
                b')' => {
                    let Some(next) = parens.checked_sub(1) else {
                        return false;
                    };
                    parens = next;
                }
                b'[' => brackets += 1,
                b']' => {
                    let Some(next) = brackets.checked_sub(1) else {
                        return false;
                    };
                    brackets = next;
                }
                b';' => return false,
                _ => {}
            }
        }
        preamble.push_str(before_open);
        if open.is_some() {
            return parens == 0
                && brackets == 0
                && !["struct", "interface", "func", "type"]
                    .iter()
                    .any(|keyword| {
                        preamble
                            .match_indices(keyword)
                            .any(|(index, _)| boundary(&preamble, index, keyword.len()))
                    });
        }
        preamble.push('\n');
        if parens == 0 && brackets == 0 {
            return false;
        }
    }
    false
}

fn java_def(lines: &[&str], name: &str) -> Option<(usize, usize)> {
    let declarations = declaration_lines(lines, BraceLanguage::Java)?;
    declarations.iter().enumerate().find_map(|(line, source)| {
        let column = java_decl(source, name)?;
        if !java_preamble_is_unambiguous(&declarations, line, column) {
            return None;
        }
        let (_, end) = brace_definition(lines, line, column, BraceLanguage::Java)?;
        Some((leading_definition_start(lines, line, true), end))
    })
}

fn java_preamble_is_unambiguous(lines: &[String], start: usize, column: usize) -> bool {
    let mut parens = 0usize;
    for (line, source) in lines.iter().enumerate().skip(start).take(32) {
        let source = if line == start {
            &source[column..]
        } else {
            source
        };
        let brace = source.find('{').unwrap_or(source.len());
        if source[..brace].contains(';') {
            return false;
        }
        for byte in source[..brace].bytes() {
            match byte {
                b'(' => parens += 1,
                b')' => {
                    let Some(next) = parens.checked_sub(1) else {
                        return false;
                    };
                    parens = next;
                }
                _ => {}
            }
        }
        if brace < source.len() {
            return parens == 0;
        }
        if line > start && java_name(source).is_some() {
            return false;
        }
    }
    false
}

fn java_decl(line: &str, name: &str) -> Option<usize> {
    let code = line.split("//").next().unwrap_or("");
    for keyword in ["class", "interface", "enum", "record"] {
        let declaration = format!("{keyword} {name}");
        if let Some((index, _)) = code
            .match_indices(&declaration)
            .find(|(index, _)| boundary(code, *index, declaration.len()))
        {
            return Some(index);
        }
    }
    code.match_indices(name).find_map(|(index, _)| {
        if !boundary(code, index, name.len())
            || !code[index + name.len()..].trim_start().starts_with('(')
        {
            return None;
        }
        let prefix = code[..index].trim();
        if prefix.is_empty()
            || prefix
                .bytes()
                .any(|byte| matches!(byte, b'=' | b'.' | b';' | b'{' | b'}' | b'(' | b')'))
        {
            return None;
        }
        let last = prefix
            .split_whitespace()
            .next_back()?
            .trim_matches(|character: char| matches!(character, '<' | '>' | '[' | ']'));
        (!matches!(
            last,
            "public"
                | "protected"
                | "private"
                | "static"
                | "final"
                | "abstract"
                | "native"
                | "synchronized"
                | "default"
                | "if"
                | "for"
                | "while"
                | "switch"
                | "return"
                | "throw"
                | "new"
        ))
        .then_some(index)
    })
}

#[derive(Clone, Copy)]
enum BraceLanguage {
    Go,
    Java,
}

#[derive(Default)]
struct BraceLexer {
    block_comment: bool,
    raw: bool,
    text_block: bool,
}

fn declaration_lines(lines: &[&str], language: BraceLanguage) -> Option<Vec<String>> {
    let mut lexer = BraceLexer::default();
    let mut masked = Vec::with_capacity(lines.len());
    for line in lines {
        let bytes = line.as_bytes();
        let mut output = bytes.to_vec();
        let mut quote = None;
        let mut escape = false;
        let mut index = 0;
        while index < bytes.len() {
            if lexer.block_comment {
                output[index] = b' ';
                if bytes.get(index..index + 2) == Some(b"*/") {
                    output[index + 1] = b' ';
                    lexer.block_comment = false;
                    index += 2;
                } else {
                    index += 1;
                }
                continue;
            }
            if lexer.raw {
                output[index] = b' ';
                if bytes[index] == b'`' {
                    lexer.raw = false;
                }
                index += 1;
                continue;
            }
            if lexer.text_block {
                output[index] = b' ';
                if bytes.get(index..index + 3) == Some(b"\"\"\"") && !escaped(bytes, index) {
                    output[index + 1] = b' ';
                    output[index + 2] = b' ';
                    lexer.text_block = false;
                    index += 3;
                } else {
                    index += 1;
                }
                continue;
            }
            if let Some(delimiter) = quote {
                output[index] = b' ';
                if escape {
                    escape = false;
                } else if bytes[index] == b'\\' {
                    escape = true;
                } else if bytes[index] == delimiter {
                    quote = None;
                }
                index += 1;
                continue;
            }
            if bytes.get(index..index + 2) == Some(b"//") {
                output[index..].fill(b' ');
                break;
            }
            if bytes.get(index..index + 2) == Some(b"/*") {
                output[index] = b' ';
                output[index + 1] = b' ';
                lexer.block_comment = true;
                index += 2;
                continue;
            }
            if matches!(language, BraceLanguage::Java)
                && bytes.get(index..index + 3) == Some(b"\"\"\"")
            {
                output[index..index + 3].fill(b' ');
                lexer.text_block = true;
                index += 3;
                continue;
            }
            if matches!(language, BraceLanguage::Go) && bytes[index] == b'`' {
                output[index] = b' ';
                lexer.raw = true;
                index += 1;
                continue;
            }
            if matches!(bytes[index], b'\"' | b'\'') {
                output[index] = b' ';
                quote = Some(bytes[index]);
            }
            index += 1;
        }
        if quote.is_some() {
            return None;
        }
        masked.push(String::from_utf8(output).ok()?);
    }
    lexer.complete().then_some(masked)
}

fn escaped(bytes: &[u8], index: usize) -> bool {
    bytes[..index]
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'\\')
        .count()
        % 2
        == 1
}

impl BraceLexer {
    fn scan(&mut self, line: &str, language: BraceLanguage) -> Option<Vec<bool>> {
        let bytes = line.as_bytes();
        let mut braces = vec![];
        let mut quote = None;
        let mut escape = false;
        let mut index = 0;
        while index < bytes.len() {
            if self.block_comment {
                if bytes.get(index..index + 2) == Some(b"*/") {
                    self.block_comment = false;
                    index += 2;
                } else {
                    index += 1;
                }
                continue;
            }
            if self.raw {
                if bytes[index] == b'`' {
                    self.raw = false;
                }
                index += 1;
                continue;
            }
            if self.text_block {
                if bytes.get(index..index + 3) == Some(b"\"\"\"") && !escaped(bytes, index) {
                    self.text_block = false;
                    index += 3;
                } else {
                    index += 1;
                }
                continue;
            }
            if let Some(delimiter) = quote {
                if escape {
                    escape = false;
                } else if bytes[index] == b'\\' {
                    escape = true;
                } else if bytes[index] == delimiter {
                    quote = None;
                }
                index += 1;
                continue;
            }
            if bytes.get(index..index + 2) == Some(b"//") {
                break;
            }
            if bytes.get(index..index + 2) == Some(b"/*") {
                self.block_comment = true;
                index += 2;
                continue;
            }
            if matches!(language, BraceLanguage::Java)
                && bytes.get(index..index + 3) == Some(b"\"\"\"")
            {
                self.text_block = true;
                index += 3;
                continue;
            }
            match bytes[index] {
                b'`' if matches!(language, BraceLanguage::Go) => self.raw = true,
                b'\"' | b'\'' => quote = Some(bytes[index]),
                b'{' => braces.push(true),
                b'}' => braces.push(false),
                _ => {}
            }
            index += 1;
        }
        quote.is_none().then_some(braces)
    }

    fn complete(&self) -> bool {
        !self.block_comment && !self.raw && !self.text_block
    }
}

fn brace_definition(
    lines: &[&str],
    start: usize,
    column: usize,
    language: BraceLanguage,
) -> Option<(usize, usize)> {
    let mut lexer = BraceLexer::default();
    let mut opened = false;
    let mut depth = 0usize;
    for (line, source) in lines.iter().enumerate().skip(start) {
        let source = if line == start {
            source.get(column..)?
        } else {
            source
        };
        for opening in lexer.scan(source, language)? {
            if opening {
                opened = true;
                depth += 1;
            } else {
                depth = depth.checked_sub(1)?;
                if opened && depth == 0 {
                    return lexer.complete().then_some((start, line + 1));
                }
            }
        }
    }
    None
}
fn infer_incomplete_symbol(lines: &[&str], lang: Lang) -> Option<String> {
    let markers: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            ["NotImplementedError", "TODO", "todo!", "unimplemented!"]
                .iter()
                .any(|marker| line.contains(marker))
        })
        .map(|(index, _)| index)
        .collect();
    if markers.len() != 1 {
        return None;
    }
    let marker = markers[0];
    (0..=marker).rev().find_map(|index| {
        let candidate = name(lines[index], lang)?;
        let range = definition(lines, candidate, lang)?;
        (marker >= range.0 && marker < range.1).then(|| candidate.to_string())
    })
}
fn py_decl(l: &str, n: &str) -> bool {
    let t = l.trim_start();
    [
        format!("def {n}"),
        format!("async def {n}"),
        format!("class {n}"),
    ]
    .iter()
    .any(|p| {
        t.strip_prefix(p)
            .is_some_and(|r| r.starts_with('(') || r.starts_with(':'))
    })
}
fn py_def(l: &[&str], n: &str) -> Option<(usize, usize)> {
    let i = l.iter().position(|x| py_decl(x, n))?;
    let ind = indent(l[i]);
    let mut s = i;
    while s > 0 && l[s - 1].trim_start().starts_with('@') {
        s -= 1
    }
    let e = (i + 1..l.len())
        .find(|&j| {
            let t = l[j].trim();
            !t.is_empty() && indent(l[j]) <= ind
        })
        .unwrap_or(l.len());
    Some((s, e))
}
fn rs_decl(l: &str, n: &str) -> bool {
    let code = l.split("//").next().unwrap_or("");
    ["fn", "struct", "enum", "trait", "type", "union"]
        .iter()
        .any(|k| {
            let q = format!("{k} {n}");
            code.match_indices(&q)
                .any(|(i, _)| boundary(code, i, q.len()))
        })
}
fn rs_def(l: &[&str], n: &str) -> Option<(usize, usize)> {
    let i = l.iter().position(|x| rs_decl(x, n))?;
    let mut s = i;
    while s > 0 {
        let t = l[s - 1].trim_start();
        if t.starts_with("#[") || t.starts_with("///") {
            s -= 1
        } else {
            break;
        }
    }
    let mut lex = Lexer::default();
    let mut d: isize = 0;
    let mut opened = false;
    for (j, x) in l.iter().enumerate().skip(i) {
        let (a, b, semi) = lex.scan(x);
        d += a as isize - b as isize;
        opened |= a > 0;
        if (opened && d == 0) || (!opened && semi) {
            return Some((s, j + 1));
        }
    }
    None
}
#[derive(Default)]
struct Lexer {
    block: usize,
    raw: Option<usize>,
    string: bool,
    character: bool,
    escape: bool,
}
impl Lexer {
    fn scan(&mut self, l: &str) -> (usize, usize, bool) {
        let b = l.as_bytes();
        let (mut i, mut a, mut z, mut semi) = (0, 0, 0, false);
        while i < b.len() {
            if let Some(hashes) = self.raw {
                if b[i] == b'"'
                    && b.get(i + 1..i + 1 + hashes)
                        .is_some_and(|tail| tail.iter().all(|&c| c == b'#'))
                {
                    self.raw = None;
                    i += hashes + 1;
                } else {
                    i += 1;
                }
                continue;
            }
            if self.block > 0 {
                if b.get(i..i + 2) == Some(b"/*") {
                    self.block += 1;
                    i += 2
                } else if b.get(i..i + 2) == Some(b"*/") {
                    self.block -= 1;
                    i += 2
                } else {
                    i += 1
                }
                continue;
            }
            if self.string || self.character {
                if self.escape {
                    self.escape = false;
                    i += 1;
                    continue;
                }
                if b[i] == b'\\' {
                    self.escape = true;
                    i += 1;
                    continue;
                }
                if (self.string && b[i] == b'"') || (self.character && b[i] == b'\'') {
                    self.string = false;
                    self.character = false
                }
                i += 1;
                continue;
            }
            if b.get(i..i + 2) == Some(b"//") {
                break;
            }
            if b.get(i..i + 2) == Some(b"/*") {
                self.block = 1;
                i += 2;
                continue;
            }
            if b[i] == b'r' {
                let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                if b.get(i + hashes + 1) == Some(&b'"') {
                    self.raw = Some(hashes);
                    i += hashes + 2;
                    continue;
                }
            }
            match b[i] {
                b'"' => self.string = true,
                // Lifetimes have no closing quote. Only a short, closed
                // literal may hide braces from the definition scanner.
                b'\''
                    if b[i + 1..]
                        .iter()
                        .position(|&c| c == b'\'')
                        .is_some_and(|distance| distance <= 5) =>
                {
                    self.character = true
                }
                b'{' => a += 1,
                b'}' => z += 1,
                b';' => semi = true,
                _ => {}
            }
            i += 1
        }
        (a, z, semi)
    }
}
fn imports(l: &[&str], p: &str, g: Lang) -> Vec<SourceExcerpt> {
    l.iter()
        .enumerate()
        .filter(|(_, x)| {
            let t = x.trim_start();
            match g {
                Lang::Py => t.starts_with("import ") || t.starts_with("from "),
                Lang::Rs => {
                    t.starts_with("use ") || t.starts_with("pub use ") || t.starts_with("mod ")
                }
                Lang::Js | Lang::Jsx | Lang::Ts | Lang::Tsx => {
                    t.starts_with("import ")
                        || (t.starts_with("export ") && t.contains(" from "))
                        || t.contains("require(")
                }
                Lang::Go => t.starts_with("import ") || t == "import (",
                Lang::Java => t.starts_with("import "),
                _ => false,
            }
        })
        .take(6)
        // This deliberately does not claim a one-line excerpt is a complete
        // multi-line import declaration.
        .map(|(i, x)| {
            make(
                ContextRole::Import,
                p.into(),
                (i, i + 1),
                (*x).into(),
                false,
            )
        })
        .collect()
}
/// The file's declaration lines, for a reader whose symbol was not in it.
///
/// Returns the rendered outline, how many declarations it names, and how
/// many were dropped at [`OUTLINE_CAP`]. Each line is prefixed with its own
/// number, because the outline is not a slice and its lines are not
/// adjacent -- a reader that takes one of these names either calls again
/// with it or reads around the number.
fn outline(lines: &[&str], lang: Lang) -> (String, usize, usize) {
    let mut named: Vec<(usize, &str)> = vec![];
    let mut dropped = 0;
    for (i, line) in lines.iter().enumerate() {
        let Some(found) = name(line, lang) else {
            continue;
        };
        if named.iter().any(|(_, seen)| *seen == found) {
            continue;
        }
        if named.len() == OUTLINE_CAP {
            dropped += 1;
            continue;
        }
        named.push((i, found));
    }
    let body = named
        .iter()
        .map(|(i, _)| format!("{}: {}", i + 1, lines[*i].trim_end()))
        .collect::<Vec<_>>()
        .join("\n");
    (body, named.len(), dropped)
}
fn name(l: &str, g: Lang) -> Option<&str> {
    if g == Lang::Go {
        return go_name(l);
    }
    if g == Lang::Java {
        return java_name(l);
    }
    let t = l.trim_start();
    let ks: &[&str] = match g {
        Lang::Py => &["def ", "async def ", "class "],
        Lang::Rs => &[
            "pub fn ",
            "fn ",
            "pub struct ",
            "struct ",
            "pub enum ",
            "enum ",
            "trait ",
            "type ",
        ],
        Lang::Js | Lang::Jsx | Lang::Ts | Lang::Tsx => &[
            "function ",
            "async function ",
            "export function ",
            "export async function ",
            "class ",
            "export class ",
            "interface ",
            "export interface ",
            "type ",
            "export type ",
            "enum ",
            "export enum ",
            "const ",
            "let ",
            "var ",
            "export const ",
            "export let ",
            "export var ",
        ],
        _ => &[],
    };
    let declaration = ks.iter().find_map(|k| {
        t.strip_prefix(k)
            .and_then(|r| {
                r.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
            })
            .filter(|x| !x.is_empty())
    });
    declaration.or_else(|| {
        matches!(g, Lang::Js | Lang::Jsx | Lang::Ts | Lang::Tsx)
            .then(|| js_method_name(l))
            .flatten()
    })
}
fn js_method_name(line: &str) -> Option<&str> {
    let code = line.split("//").next().unwrap_or("");
    let paren = code.find('(')?;
    let prefix = code[..paren].trim_end();
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, character)| !ident(*character))
        .map_or(0, |(index, character)| index + character.len_utf8());
    let candidate = &prefix[start..];
    (!candidate.is_empty() && js_decl(code, candidate)).then_some(candidate)
}
fn go_name(line: &str) -> Option<&str> {
    let trimmed = line.split("//").next().unwrap_or("").trim_start();
    if let Some(rest) = trimmed.strip_prefix("type ") {
        return rest
            .trim_start()
            .split(|character: char| !ident(character))
            .next()
            .filter(|name| !name.is_empty());
    }
    let mut rest = trimmed.strip_prefix("func ")?.trim_start();
    if rest.starts_with('(') {
        let mut depth = 0usize;
        let mut end = None;
        for (index, byte) in rest.bytes().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        end = Some(index + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = rest.get(end?..)?.trim_start();
    }
    rest.split(|character: char| !ident(character))
        .next()
        .filter(|name| !name.is_empty())
}
fn java_name(line: &str) -> Option<&str> {
    let code = line.split("//").next().unwrap_or("");
    for keyword in ["class ", "interface ", "enum ", "record "] {
        if let Some(start) = code.find(keyword) {
            let rest = &code[start + keyword.len()..];
            if let Some(name) = rest
                .split(|character: char| !ident(character))
                .next()
                .filter(|name| !name.is_empty())
                && java_decl(code, name).is_some()
            {
                return Some(name);
            }
        }
    }
    let paren = code.find('(')?;
    let prefix = code[..paren].trim_end();
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, character)| !ident(*character))
        .map_or(0, |(index, character)| index + character.len_utf8());
    let candidate = &prefix[start..];
    (!candidate.is_empty() && java_decl(code, candidate).is_some()).then_some(candidate)
}
fn nearby(l: &[&str], p: &str, target: (usize, usize), g: Lang) -> Vec<SourceExcerpt> {
    let mut v = vec![];
    for i in target.0.saturating_sub(120)..(target.1 + 120).min(l.len()) {
        if i >= target.0 && i < target.1 {
            continue;
        }
        if let Some(n) = name(l[i], g)
            && let Some(r) = definition(l, n, g)
            && (r.1 <= target.0 || r.0 >= target.1)
        {
            v.push((i.abs_diff(target.0), r))
        }
    }
    v.sort();
    v.dedup_by_key(|x| x.1);
    v.into_iter()
        .take(4)
        .map(|(_, r)| {
            make(
                ContextRole::NearbyDefinition,
                p.into(),
                r,
                slice(l, r),
                false,
            )
        })
        .collect()
}
fn references(profile: &Profile, target: &Path, symbol: &str) -> (Vec<SourceExcerpt>, Vec<String>) {
    let mut stack = vec![(profile.root().to_path_buf(), 0)];
    let (mut files, mut visited, mut refused, mut cutoff) = (vec![], 0, 0, false);
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        let mut es: Vec<_> = rd.flatten().collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            if visited == VISIT_CAP {
                cutoff = true;
                break;
            }
            visited += 1;
            let p = e.path();
            if profile.check("source context", Access::Read, &p).is_err() {
                refused += 1;
                continue;
            }
            let Ok(k) = e.file_type() else { continue };
            let n = e.file_name();
            let n = n.to_string_lossy();
            if k.is_dir() && !k.is_symlink() && depth < 5 && !SKIP.contains(&n.as_ref()) {
                stack.push((p, depth + 1))
            } else if k.is_file() && source(&p) {
                files.push(p)
            }
        }
        if cutoff {
            break;
        }
        stack.sort_by(|a, b| b.0.cmp(&a.0))
    }
    files.sort();
    let mut hits = vec![];
    // **A file the scan could not read is named, never silently skipped.**
    // Its two siblings above -- `refused` and `cutoff` -- both report
    // themselves, and this one did not, so a model read "visited N entries",
    // found no callers and concluded there were none. In a repository of
    // large sources that hid most of them.
    let mut unscanned: Vec<String> = vec![];
    for p in files {
        let Ok(t) = read(&p, SCAN_CAP) else {
            unscanned.push(relative(profile, &p));
            continue;
        };
        let l: Vec<&str> = t.lines().collect();
        let Some(i) = l.iter().position(|x| {
            has_ident(x, symbol) && (p != target || !def_line(x, symbol, Lang::of(&p)))
        }) else {
            continue;
        };
        let rel = relative(profile, &p);
        let test = is_test(&rel);
        let score = usize::from(test) * 4
            + usize::from(l[i].contains(&format!("{symbol}("))) * 2
            + usize::from(p.extension() == target.extension());
        let r = (i.saturating_sub(2), (i + 3).min(l.len()));
        hits.push((
            std::cmp::Reverse(score),
            rel.clone(),
            i,
            make(
                if test {
                    ContextRole::Test
                } else {
                    ContextRole::Caller
                },
                rel,
                r,
                slice(&l, r),
                false,
            ),
        ))
    }
    hits.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
    let mut notes = vec![format!(
        "reference index visited {visited} entries; {refused} were refused"
    )];
    if cutoff {
        notes.push(format!(
            "reference traversal stopped at the {VISIT_CAP}-entry cap"
        ))
    }
    if !unscanned.is_empty() {
        let shown = unscanned.len().min(UNSCANNED_SHOWN);
        let named = unscanned[..shown].join(", ");
        let rest = unscanned.len() - shown;
        notes.push(match rest {
            0 => format!(
                "{} file(s) were not searched for callers, over the {SCAN_CAP} byte scan cap or unreadable: {named}",
                unscanned.len()
            ),
            rest => format!(
                "{} file(s) were not searched for callers, over the {SCAN_CAP} byte scan cap or unreadable: {named} and {rest} more",
                unscanned.len()
            ),
        })
    }
    if hits.len() > 12 {
        notes.push(format!(
            "{} lower-ranked references omitted",
            hits.len() - 12
        ))
    }
    (hits.into_iter().take(12).map(|x| x.3).collect(), notes)
}
fn make(
    role: ContextRole,
    path: String,
    r: (usize, usize),
    text: String,
    complete: bool,
) -> SourceExcerpt {
    SourceExcerpt {
        role,
        path,
        range: LineRange {
            start: r.0 + 1,
            end: r.1,
        },
        text,
        complete,
    }
}
fn slice(l: &[&str], r: (usize, usize)) -> String {
    l[r.0..r.1].join("\n")
}
/// How many of `body`'s lines fit `cap` bytes, counting the newlines between
/// them exactly as [`slice`] joins them, so the kept text is never larger
/// than the number this answered.
fn bounded_body_lines(body: &str, cap: usize) -> usize {
    let mut bytes = 0;
    body.lines()
        .take_while(|line| {
            let extra = line.len() + usize::from(bytes > 0);
            if bytes + extra > cap {
                false
            } else {
                bytes += extra;
                true
            }
        })
        .count()
}
fn bounded_prefix(lines: &[&str], line_cap: usize, byte_cap: usize) -> usize {
    let mut bytes = 0;
    lines
        .iter()
        .take(line_cap)
        .take_while(|line| {
            let extra = line.len() + usize::from(bytes > 0);
            if bytes + extra > byte_cap {
                false
            } else {
                bytes += extra;
                true
            }
        })
        .count()
}
fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}
fn has_ident(l: &str, n: &str) -> bool {
    l.match_indices(n).any(|(i, _)| boundary(l, i, n.len()))
}
fn boundary(l: &str, i: usize, n: usize) -> bool {
    l[..i].chars().next_back().is_none_or(|c| !ident(c))
        && l[i + n..].chars().next().is_none_or(|c| !ident(c))
}
fn ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
fn def_line(l: &str, n: &str, lang: Lang) -> bool {
    match lang {
        Lang::Py => py_decl(l, n),
        Lang::Rs => rs_decl(l, n),
        Lang::Js | Lang::Jsx | Lang::Ts | Lang::Tsx => js_decl(l, n),
        Lang::Go => go_decl(l, n).is_some(),
        Lang::Java => java_decl(l, n).is_some(),
        _ => false,
    }
}
fn js_decl(line: &str, name: &str) -> bool {
    let code = line.split("//").next().unwrap_or("");
    [
        "function",
        "class",
        "interface",
        "type",
        "enum",
        "const",
        "let",
        "var",
    ]
    .iter()
    .any(|keyword| {
        let declaration = format!("{keyword} {name}");
        code.match_indices(&declaration)
            .any(|(index, _)| boundary(code, index, declaration.len()))
    }) || code.match_indices(name).any(|(index, _)| {
        boundary(code, index, name.len())
            && code[index + name.len()..].trim_start().starts_with('(')
    })
}
fn source(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|x| x.to_str()),
        Some(
            "rs" | "py"
                | "ts"
                | "tsx"
                | "mts"
                | "cts"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "go"
                | "java"
                | "c"
                | "h"
                | "cpp"
                | "cc"
        )
    )
}
fn is_test(p: &str) -> bool {
    p.split('/').any(|x| x == "tests" || x == "test") || p.contains("_test.") || p.contains("test_")
}
fn relative(profile: &Profile, p: &Path) -> String {
    p.strip_prefix(profile.root())
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}
fn render_one(o: &mut String, e: &SourceExcerpt) {
    o.push_str(&format!(
        "\n### {:?}: {}:{}-{} [{}]\n",
        e.role,
        e.path,
        e.range.start,
        e.range.end,
        if e.complete { "complete" } else { "excerpt" }
    ));
    for (i, l) in e.text.lines().enumerate() {
        o.push_str(&format!("{:>5} | {l}\n", e.range.start + i))
    }
}
