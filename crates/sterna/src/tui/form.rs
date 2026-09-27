//! A sheet that asks a person for something: labelled fields, one in focus,
//! a check beside what was typed, and the keys that act on it.
//!
//! **One line of text is not a form** (the user, 2026-09-25). A key pasted
//! into a bare title and a row of bullets left a person unsure where the
//! paste went, and an endpoint was three such lines in a row. A form names
//! each field, shows where typing lands, says what it expects before it is
//! wrong and what went wrong after, and keeps every field of one task on one
//! sheet.
//!
//! **A secret never leaves the form but through [`Form::take`].** Its field
//! is drawn as bullets unless the person reveals it, and `Debug` prints the
//! bullets too, so a screen state that is cloned, logged or dumped carries
//! no key.

/// A check against what is typed: `Ok` is praise worth saying, `Err` what
/// is wrong.
pub type Check = fn(&str) -> Result<String, String>;

/// What a field holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Secret,
    /// One of these words; `←→` moves between them.
    Choice(Vec<String>),
}

/// One labelled field.
#[derive(Clone)]
pub struct Field {
    pub label: String,
    /// Said under the field before anything is wrong: what goes here, and
    /// how to paste it.
    pub hint: String,
    pub kind: Kind,
    pub optional: bool,
    value: String,
    /// The chosen word of a [`Kind::Choice`].
    chosen: usize,
    revealed: bool,
    /// A check against what is typed, run as it changes: `Ok` is praise
    /// worth saying ("looks like an OpenRouter key"), `Err` what is wrong.
    check: Option<Check>,
}

impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("label", &self.label)
            .field("value", &self.shown())
            .finish()
    }
}

impl Field {
    #[must_use]
    pub fn new(label: impl Into<String>, kind: Kind, hint: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: hint.into(),
            kind,
            optional: false,
            value: String::new(),
            chosen: 0,
            revealed: false,
            check: None,
        }
    }
    #[must_use]
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
    #[must_use]
    pub fn checked(mut self, check: Check) -> Self {
        self.check = Some(check);
        self
    }
    /// What the screen draws: the text, bullets for an unrevealed secret,
    /// or the chosen word.
    #[must_use]
    pub fn shown(&self) -> String {
        match &self.kind {
            Kind::Secret if !self.revealed => "•".repeat(self.value.chars().count()),
            Kind::Choice(words) => words.get(self.chosen).cloned().unwrap_or_default(),
            _ => self.value.clone(),
        }
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self.kind, Kind::Text | Kind::Secret) && self.value.trim().is_empty()
    }
    #[must_use]
    pub fn chosen(&self) -> usize {
        self.chosen
    }
    /// The check's verdict on what is typed now; nothing while it is empty.
    #[must_use]
    pub fn verdict(&self) -> Option<Result<String, String>> {
        (!self.is_empty())
            .then_some(self.check)
            .flatten()
            .map(|check| check(self.value.trim()))
    }
}

/// A sheet of fields and what submitting it does.
#[derive(Clone, Debug)]
pub struct Form {
    pub title: String,
    /// `Some((2, 3))` for "step 2 of 3" of a flow.
    pub step: Option<(u8, u8)>,
    pub intro: String,
    /// A risk the person should read before submitting, under the intro.
    pub warning: Option<String>,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// What went wrong with the last submit, under the field it is about.
    pub error: Option<(usize, String)>,
    /// A place to get what the form asks for, shown under the fields.
    pub help: Option<String>,
    /// What Enter does, in the footer: "save", "connect".
    pub submit: String,
}

impl Form {
    #[must_use]
    pub fn new(title: impl Into<String>, intro: impl Into<String>, fields: Vec<Field>) -> Self {
        Self {
            title: title.into(),
            step: None,
            intro: intro.into(),
            warning: None,
            fields,
            focus: 0,
            error: None,
            help: None,
            submit: "save".into(),
        }
    }
    #[must_use]
    pub fn step(mut self, at: u8, of: u8) -> Self {
        self.step = Some((at, of));
        self
    }
    #[must_use]
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
    #[must_use]
    pub fn warn(mut self, warning: impl Into<String>) -> Self {
        self.warning = Some(warning.into());
        self
    }
    #[must_use]
    pub fn submit(mut self, what: impl Into<String>) -> Self {
        self.submit = what.into();
        self
    }
    /// The same form reopened with what went wrong under `field`.
    #[must_use]
    pub fn with_error(mut self, field: usize, error: impl Into<String>) -> Self {
        self.focus = field.min(self.fields.len().saturating_sub(1));
        self.error = Some((self.focus, error.into()));
        self
    }
    fn field(&mut self) -> Option<&mut Field> {
        self.fields.get_mut(self.focus)
    }
    /// Typed or pasted text into the field in focus, control characters
    /// dropped: a pasted key carries whatever line ending its source used.
    pub fn push(&mut self, text: &str) {
        self.error = None;
        if let Some(field) = self.field()
            && !matches!(field.kind, Kind::Choice(_))
        {
            field.value.extend(text.chars().filter(|c| !c.is_control()));
        }
    }
    pub fn backspace(&mut self) {
        self.error = None;
        if let Some(field) = self.field() {
            field.value.pop();
        }
    }
    pub fn clear(&mut self) {
        if let Some(field) = self.field() {
            field.value.clear();
        }
    }
    /// Tab and ↓: the next field, wrapping; Shift-Tab and ↑ the one before.
    pub fn move_focus(&mut self, forward: bool) {
        let n = self.fields.len().max(1);
        self.focus = if forward {
            (self.focus + 1) % n
        } else {
            (self.focus + n - 1) % n
        };
    }
    /// ←→ on a choice.
    pub fn choose(&mut self, forward: bool) {
        if let Some(field) = self.field()
            && let Kind::Choice(words) = &field.kind
        {
            let n = words.len().max(1);
            field.chosen = if forward {
                (field.chosen + 1) % n
            } else {
                (field.chosen + n - 1) % n
            };
        }
    }
    /// Ctrl-R: shows or hides the secret in focus.
    pub fn reveal(&mut self) {
        if let Some(field) = self.field()
            && field.kind == Kind::Secret
        {
            field.revealed = !field.revealed;
        }
    }
    /// Enter. `true` when the form is complete and the caller takes it; a
    /// required field still empty takes the focus instead, with a word on
    /// why, and a field whose check fails stops it there.
    pub fn enter(&mut self) -> bool {
        for (index, field) in self.fields.iter().enumerate() {
            if field.is_empty() && !field.optional {
                self.focus = index;
                self.error = Some((index, format!("{} is needed", field.label)));
                return false;
            }
            if let Some(Err(problem)) = field.verdict() {
                self.focus = index;
                self.error = Some((index, problem));
                return false;
            }
        }
        true
    }
    /// Every field's answer, in order: the text, or the chosen word.
    #[must_use]
    pub fn take(self) -> Vec<String> {
        self.fields
            .into_iter()
            .map(|field| match &field.kind {
                Kind::Choice(words) => words.get(field.chosen).cloned().unwrap_or_default(),
                _ => field.value.trim().to_string(),
            })
            .collect()
    }
}

/// What an API key looks like for the providers whose keys say so.
pub fn key_shape(key: &str) -> Result<String, String> {
    if key.chars().any(char::is_whitespace) {
        return Err("a key has no spaces in it; paste it again".into());
    }
    let named = [
        ("sk-ant-", "an Anthropic key"),
        ("sk-or-", "an OpenRouter key"),
        ("sk-proj-", "an OpenAI project key"),
        ("xai-", "an xAI key"),
        ("gsk_", "a Groq key"),
        ("AIza", "a Google AI key"),
        ("sk-", "an OpenAI-style key"),
    ];
    Ok(named
        .iter()
        .find(|(prefix, _)| key.starts_with(prefix))
        .map_or_else(
            || "pasted".to_string(),
            |(_, what)| format!("looks like {what}"),
        ))
}

/// An endpoint's base URL: a scheme and a host.
pub fn base_url(url: &str) -> Result<String, String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| {
            "starts with https:// or http:// — for example https://api.example.com/v1".to_string()
        })?;
    if rest.split(['/', '?']).next().unwrap_or("").is_empty() {
        return Err("needs a host after the scheme".into());
    }
    Ok(
        if url.starts_with("http://") && !rest.starts_with("localhost") && !rest.starts_with("127.")
        {
            "http, not https: the key would travel unencrypted".into()
        } else {
            "a URL".into()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Form {
        Form::new(
            "Your own endpoint",
            "",
            vec![
                Field::new("Base URL", Kind::Text, "").checked(base_url),
                Field::new(
                    "It speaks",
                    Kind::Choice(vec!["openai".into(), "anthropic".into()]),
                    "",
                ),
                Field::new("API key", Kind::Secret, "")
                    .optional()
                    .checked(key_shape),
            ],
        )
    }

    #[test]
    fn a_form_fills_field_by_field_and_is_taken_whole() {
        let mut form = endpoint();
        assert!(!form.enter(), "an empty required field stops it");
        assert_eq!(form.error.as_ref().map(|e| e.0), Some(0));
        form.push("api.example.com/v1");
        assert!(!form.enter(), "a URL without a scheme is refused in place");
        form.clear();
        form.push("https://api.example.com/v1\n");
        form.move_focus(true);
        form.choose(true);
        form.move_focus(true);
        form.push("sk-or-v1-abc");
        assert_eq!(
            form.fields[2].shown(),
            "••••••••••••",
            "a secret is bullets"
        );
        form.reveal();
        assert_eq!(form.fields[2].shown(), "sk-or-v1-abc");
        assert!(form.enter());
        assert_eq!(
            form.take(),
            ["https://api.example.com/v1", "anthropic", "sk-or-v1-abc"]
        );
    }

    #[test]
    fn a_secret_is_bullets_wherever_the_form_is_printed() {
        let mut form = endpoint();
        form.focus = 2;
        form.push("sk-ant-secret");
        assert!(!format!("{form:?}").contains("sk-ant-secret"));
    }

    #[test]
    fn a_key_says_whose_it_looks_like() {
        assert_eq!(
            key_shape("sk-ant-api03-x").unwrap(),
            "looks like an Anthropic key"
        );
        assert!(key_shape("sk-or v1").is_err());
    }
}
