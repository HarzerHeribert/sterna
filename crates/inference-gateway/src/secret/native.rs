//! The operating system's own credential store, and the labelled fallback
//! for when there isn't one (Phase 9E, lines 1 and 2).
//!
//! [`NativeSecretStore`] is the OS store (Keychain, Credential Manager, or a
//! Secret Service keyring, chosen for refusing a locked collection rather
//! than waiting on it); [`super::EnvironmentSecretStore`] is the existing
//! cross-platform source; [`PreferNativeSecretStore`] runs the native store
//! first, the environment second, naming which in [`SecretStore::describe`].
//!
//! `keyring` 3.x silently resolves to a **mock** store, dropping every
//! credential, when no backend feature is enabled; this module names the
//! platform's own builder instead, so a missing feature fails to compile.
//!
//! [`SecretRef`] is a *name*, never a location, which is why preferring the
//! Keychain needed no change to `profile::resolve` or the gateway.
//! No platform error's payload ever reaches a Glasshouse message: `classify`
//! reduces every error to fixed text chosen by variant alone.
//
// History: design-decisions.md, "Trims: migration and native-secret module
// docs", secret/native.rs module doc.

use super::{EnvironmentSecretStore, Secret, SecretRef, SecretStore};

/// The service name every Glasshouse-managed credential is filed under.
///
/// One fixed service, with the credential's own variable name as the
/// account, so the OS store mirrors the environment's namespace exactly:
/// one name, one value, and a user can find the item themselves —
/// `security find-generic-password -s glasshouse -a <VARIABLE>` on macOS,
/// or `cmdkey /list` on Windows, where `keyring` files a generic credential
/// whose target is `<VARIABLE>.glasshouse`.
pub const SERVICE: &str = "glasshouse";

/// The account [`NativeSecretStore::detect`] probes with. Never written, so
/// probing it reads nothing and prompts for nothing: on macOS a missing
/// generic password is answered from the keychain database without
/// consulting an item's access control list, and on Windows `CredReadW`
/// answers `ERROR_NOT_FOUND` from the user's own credential set with no UI
/// at any point.
///
/// Gated to the platforms that have a backend. Only the `keyring` `backend`
/// module reads it, so on every other target it is dead code, and `-D
/// warnings` makes dead code a hard error: this constant took Linux, Windows
/// and lint red while macOS stayed green. **Anything here that only a
/// platform-gated backend uses needs the same gate as that backend** — which
/// is why this gate has to be widened in lockstep with the backend's own,
/// and not one line later.
///
/// The Secret Service arm is the deliberate exception: it probes a
/// *collection* for its `Locked` property rather than an item, because
/// searching for an account nothing ever wrote is exactly the probe that
/// cannot see a locked keyring coming. So this gate stayed at two platforms
/// while the backend went to three.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const PROBE_ACCOUNT: &str = "glasshouse-availability-probe";

/// The reference Glasshouse files the credential for `var` under.
///
/// A *name*, like every [`SecretRef`]: this function reads nothing and
/// resolves nothing. It exists so that configuration, the Settings overlay
/// and this module all derive the same service/account pair from the same
/// place rather than three of them agreeing by accident.
pub fn os_credential_for_variable(var: &str) -> SecretRef {
    SecretRef::OsCredential {
        service: SERVICE.to_owned(),
        account: var.to_owned(),
    }
}

/// Why the native store is not answering.
///
/// Two reasons, deliberately distinguished, because they call for different
/// things from the user: one is "this platform has no store Glasshouse can
/// use", which nothing the user does will change, and the other is "there is
/// one and it would not open", which logging in differently might.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No backend is compiled for this target — see "Which platforms, and
    /// how the third one refuses".
    UnsupportedPlatform,
    /// A backend exists and refused a probe: a locked or missing keychain,
    /// or a session with no access to one — with the store's own account of
    /// which, in [`StoreRefusal`].
    StoreUnreachable(StoreRefusal),
}

/// What the platform's store said when it refused to open.
///
/// Carries more than one fixed sentence because the two failures it can
/// describe call for opposite user responses -- *this session has no
/// credential set* versus *the backend is broken* -- and a bare variant
/// cannot tell them apart. `classification` is `classify`'s fixed text,
/// chosen by the error's *variant alone*; `status` is the platform's own
/// status, taken only from the two `keyring::Error` variants whose payload
/// is a status code rather than something the store returned --
/// `PlatformFailure`/`NoStorageAccess`, never `BadEncoding`, `Ambiguous`,
/// `TooLong` or `Invalid`, which are excluded by the match itself, not by
/// inspecting what they hold.
/// `tests::a_store_error_never_carries_anything_the_store_returned` and
/// `tests::a_store_refusals_status_comes_from_no_variant_that_carries_store_data`
/// are what fail if either drifts.
///
/// History: design-decisions.md, "Trims: migration and native-secret
/// module docs", secret/native.rs `StoreRefusal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRefusal {
    /// Fixed text chosen by the store error's variant alone.
    classification: &'static str,
    /// The platform's own status, for the two variants that carry one.
    /// `None` otherwise — never a placeholder, because "no status" and "a
    /// status that says nothing" are different facts.
    status: Option<String>,
}

impl std::fmt::Display for StoreRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.status {
            Some(status) => write!(f, "{} ({status})", self.classification),
            None => f.write_str(self.classification),
        }
    }
}

impl Unavailable {
    /// A short reason, for a diagnostic line.
    ///
    /// `glasshouse doctor` prints this after *"native secure store:
    /// unavailable"*, and the Settings overlay prints it when a credential
    /// cannot be stored or deleted, so widening it is what puts the
    /// platform's own answer in front of a **user** rather than only in
    /// front of a test.
    pub fn reason(&self) -> String {
        match self {
            Self::UnsupportedPlatform => {
                "this platform has no secure store the gateway can use yet".to_owned()
            }
            Self::StoreUnreachable(refusal) => {
                format!("the native secure store could not be opened: {refusal}")
            }
        }
    }
}

/// What [`NativeSecretStore::delete`] found.
///
/// Deleting a credential that is not there is **not** an error: it is
/// already the desired state, and the caller still deserves to know which
/// of the two happened, so this is reported rather than raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deletion {
    /// There was an item, and it is gone now.
    Removed,
    /// There was nothing to remove.
    AlreadyAbsent,
}

/// What the native store answered when asked whether an item is filed
/// under a name — with **"there is nothing here" and "there is something
/// here and you may not have it" kept apart**.
///
/// [`SecretStore::is_present`] answers one bit, which is the right answer
/// for resolution: a credential the gateway cannot read is a credential
/// Glasshouse cannot use, and every consumer treats both as absent. It is
/// the wrong answer for a *diagnostic*. On macOS an item's access control
/// list names the program that created it, and Glasshouse asks for no
/// authorization dialog by design (`silence_authorization_dialogs`), so an
/// item filed by hand with `security add-generic-password` fails its read
/// and reads as absent — which is how `glasshouse doctor` came to say *not
/// set* about a key sitting in the Keychain, and why a user following that
/// advice stores it again by hand and is told *not set* again.
///
/// The fix that state calls for is a different one — re-store it *through*
/// Glasshouse — so it needs a name of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The store has nothing filed under this name.
    Absent,
    /// The item is there and this process read it.
    Present,
    /// An item answered and the store would not hand it over. Not a
    /// promise about *why*: the store's own reasons are reduced to fixed
    /// text by `classify` long before anything here sees them, and this
    /// variant deliberately carries none of it.
    Refused,
}

/// Why a write to or a deletion from the native store did not happen.
///
/// Every variant carries **names and fixed text only**. The store's own
/// error is reduced to a `&'static str` by `classify` before it gets
/// anywhere near this type, so nothing that came out of the store can be
/// carried by one — see the module documentation.
#[derive(Debug, thiserror::Error)]
pub enum NativeStoreError {
    #[error("no native secure store is available: {}", .0.reason())]
    Unavailable(Unavailable),

    #[error(
        "the native secure store refused the credential filed as `{service}`/`{account}`: {reason}"
    )]
    Refused {
        service: String,
        account: String,
        /// Fixed text chosen by the store error's *variant*. Never the
        /// store's own message, and never any data it returned.
        reason: &'static str,
    },
}

/// The operating system's own credential store.
///
/// Only constructible through [`NativeSecretStore::detect`], and only on a
/// platform whose store this project can prove, so holding one is itself the
/// evidence that a real secure store answered a probe. Never the mock: see
/// the module documentation's "The mock hazard".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSecretStore {
    /// Deliberately unconstructible from outside: see the type's own doc.
    _probed: (),
}

impl NativeSecretStore {
    /// Probe the platform's store, and hand back a handle only if it
    /// answered.
    ///
    /// No credential is read either way, but *what* is asked differs by
    /// platform and has to: macOS and Windows read an account that is never
    /// written, so `keyring::Error::NoEntry` — the expected outcome — counts
    /// as success. The Secret Service arm reads the default collection's
    /// `Locked` property instead, because a locked keyring is the state an
    /// item probe cannot see and the state that would otherwise freeze a
    /// launch.
    ///
    /// That name is deliberately not a link. `keyring` is a macOS-only
    /// dependency (see `crates/glasshouse/Cargo.toml`), so an intra-doc link
    /// to it resolves on macOS and fails the rustdoc gate on Linux and
    /// Windows. **Anything naming a platform-gated dependency in a doc
    /// comment on an ungated item has the same problem** — the sibling rule to
    /// the one `PROBE_ACCOUNT` earned about platform-gated code.
    pub fn detect() -> Result<Self, Unavailable> {
        backend::probe()?;
        Ok(Self { _probed: () })
    }

    /// Put `value` in the native store under `reference`, replacing whatever
    /// was there.
    ///
    /// `value` is a `&str` rather than a [`Secret`] because a [`Secret`] is
    /// what comes *out* of a store; the value going in is text the user just
    /// typed, and it is borrowed for exactly the length of this call.
    pub fn store(&self, reference: &SecretRef, value: &str) -> Result<(), NativeStoreError> {
        let (service, account) = entry_name(reference);
        backend::set(service, account, value).map_err(|reason| NativeStoreError::Refused {
            service: service.to_owned(),
            account: account.to_owned(),
            reason,
        })
    }

    /// Remove the credential `reference` names.
    ///
    /// Absent is [`Deletion::AlreadyAbsent`], not an error — see
    /// [`Deletion`].
    pub fn delete(&self, reference: &SecretRef) -> Result<Deletion, NativeStoreError> {
        let (service, account) = entry_name(reference);
        backend::delete(service, account).map_err(|reason| NativeStoreError::Refused {
            service: service.to_owned(),
            account: account.to_owned(),
            reason,
        })
    }

    /// Whether `reference` is filed here, telling absent from refused —
    /// see [`Presence`] for why those are two answers and not one.
    ///
    /// For a **diagnostic only**. Resolution goes on asking
    /// [`SecretStore::is_present`], which is this reduced to one bit, so no
    /// caller can accidentally treat a refused item as a usable one.
    pub fn presence(&self, reference: &SecretRef) -> Presence {
        let (service, account) = entry_name(reference);
        backend::presence(service, account)
    }
}

impl SecretStore for NativeSecretStore {
    fn resolve(&self, reference: &SecretRef) -> Option<Secret> {
        let (service, account) = entry_name(reference);
        backend::get(service, account).map(Secret)
    }

    /// Answered without the value ever entering this process.
    ///
    /// Unlike [`super::EnvironmentSecretStore::is_present`], which has
    /// `var_os`'s `Option` to read, **neither native store has an
    /// existence-only query**, so both are asked for the item's
    /// *attributes* and the answer is whether that succeeded:
    ///
    /// - macOS: `SecKeychainFindGenericPassword` returns the data.
    ///   `keyring`'s `get_attributes` performs that same lookup for effect
    ///   and returns the item's attributes — an empty map there.
    /// - Windows: `CredReadW` returns the whole credential, blob included.
    ///   `keyring` copies out the comment, target alias and user name, then
    ///   zeroes the blob and frees the credential, so what it hands back is
    ///   three names.
    ///
    /// In both cases the store reads the item and Glasshouse still never
    /// receives its value. That is the closest these platforms allow, and
    /// saying so is better than implying a check that does not exist.
    fn is_present(&self, reference: &SecretRef) -> bool {
        let (service, account) = entry_name(reference);
        backend::exists(service, account)
    }

    fn describe(&self) -> &'static str {
        backend::LABEL
    }
}

/// The store Glasshouse runs with: the native one first, the environment
/// second, and a description that says which of those two arrangements is
/// actually in force.
///
/// The fallback is **labelled, never silent**. [`SecretStore::describe`]
/// answers one of three fixed strings, and `glasshouse doctor` prints it, so
/// "is my key in the Keychain or in my shell profile" is a question a user
/// can read the answer to rather than infer.
#[derive(Debug)]
pub struct PreferNativeSecretStore {
    /// The gateway's own credential file, when a gateway is what runs this
    /// store. Answers before both others: a key stored through
    /// `credentials set` is the one the user chose most deliberately.
    file: Option<super::file::FileSecretStore>,
    native: Result<NativeSecretStore, Unavailable>,
    environment: EnvironmentSecretStore,
}

/// Which of a [`PreferNativeSecretStore`]'s sources answered — the token a
/// program switches on, where [`PreferNativeSecretStore::source_of`] is the
/// label a person reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Native,
    Environment,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Native => "native",
            Self::Environment => "environment",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::File => super::file::LABEL,
            Self::Native => "the native secure store",
            Self::Environment => "the process environment",
        }
    }
}

/// [`SecretStore::describe`] with the gateway's credential file answering
/// first and a native store behind it.
pub const FILE_FIRST_LABEL: &str =
    "the gateway's credential file, then the native secure store, then the process environment";

/// [`SecretStore::describe`] with the gateway's credential file answering
/// first and no native store behind it.
pub const FILE_THEN_ENVIRONMENT_LABEL: &str = "the gateway's credential file, then the process environment (no native secure store could \
     be opened)";

/// [`SecretStore::describe`] with a native store answering first.
///
/// Platform-dependent, because naming the store is the whole point of the
/// label: a Windows user told "the macOS Keychain" would be told a lie in
/// the one place this phase exists to tell the truth. It is defined next to
/// the backend's own [`SecretStore::describe`] label so the two cannot
/// drift — `tests::the_native_first_label_names_this_platforms_store`
/// asserts that it starts with it.
pub const NATIVE_FIRST_LABEL: &str = backend::NATIVE_FIRST_LABEL;

/// [`SecretStore::describe`] on a platform with no store the gateway can use.
pub const UNSUPPORTED_PLATFORM_LABEL: &str =
    "the process environment (this platform has no secure store the gateway can use yet)";

/// [`SecretStore::describe`] when a store exists but would not open.
///
/// Deliberately still **one fixed string**, and not the widened
/// [`StoreRefusal`]. `describe` answers *which arrangement is in force* —
/// one of exactly three, which `integrations::doctor`'s own test asserts by
/// listing them — and that is a different question from *why*. The why is
/// [`Unavailable::reason`], which `doctor` prints on the very next line.
pub const STORE_UNREACHABLE_LABEL: &str =
    "the process environment (the native secure store could not be opened)";

impl Default for PreferNativeSecretStore {
    fn default() -> Self {
        Self::detect()
    }
}

impl PreferNativeSecretStore {
    /// Probe once, here, rather than on every resolution: a store that was
    /// reachable at startup is the fact a launch and a diagnostic both want,
    /// and probing per credential would turn one keychain round trip into
    /// one per variable per frame.
    pub fn detect() -> Self {
        Self {
            file: None,
            native: NativeSecretStore::detect(),
            environment: EnvironmentSecretStore::new(),
        }
    }

    /// [`Self::detect`], with the gateway's own credential file at `path`
    /// answering before either other source — see [`mod@super::file`] for
    /// why a gateway keeps one.
    pub fn detect_with_file(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            file: Some(super::file::FileSecretStore::at(path)),
            ..Self::detect()
        }
    }

    /// The credential file, when this store has one.
    pub fn file(&self) -> Option<&super::file::FileSecretStore> {
        self.file.as_ref()
    }

    /// Which kind of source answers `reference` right now. `None` when none
    /// does. [`Self::source_of`] is the same answer as a label.
    pub fn source_kind(&self, reference: &SecretRef) -> Option<SourceKind> {
        if let Some(file) = &self.file
            && file.is_present(reference)
        {
            return Some(SourceKind::File);
        }
        if let Ok(native) = &self.native
            && native.is_present(reference)
        {
            return Some(SourceKind::Native);
        }
        if self.environment.is_present(reference) {
            return Some(SourceKind::Environment);
        }
        None
    }

    /// The native store, when one answered — the handle
    /// [`NativeSecretStore::store`] and [`NativeSecretStore::delete`] need.
    pub fn native(&self) -> Result<&NativeSecretStore, Unavailable> {
        self.native.as_ref().map_err(Clone::clone)
    }

    /// Which of this store's two sources answers `reference` right now, as
    /// a short label for a diagnostic. `None` when neither does.
    ///
    /// Per credential, unlike [`SecretStore::describe`], which describes the
    /// arrangement. A user with one key in the Keychain and another in a
    /// shell profile is told that, one line each.
    pub fn source_of(&self, reference: &SecretRef) -> Option<&'static str> {
        if let Some(file) = &self.file
            && file.is_present(reference)
        {
            return Some(super::file::LABEL);
        }
        if let Ok(native) = &self.native
            && native.is_present(reference)
        {
            return Some(backend::LABEL);
        }
        if self.environment.is_present(reference) {
            return Some(self.environment.describe());
        }
        None
    }
}

impl SecretStore for PreferNativeSecretStore {
    /// The native store first, the environment second.
    ///
    /// "Prefer" is the whole of line 1: a credential the user stored in the
    /// Keychain wins over one that happens to be exported in the shell that
    /// launched Glasshouse, because the stored one is the one they chose
    /// deliberately.
    fn resolve(&self, reference: &SecretRef) -> Option<Secret> {
        if let Some(file) = &self.file
            && let Some(secret) = file.resolve(reference)
        {
            return Some(secret);
        }
        if let Ok(native) = &self.native
            && let Some(secret) = native.resolve(reference)
        {
            return Some(secret);
        }
        self.environment.resolve(reference)
    }

    fn is_present(&self, reference: &SecretRef) -> bool {
        self.source_kind(reference).is_some()
    }

    fn describe(&self) -> &'static str {
        match (&self.file, &self.native) {
            (Some(_), Ok(_)) => FILE_FIRST_LABEL,
            (Some(_), Err(_)) => FILE_THEN_ENVIRONMENT_LABEL,
            (None, Ok(_)) => NATIVE_FIRST_LABEL,
            (None, Err(Unavailable::UnsupportedPlatform)) => UNSUPPORTED_PLATFORM_LABEL,
            (None, Err(Unavailable::StoreUnreachable(_))) => STORE_UNREACHABLE_LABEL,
        }
    }
}

/// The service and account a reference is filed under.
///
/// An [`SecretRef::Environment`] reference maps to the fixed [`SERVICE`]
/// with the variable's own name as the account — see the module
/// documentation on why a reference names a credential rather than a
/// location.
fn entry_name(reference: &SecretRef) -> (&str, &str) {
    match reference {
        SecretRef::Environment { var } => (SERVICE, var.as_str()),
        SecretRef::OsCredential { service, account } => (service.as_str(), account.as_str()),
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod backend {
    //! The operating system's own credential store, through `keyring`.
    //!
    //! **One module for two platforms, because the plumbing really is the
    //! same.** `keyring` presents the macOS Keychain and Windows Credential
    //! Manager behind one `CredentialApi`, and every difference between them
    //! that Glasshouse cares about is a `const` or a one-function detail —
    //! the label, and whether the platform can raise a dialog. Writing the
    //! two out separately would duplicate `classify`, which is the piece
    //! that must not diverge: it is the only thing standing between a
    //! `keyring::Error` payload and a Glasshouse message.

    use super::{Deletion, PROBE_ACCOUNT, Presence, SERVICE, Unavailable};

    /// [`super::SecretStore::describe`] for the native store alone.
    #[cfg(target_os = "macos")]
    pub const LABEL: &str = "the macOS Keychain";
    /// [`super::PreferNativeSecretStore`]'s whole arrangement, native first.
    #[cfg(target_os = "macos")]
    pub const NATIVE_FIRST_LABEL: &str = "the macOS Keychain, then the process environment";

    /// [`super::SecretStore::describe`] for the native store alone.
    #[cfg(target_os = "windows")]
    pub const LABEL: &str = "Windows Credential Manager";
    /// [`super::PreferNativeSecretStore`]'s whole arrangement, native first.
    #[cfg(target_os = "windows")]
    pub const NATIVE_FIRST_LABEL: &str = "Windows Credential Manager, then the process environment";

    /// The platform's own credential builder, named by the path that only
    /// exists when that platform's backend feature is enabled.
    ///
    /// **This import is the compile-time half of the mock guard.**
    /// `keyring::macos` and `keyring::windows` are declared
    /// `#[cfg(all(target_os = .., feature = ..))]` inside `keyring`, so
    /// declaring the dependency without its backend feature does not
    /// silently fall through to `keyring::mock` here — it fails to resolve
    /// this path and the build stops. Going through `keyring::Entry::new`
    /// instead would read a process-global builder that
    /// `keyring::set_default_credential_builder` can replace at run time,
    /// which is a second way to end up on the mock and one no manifest rule
    /// can catch. See the module documentation.
    #[cfg(target_os = "macos")]
    use keyring::macos::default_credential_builder as platform_credential_builder;
    #[cfg(target_os = "windows")]
    use keyring::windows::default_credential_builder as platform_credential_builder;

    /// Turn off the Keychain's authorization dialogs for this process,
    /// once, before any Keychain call is made.
    ///
    /// Not optional: for an item this binary did not create, decryption
    /// consults an access control list that does not name it, and
    /// `SecKeychainFindGenericPassword` blocks in
    /// `SecurityServer::ClientSession::decrypt` until a user answers a
    /// dialog -- found by `glasshouse doctor` hanging against exactly that
    /// item. `doctor` is piped into files and the same read runs on the
    /// session-start path, so a dialog would freeze the TUI with nothing
    /// visible behind it. With interaction off the call instead fails
    /// cleanly and resolution falls back to the environment: refusing
    /// plainly is the whole of line 2, blocking forever is not a fallback.
    /// The cost: a credential filed by hand with the `security` CLI is not
    /// read; the supported path is to store it *through* Glasshouse.
    ///
    /// History: design-decisions.md, "Trims: migration and native-secret
    /// module docs", secret/native.rs `fn silence_authorization_dialogs` (macos).
    #[cfg(target_os = "macos")]
    fn silence_authorization_dialogs() {
        #[link(name = "Security", kind = "framework")]
        unsafe extern "C" {
            fn SecKeychainSetUserInteractionAllowed(state: u8) -> i32;
        }

        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: a C call taking one scalar and returning one, with no
            // pointers and no memory to own. It is process-global state,
            // which is why it is set exactly once, before the first
            // Keychain call any `NativeSecretStore` can make — nothing else
            // in Glasshouse touches the Keychain.
            unsafe {
                SecKeychainSetUserInteractionAllowed(0);
            }
        });
    }

    /// Nothing to silence: Credential Manager cannot raise a dialog.
    ///
    /// `CredReadW`, `CredWriteW` and `CredDeleteW` are local RPC calls into
    /// LSA against the calling user's own credential set: no UI, no wait,
    /// only an answer or an error code -- so there is no Windows analogue
    /// of the macOS hang. Measured on the Windows ARM64 CI VM on
    /// 2026-09-02: an ssh session (process session 0) got
    /// `ERROR_NO_SUCH_LOGON_SESSION` from every call with no Rust involved,
    /// while a scheduled task under the same user's interactive logon
    /// (process session 1) succeeded -- Credential Manager is per logon
    /// session, not per user. Kept as a same-named no-op rather than
    /// dropped, so `probe` has one shape on every platform and a future
    /// backend that *can* prompt has an obvious place to refuse from.
    ///
    /// History: design-decisions.md, "Trims: migration and native-secret
    /// module docs", secret/native.rs `fn silence_authorization_dialogs` (windows).
    #[cfg(target_os = "windows")]
    fn silence_authorization_dialogs() {}

    /// Every `keyring::Error` reduced to fixed text chosen by **variant
    /// alone**.
    ///
    /// `BadEncoding` carries the raw bytes the store returned and
    /// `Ambiguous` carries credential handles; neither the payloads nor the
    /// crate's own `Display` are used, so nothing that came out of the store
    /// can reach a Glasshouse message. This is shared by both platforms on
    /// purpose: it is the choke point the whole "no secret reaches a
    /// message" property rests on, and two copies of it would be two chances
    /// for one of them to start formatting an error.
    fn classify(err: &keyring::Error) -> &'static str {
        match err {
            keyring::Error::PlatformFailure(_) => "the platform's secure storage failed",
            keyring::Error::NoStorageAccess(_) => {
                "the secure store would not grant access; it may be locked"
            }
            keyring::Error::NoEntry => "there is no such credential",
            keyring::Error::BadEncoding(_) => "the stored credential is not valid UTF-8",
            keyring::Error::TooLong(_, _) => "a name exceeded the store's length limit",
            keyring::Error::Invalid(_, _) => "a name is not valid for this store",
            keyring::Error::Ambiguous(_) => "more than one stored credential matches this name",
            _ => "the secure store reported an error",
        }
    }

    /// One entry, built by the platform's backend and by nothing else.
    ///
    /// See `platform_credential_builder`: this is the only place in
    /// Glasshouse that constructs a `keyring::Entry`, and it does so from a
    /// named platform builder rather than the global default.
    ///
    /// `pub(super)` so that
    /// `tests::the_store_is_built_by_the_platforms_own_backend_and_never_the_mock`
    /// can downcast what it builds, rather than a test-only wrapper here.
    /// A `cfg(test)` item anywhere above the test module would truncate
    /// `production_code`, which splits this file on the first occurrence of
    /// that attribute, and would quietly shrink what the source scans below
    /// cover — including this whole backend.
    ///
    /// Hands back the store's own error rather than a classification of it,
    /// because `probe` needs the platform status inside it and every other
    /// caller here reduces it with `classify` on the next line. The
    /// reduction still happens at exactly one place per path; what changed
    /// is that it no longer happens *before* the one caller that has a use
    /// for what is being thrown away.
    pub(super) fn entry(service: &str, account: &str) -> Result<keyring::Entry, keyring::Error> {
        let credential = platform_credential_builder().build(None, service, account)?;
        Ok(keyring::Entry::new_with_credential(credential))
    }

    /// The platform's own status, and **only** from the two variants whose
    /// payload is a status code rather than something the store returned.
    ///
    /// The exclusion is the match arm, not a judgement about what a payload
    /// happens to contain: `BadEncoding` carries the stored bytes,
    /// `Ambiguous` carries credential handles, and `TooLong`/`Invalid` carry
    /// names, so none of them reaches a `to_string()` here. See
    /// [`super::StoreRefusal`] for what the two remaining payloads are on
    /// each platform and why they cannot carry stored data.
    pub(super) fn platform_status(err: &keyring::Error) -> Option<String> {
        match err {
            keyring::Error::PlatformFailure(status) | keyring::Error::NoStorageAccess(status) => {
                Some(status.to_string())
            }
            _ => None,
        }
    }

    /// The one place a `keyring::Error` becomes an [`Unavailable`].
    pub(super) fn refusal(err: &keyring::Error) -> Unavailable {
        Unavailable::StoreUnreachable(super::StoreRefusal {
            classification: classify(err),
            status: platform_status(err),
        })
    }

    pub fn probe() -> Result<(), Unavailable> {
        // Every path into this module runs through `NativeSecretStore::detect`,
        // which runs through here, so this is the one place that has to
        // remember.
        silence_authorization_dialogs();
        let entry = entry(SERVICE, PROBE_ACCOUNT).map_err(|err| refusal(&err))?;
        match entry.get_attributes() {
            // `NoEntry` is the expected answer and the successful one: the
            // store was reached and had nothing filed under a name nothing
            // ever writes. Anything else means it did not answer, and now
            // says so in the store's own words.
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(refusal(&err)),
        }
    }

    pub fn get(service: &str, account: &str) -> Option<String> {
        entry(service, account).ok()?.get_password().ok()
    }

    /// The item is read for effect and its **attributes** are returned — an
    /// empty map on macOS, three names on Windows — so the value never
    /// enters this process. See `NativeSecretStore::is_present`.
    ///
    /// One bit, derived from [`presence`] rather than computed beside it:
    /// the two answers must not be able to disagree about the same item.
    pub fn exists(service: &str, account: &str) -> bool {
        matches!(presence(service, account), Presence::Present)
    }

    /// Absent, present, or refused — the three-way answer behind
    /// [`super::Presence`].
    ///
    /// `NoEntry` is the store's own *nothing is filed under this name*, and
    /// it is the only answer that means absent. Every other error means the
    /// store answered and would not hand the item over. On macOS that is
    /// overwhelmingly the access-control refusal an item this binary did not
    /// create produces: `keyring`'s attribute read calls `get_secret` for
    /// effect (`keyring-3.6.3/src/credential.rs`), the Security framework
    /// consults an access control list that does not name this program, and
    /// `silence_authorization_dialogs` has already turned the *Allow* dialog
    /// into an error. `keyring` surfaces it as `PlatformFailure`, because
    /// `macos::decode_error` maps only `errSecItemNotFound` to `NoEntry`.
    ///
    /// A name the store will not accept at all — an empty service or account
    /// on macOS, an over-long target on Windows — is `Absent`, and correctly
    /// so: nothing can be filed under a name the store refuses to build.
    pub fn presence(service: &str, account: &str) -> Presence {
        let Ok(entry) = entry(service, account) else {
            return Presence::Absent;
        };
        match entry.get_attributes() {
            Ok(_) => Presence::Present,
            Err(keyring::Error::NoEntry) => Presence::Absent,
            Err(_) => Presence::Refused,
        }
    }

    pub fn set(service: &str, account: &str, value: &str) -> Result<(), &'static str> {
        entry(service, account)
            .map_err(|err| classify(&err))?
            .set_password(value)
            .map_err(|err| classify(&err))
    }

    pub fn delete(service: &str, account: &str) -> Result<Deletion, &'static str> {
        let entry = entry(service, account).map_err(|err| classify(&err))?;
        match entry.delete_credential() {
            Ok(()) => Ok(Deletion::Removed),
            Err(keyring::Error::NoEntry) => Ok(Deletion::AlreadyAbsent),
            Err(err) => Err(classify(&err)),
        }
    }
}

#[cfg(target_os = "linux")]
mod backend {
    //! A Secret Service keyring, through `dbus-secret-service`, and **refusing
    //! rather than waiting**.
    //!
    //! **The invariant: no call this module makes can wait for a user.** The
    //! connection opens with `connect_with_max_prompt_timeout(_, 0)`, which
    //! cancels a prompt instead of raising one, and nothing here names
    //! `unlock`, `ensure_unlocked` or `create_collection` -- a locked
    //! collection is **read** through its `Locked` property and refused,
    //! never opened. This is the Linux equivalent of macOS's
    //! `SecKeychainSetUserInteractionAllowed(0)`, and is why this arm is
    //! written against `dbus-secret-service` directly rather than `keyring`.
    //!
    //! The probe reads a *collection*, not an item: a collection can be
    //! locked and probing an item cannot see it, so searching for a
    //! never-written account would match nothing and report a locked
    //! keyring healthy, exactly the miss this arm exists to close.
    //!
    //! History: design-decisions.md, "Trims: migration and native-secret
    //! module docs", secret/native.rs `mod backend` (linux).

    use std::collections::HashMap;

    use dbus_secret_service::{Collection, EncryptionType, Error, SecretService};

    use super::{Deletion, Presence, Unavailable};

    /// [`super::SecretStore::describe`] for the native store alone.
    pub const LABEL: &str = "a Secret Service keyring";
    /// [`super::PreferNativeSecretStore`]'s whole arrangement, native first.
    pub const NATIVE_FIRST_LABEL: &str = "a Secret Service keyring, then the process environment";

    /// Zero, and it is load-bearing: with a zero timeout `dbus-secret-service`
    /// cancels a prompt instead of raising one, so no call from here can wait
    /// for a user who is not there.
    const PROMPT_TIMEOUT_SECONDS: u64 = 0;

    /// **The compile-time half of the "never in the clear" guard.**
    /// `EncryptionType::Dh` is declared `#[cfg(any(feature = "crypto-rust",
    /// feature = "crypto-openssl"))]` inside `dbus-secret-service`, so naming
    /// it here stops compiling if the manifest ever drops the crypto feature —
    /// rather than silently leaving `EncryptionType::Plain`, which would send
    /// a provider credential over the session bus unencrypted. Same shape as
    /// the macOS arm's `keyring::macos::default_credential_builder` import.
    const ENCRYPTION: EncryptionType = EncryptionType::Dh;

    /// The Secret Service's own attribute names for a service/account pair —
    /// the ones `libsecret`, `secret-tool` and `keyring` all use — so a user
    /// can find a Glasshouse credential with
    /// `secret-tool lookup service glasshouse username <VARIABLE>` exactly as
    /// they can with `security find-generic-password` on macOS.
    const ATTRIBUTE_SERVICE: &str = "service";
    const ATTRIBUTE_ACCOUNT: &str = "username";

    /// What the Secret Service is told a credential's bytes are. `text/plain`
    /// is what every other Secret Service client writes, and what makes a
    /// Glasshouse item readable by one.
    const CONTENT_TYPE: &str = "text/plain";

    /// Every classification this backend can give, and each is
    /// **diagnosis, semicolon, instruction** — the user's decision of
    /// 2026-09-03 asks for actionable installation or configuration
    /// instructions, and this is where the first sentence of those lives.
    /// Fixed text chosen by the error's variant alone, exactly as `classify`
    /// is on the other two platforms.
    const NO_SESSION_BUS: &str = "no D-Bus session bus is reachable, so no keyring can be; \
         start the gateway from a desktop session, or run it under `dbus-run-session`";
    const NO_PROVIDER: &str = "nothing owns `org.freedesktop.secrets` on the session bus; \
         install gnome-keyring, KWallet or KeePassXC and enable its Secret Service integration";
    pub(super) const COLLECTION_LOCKED: &str = "the keyring's default collection is locked, and \
         the gateway will not wait for an unlock prompt; unlock the keyring in your desktop \
         session and start the gateway again";
    const NO_DEFAULT_COLLECTION: &str = "the keyring has no default collection; create one in \
         your keyring application, or run `secret-tool store --label=glasshouse service \
         glasshouse username SOME_VARIABLE` once";
    const PROMPT_REFUSED: &str = "the keyring wanted an unlock prompt, which the gateway \
         refuses on the launch path; unlock the keyring in your desktop session and start \
         the gateway again";
    const BAD_ENCODING: &str = "the stored credential is not text; re-store it through \
         the gateway, or remove the item with `secret-tool clear`";
    const REFUSED: &str = "the Secret Service keyring reported an error; check your keyring \
         application's own log for what it refused";

    /// The one place a connection is opened, and the only constructor this
    /// module names.
    ///
    /// `SecretService::connect` — the constructor that leaves the prompt
    /// timeout unset, and the one `keyring` uses — is deliberately never named
    /// here; `tests::the_secret_service_backend_can_never_wait_for_an_unlock_prompt`
    /// is what fails if it ever is.
    fn open_service() -> Result<SecretService, Error> {
        SecretService::connect_with_max_prompt_timeout(ENCRYPTION, PROMPT_TIMEOUT_SECONDS)
    }

    /// Which of the two "there is nothing to talk to" states this is, read
    /// from the D-Bus error's **name** — a well-known `org.freedesktop.*`
    /// identifier — and never from its message.
    ///
    /// Only a bus that answered can produce `ServiceUnknown`, `NameHasNoOwner`
    /// or a `Spawn` failure: those mean the session bus is there and no
    /// provider is. Anything else at connect time means there is no bus.
    fn classify_dbus(name: Option<&str>) -> &'static str {
        match name {
            Some(
                "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner",
            ) => NO_PROVIDER,
            Some(name) if name.starts_with("org.freedesktop.DBus.Error.Spawn.") => NO_PROVIDER,
            _ => NO_SESSION_BUS,
        }
    }

    /// Every `dbus_secret_service::Error` reduced to fixed text chosen by
    /// **variant alone**, the same choke point `classify` is on the other two
    /// platforms: no payload and no `Display` of the crate's own error reaches
    /// a Glasshouse message.
    fn classify(err: &Error) -> &'static str {
        match err {
            Error::Locked => COLLECTION_LOCKED,
            Error::NoResult => NO_DEFAULT_COLLECTION,
            Error::Prompt => PROMPT_REFUSED,
            Error::Unavailable => NO_PROVIDER,
            Error::UnsupportedSecretFormat => BAD_ENCODING,
            Error::Dbus(err) => classify_dbus(err.name()),
            _ => REFUSED,
        }
    }

    /// The provider's own status, and **only** the D-Bus error *name*.
    ///
    /// A name is an interface identifier from the `org.freedesktop.*`
    /// namespace — `ServiceUnknown`, `AccessDenied` — fixed by the protocol
    /// and composed by nobody. The *message* beside it is free text a provider
    /// writes, which is the one field on this error that could echo something
    /// it read, so `message()` is never called here. `Crypto`, `Path` and
    /// `Parse` carry payloads that are not statuses at all and are excluded by
    /// the match rather than by inspecting them. See [`super::StoreRefusal`].
    fn platform_status(err: &Error) -> Option<String> {
        match err {
            Error::Dbus(err) => err.name().map(str::to_owned),
            _ => None,
        }
    }

    /// The one place a `dbus_secret_service::Error` becomes an [`Unavailable`].
    fn refusal(err: &Error) -> Unavailable {
        Unavailable::StoreUnreachable(super::StoreRefusal {
            classification: classify(err),
            status: platform_status(err),
        })
    }

    /// **The locked-collection guard, and the reason this backend exists.**
    /// A locked collection is refused; it is never unlocked.
    ///
    /// `proceed` is what the caller would do next with a collection it
    /// believes is open — and every one of those operations, on a locked
    /// collection, is answered by the Secret Service with an unlock prompt.
    /// Taking it as a parameter is what lets
    /// `tests::a_locked_collection_is_refused_before_anything_can_prompt`
    /// supply a `proceed` standing in for a prompt nobody answers and prove
    /// this function never reaches it. The test cannot *actually* block: a
    /// test that reproduced the hang would be the defect this packet exists
    /// to prevent, so it panics instead and fails at once.
    pub(super) fn refuse_if_locked<T>(
        locked: Result<bool, Error>,
        proceed: impl FnOnce() -> Result<T, &'static str>,
    ) -> Result<T, &'static str> {
        match locked {
            Ok(false) => proceed(),
            Ok(true) => Err(COLLECTION_LOCKED),
            Err(err) => Err(classify(&err)),
        }
    }

    /// The default collection, or the fixed text saying why not.
    ///
    /// The **only** producer of a [`Collection`] in this module, so every
    /// search, read, write and delete below is downstream of the locked check
    /// by construction rather than by each of them remembering.
    fn unlocked_default_collection(
        connection: &SecretService,
    ) -> Result<Collection<'_>, &'static str> {
        let collection = connection
            .get_default_collection()
            .map_err(|err| classify(&err))?;
        refuse_if_locked(collection.is_locked(), || Ok(collection))
    }

    /// The attributes a credential is filed under, and the only two.
    fn attributes<'a>(service: &'a str, account: &'a str) -> HashMap<&'a str, &'a str> {
        HashMap::from([(ATTRIBUTE_SERVICE, service), (ATTRIBUTE_ACCOUNT, account)])
    }

    /// What a user sees in Seahorse or KWalletManager. Two names, never a
    /// value — the same thing [`super::SecretRef`] is safe to print for.
    fn label(service: &str, account: &str) -> String {
        format!("{service}: {account}")
    }

    pub fn probe() -> Result<(), Unavailable> {
        let connection = open_service().map_err(|err| refusal(&err))?;
        unlocked_default_collection(&connection)
            .map(|_| ())
            .map_err(|classification| {
                Unavailable::StoreUnreachable(super::StoreRefusal {
                    classification,
                    status: None,
                })
            })
    }

    pub fn get(service: &str, account: &str) -> Option<String> {
        let connection = open_service().ok()?;
        let collection = unlocked_default_collection(&connection).ok()?;
        let items = collection.search_items(attributes(service, account)).ok()?;
        let secret = items.first()?.get_secret().ok()?;
        String::from_utf8(secret).ok()
    }

    /// Answered without the value ever entering this process, and here that is
    /// literal rather than a near miss: `SearchItems` returns object paths and
    /// no secret at all, so unlike the macOS and Windows arms — which have to
    /// read the item for its attributes — nothing is decrypted to answer this.
    pub fn exists(service: &str, account: &str) -> bool {
        let Ok(connection) = open_service() else {
            return false;
        };
        let Ok(collection) = unlocked_default_collection(&connection) else {
            return false;
        };
        collection
            .search_items(attributes(service, account))
            .is_ok_and(|items| !items.is_empty())
    }

    /// **This arm has no `Refused` state, and that is a fact about the
    /// Secret Service rather than a gap here.** `SearchItems` answers with
    /// object paths and decrypts nothing, so an item is either listed or it
    /// is not; there is no per-item access control list to be refused by,
    /// which is what produces the macOS state [`super::Presence::Refused`]
    /// exists for. The one way this backend cannot answer — a locked
    /// collection — is a property of the whole store, refused by
    /// `unlocked_default_collection` before any item is named, and reported
    /// by `probe` as [`Unavailable::StoreUnreachable`] on its own line.
    pub fn presence(service: &str, account: &str) -> Presence {
        if exists(service, account) {
            Presence::Present
        } else {
            Presence::Absent
        }
    }

    pub fn set(service: &str, account: &str, value: &str) -> Result<(), &'static str> {
        let connection = open_service().map_err(|err| classify(&err))?;
        let collection = unlocked_default_collection(&connection)?;
        collection
            .create_item(
                &label(service, account),
                attributes(service, account),
                value.as_bytes(),
                true,
                CONTENT_TYPE,
            )
            .map(|_| ())
            .map_err(|err| classify(&err))
    }

    /// The one operation here the Secret Service can answer with a prompt,
    /// and the reason the prompt timeout is not merely belt-and-braces.
    ///
    /// An item in an unlocked collection can still be individually locked on
    /// some providers, and `Item::Delete` then returns a prompt path.
    /// `dbus-secret-service` refuses it at once rather than raising it,
    /// because the timeout is zero, and this reports `PROMPT_REFUSED` — which
    /// tells the user to unlock the keyring. Reading is not exposed to this:
    /// `get_secret` is a plain method call that fails on a locked item rather
    /// than unlocking it.
    pub fn delete(service: &str, account: &str) -> Result<Deletion, &'static str> {
        let connection = open_service().map_err(|err| classify(&err))?;
        let collection = unlocked_default_collection(&connection)?;
        let items = collection
            .search_items(attributes(service, account))
            .map_err(|err| classify(&err))?;
        let Some(item) = items.first() else {
            return Ok(Deletion::AlreadyAbsent);
        };
        item.delete().map_err(|err| classify(&err))?;
        Ok(Deletion::Removed)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod backend {
    //! No store this project can prove. Nothing here pretends otherwise, and
    //! neither `keyring` nor `dbus-secret-service` is a dependency on these
    //! targets — see the module documentation's "Which platforms, and how the
    //! third one refuses".

    use super::{Deletion, Presence, Unavailable};

    pub const LABEL: &str = "no native secure store";

    /// Unreachable through [`super::PreferNativeSecretStore::describe`],
    /// whose native-first arm needs a `Result::Ok` that `probe` never
    /// produces on this target — but the arm still has to compile, so the
    /// constant still has to exist. Worded so that if it ever *did* appear
    /// it would read as the defect it would be.
    pub const NATIVE_FIRST_LABEL: &str = "no native secure store, then the process environment";

    pub fn probe() -> Result<(), Unavailable> {
        Err(Unavailable::UnsupportedPlatform)
    }

    /// Unreachable in practice: `probe` refuses, so no
    /// [`super::NativeSecretStore`] exists on this target to call it. Kept
    /// total rather than `unreachable!()` so that a future backend is added
    /// by writing one, not by removing a panic.
    pub fn get(_service: &str, _account: &str) -> Option<String> {
        None
    }

    pub fn exists(_service: &str, _account: &str) -> bool {
        false
    }

    /// Nothing is filed anywhere, so nothing can be refused either.
    pub fn presence(_service: &str, _account: &str) -> Presence {
        Presence::Absent
    }

    pub fn set(_service: &str, _account: &str, _value: &str) -> Result<(), &'static str> {
        Err("this platform has no secure store the gateway can use yet")
    }

    pub fn delete(_service: &str, _account: &str) -> Result<Deletion, &'static str> {
        Err("this platform has no secure store the gateway can use yet")
    }
}

/// A store with no native half, for exercising the labelled-fallback path on
/// a machine that *does* have a Keychain.
///
/// `#[cfg(test)]` and `pub(crate)`: it does not exist in a release build and
/// it is not API, so the production boundary is exactly as narrow as it was.
/// Placed immediately above the test module, so the source scans below —
/// which read everything before the first `#[cfg(test)]` as production code
/// — still cover the whole of it.
#[cfg(test)]
impl PreferNativeSecretStore {
    pub(crate) fn without_native(reason: Unavailable) -> Self {
        Self {
            file: None,
            native: Err(reason),
            environment: EnvironmentSecretStore::new(),
        }
    }
}

/// Test aids for the platform store, reachable from another crate through the
/// `fixtures` feature: Glasshouse's acceptance test that a stored credential
/// reaches a launch overlay lives on the host side of the extraction and needs
/// exactly these three.
#[cfg(all(
    any(test, feature = "fixtures"),
    any(target_os = "macos", target_os = "windows")
))]
pub mod fixtures {
    use super::{NativeSecretStore, SERVICE, SecretRef};

    /// A unique account per process, so a leftover item from an earlier run
    /// can never be read by a later one — which on macOS would mean a
    /// different binary reading an item whose access control list does not
    /// name it, and therefore an interactive prompt in the middle of a test
    /// run.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn test_account(suffix: &str) -> String {
        format!(
            "GLASSHOUSE_KEYCHAIN_TEST_ONLY_{suffix}_{}",
            std::process::id()
        )
    }

    /// Removes its Keychain item when it goes out of scope, however the
    /// test leaves — a passing assertion, a failing one, or a panic.
    ///
    /// Written after a mutation run left `glasshouse-availability-probe` in
    /// the developer's own login keychain: a test that deletes on its last
    /// line deletes nothing when an assertion above that line fires, and
    /// what it leaves behind is in a real user's real keychain.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub struct KeychainItem {
        reference: SecretRef,
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    impl KeychainItem {
        /// Store `value` under `reference` and take responsibility for
        /// removing it again.
        pub fn stored(store: &NativeSecretStore, reference: SecretRef, value: &str) -> Self {
            store.store(&reference, value).expect("store");
            Self { reference }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    impl Drop for KeychainItem {
        fn drop(&mut self) {
            // Best effort by construction: this runs while a panic may be
            // unwinding, and a second panic from here would replace the
            // assertion message that explains the failure with an abort.
            if let Ok(store) = NativeSecretStore::detect() {
                let _ = store.delete(&self.reference);
            }
        }
    }

    /// Whether the platform's own CLI can see the item — an independent
    /// witness that the credential really is in the OS store, rather than
    /// `keyring` agreeing with itself.
    ///
    /// **Neither form asks for the value.** `security`'s `-w` prints the
    /// password and is deliberately not passed; `cmdkey` has no flag that
    /// prints one at all. Both are asked only whether the item exists, and
    /// neither one's output is ever put into an assertion message.
    ///
    /// `security find-generic-password` reports absence through its exit
    /// status. `cmdkey /list` exits 0 whether or not anything matched and
    /// prints `* NONE *` when nothing did, so on Windows the **output** is
    /// the witness, matched against the target name `keyring` composes —
    /// the account, a dot, then the service — which is also the string a
    /// user sees in Credential Manager.
    #[cfg(target_os = "macos")]
    pub fn os_cli_sees(account: &str) -> bool {
        std::process::Command::new("security")
            .args(["find-generic-password", "-s", SERVICE, "-a", account])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "windows")]
    pub fn os_cli_sees(account: &str) -> bool {
        let target = format!("{account}.{SERVICE}");
        std::process::Command::new("cmdkey")
            .arg("/list")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains(&target))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    use super::fixtures::{KeychainItem, os_cli_sees, test_account};
    use super::*;

    /// This module's own source with its `#[cfg(test)]` block excluded and
    /// `//` comments stripped — the same idiom as `super`'s own
    /// `production_code`.
    fn production_code(source: &str) -> String {
        source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one part")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A `StoreUnreachable` with the shape the platforms actually produce,
    /// for the tests that need one without a store to refuse them.
    ///
    /// Written here rather than as a `Default`: a refusal always comes from
    /// a real `keyring::Error` in production, and a constructor on the type
    /// would be a way to manufacture one that never happened.
    fn refused(status: Option<&str>) -> Unavailable {
        Unavailable::StoreUnreachable(StoreRefusal {
            classification: "the secure store would not grant access; it may be locked",
            status: status.map(str::to_owned),
        })
    }

    // --- what this module must never do ----------------------------------

    /// The same guarantee `secret::nothing_in_this_module_writes_to_disk`
    /// makes for the parent, extended to the child that actually handles
    /// credential values: a module that never opens a file cannot leak one
    /// into a file.
    #[test]
    fn nothing_in_this_module_writes_to_disk() {
        let code = production_code(include_str!("native.rs"));
        for forbidden in ["std::fs", "fs::", "File::", "OpenOptions"] {
            assert!(
                !code.contains(forbidden),
                "secret/native.rs names `{forbidden}` in production code: no credential value \
                 may be written to disk by any path here"
            );
        }
    }

    /// A `Serialize` reachable from here would put a credential into
    /// whatever the serializer writes. The serialised *reference* shape
    /// lives in `crate::config`, which is the point: names are serialised
    /// there, values are handled here, and the two files do not overlap.
    #[test]
    fn nothing_in_this_module_is_serializable() {
        let code = production_code(include_str!("native.rs"));
        for forbidden in ["Serialize", "Deserialize", "serde"] {
            assert!(
                !code.contains(forbidden),
                "secret/native.rs names `{forbidden}` in production code: nothing that touches \
                 a credential value may be serialized"
            );
        }
    }

    /// The scans above are only worth having if they can fail.
    #[test]
    fn the_source_scans_would_catch_a_violation() {
        let writing = "fn save(v: &str) {\n    std::fs::write(\"k\", v).unwrap();\n}";
        assert!(production_code(writing).contains("std::fs"));
        let derived = "#[derive(Serialize)]\nstruct Leak(String);";
        assert!(production_code(derived).contains("Serialize"));
        // ... and neither fires on a doc comment that merely mentions one.
        let documented = "/// Nothing here uses `std::fs` or `Serialize`.\nfn f() {}";
        assert!(!production_code(documented).contains("std::fs"));
        assert!(!production_code(documented).contains("Serialize"));
        // ... nor on test code.
        let tested = "fn f() {}\n#[cfg(test)]\nmod tests { use std::fs; }";
        assert!(!production_code(tested).contains("std::fs"));
    }

    // --- a reference is still only names ---------------------------------

    #[test]
    fn an_os_credential_reference_is_two_names_and_is_safe_to_print() {
        let reference = os_credential_for_variable("OPENROUTER_API_KEY");
        assert_eq!(
            reference,
            SecretRef::OsCredential {
                service: SERVICE.to_owned(),
                account: "OPENROUTER_API_KEY".to_owned(),
            }
        );

        let rendered = format!("{reference:?}");
        assert!(rendered.contains("OPENROUTER_API_KEY"), "got {rendered}");
        assert!(rendered.contains(SERVICE), "got {rendered}");
    }

    /// The mapping the whole "prefer the Keychain" behaviour rests on: an
    /// `Environment` reference and the `OsCredential` reference derived from
    /// the same variable name address the *same* item, so a credential
    /// stored through Settings is the one `crate::profile::resolve` finds.
    #[test]
    fn an_environment_reference_and_its_os_credential_name_the_same_item() {
        let var = "GLASSHOUSE_NATIVE_TEST_ONLY_VAR";
        assert_eq!(
            entry_name(&SecretRef::Environment {
                var: var.to_owned()
            }),
            entry_name(&os_credential_for_variable(var)),
        );
    }

    // --- line 2: the fallback is labelled, never silent -------------------

    /// Acceptance 2. With no native store, resolution falls back to the
    /// environment and `describe` says so — asserted on the label itself,
    /// not on the mere fact that a value came back.
    #[test]
    fn with_no_native_store_resolution_falls_back_and_describe_says_which_answered() {
        const VAR: &str = "GLASSHOUSE_NATIVE_TEST_ONLY_FALLBACK_VAR";
        const VALUE: &str = "sk-fallback-test-0123456789abcdefghij";

        // SAFETY: `VAR` is unique to this test and removed again below.
        // This test has no early return between the two.
        unsafe {
            std::env::set_var(VAR, VALUE);
        }

        let store = PreferNativeSecretStore::without_native(Unavailable::UnsupportedPlatform);
        let reference = SecretRef::Environment {
            var: VAR.to_owned(),
        };

        let resolved = store.resolve(&reference).is_some();
        let present = store.is_present(&reference);
        let describe = store.describe();
        let source = store.source_of(&reference);
        let unreachable_label = PreferNativeSecretStore::without_native(refused(Some(
            "Windows ERROR_NO_SUCH_LOGON_SESSION",
        )))
        .describe();

        unsafe {
            std::env::remove_var(VAR);
        }

        assert!(resolved, "the environment must still answer");
        assert!(present);
        assert_eq!(describe, UNSUPPORTED_PLATFORM_LABEL);
        assert!(
            describe.contains("process environment"),
            "the label must name the source that answered: {describe}"
        );
        assert_eq!(source, Some(EnvironmentSecretStore::new().describe()));

        // The two unavailable reasons are told apart, because one is
        // something a user can act on and the other is not.
        assert_eq!(unreachable_label, STORE_UNREACHABLE_LABEL);
        assert_ne!(unreachable_label, UNSUPPORTED_PLATFORM_LABEL);
    }

    /// An `OsCredential` reference against a store with no native half
    /// resolves to nothing rather than to some environment variable guessed
    /// from its account name.
    #[test]
    fn with_no_native_store_an_os_credential_reference_resolves_to_nothing() {
        const ACCOUNT: &str = "GLASSHOUSE_NATIVE_TEST_ONLY_ACCOUNT_VAR";

        // SAFETY: `ACCOUNT` is unique to this test and removed again below.
        unsafe {
            std::env::set_var(ACCOUNT, "sk-must-not-be-found-by-an-os-reference");
        }

        let store = PreferNativeSecretStore::without_native(Unavailable::UnsupportedPlatform);
        let resolved = store
            .resolve(&os_credential_for_variable(ACCOUNT))
            .is_some();
        let present = store.is_present(&os_credential_for_variable(ACCOUNT));

        unsafe {
            std::env::remove_var(ACCOUNT);
        }

        assert!(!resolved, "an OS reference must not be answered from env");
        assert!(!present);
    }

    #[test]
    fn every_unavailable_reason_has_text_and_they_differ() {
        assert_ne!(
            Unavailable::UnsupportedPlatform.reason(),
            refused(None).reason()
        );
        for reason in [Unavailable::UnsupportedPlatform, refused(None)] {
            assert!(!reason.reason().is_empty());
        }
    }

    /// The widened reason must actually *say* the platform's status, or the
    /// widening bought nothing: this is the assertion that would have failed
    /// on the Windows VM before this package, where five tests skipped with
    /// a sentence that could not tell a session problem from a broken
    /// backend.
    #[test]
    fn an_unreachable_stores_reason_carries_the_platforms_own_status() {
        const STATUS: &str = "Windows ERROR_NO_SUCH_LOGON_SESSION";

        let with_status = refused(Some(STATUS)).reason();
        assert!(
            with_status.contains("could not be opened"),
            "the reason must still say what happened: {with_status}"
        );
        assert!(
            with_status.contains(STATUS),
            "the reason must carry the platform's own status: {with_status}"
        );

        // ... and a refusal with no status says only what it knows, rather
        // than padding the sentence with an empty pair of brackets.
        let without = refused(None).reason();
        assert!(!without.contains('('), "got {without}");
        assert_ne!(with_status, without);
    }

    // --- nothing the store returns is ever carried out --------------------

    /// `keyring::Error::BadEncoding` carries the raw bytes the store
    /// returned. This proves the error type Glasshouse raises cannot carry
    /// them: it is built from a `&'static str` chosen by variant, so there
    /// is nowhere for a byte to go.
    #[test]
    fn a_store_error_never_carries_anything_the_store_returned() {
        const PLANTED: &str = "sk-planted-into-a-store-error-0123456789";

        let err = NativeStoreError::Refused {
            service: SERVICE.to_owned(),
            account: "OPENROUTER_API_KEY".to_owned(),
            reason: "the stored credential is not valid UTF-8",
        };
        let rendered = format!("{err} / {err:?}");
        assert!(!rendered.contains(PLANTED), "got {rendered}");
        assert!(rendered.contains("OPENROUTER_API_KEY"), "got {rendered}");

        let unavailable = NativeStoreError::Unavailable(refused(None));
        let rendered = format!("{unavailable} / {unavailable:?}");
        assert!(!rendered.contains(PLANTED), "got {rendered}");
        assert!(rendered.contains("could not be opened"), "got {rendered}");

        // The `reason` field is typed `&'static str`, so no value read out
        // of a store — which is never `'static` — can be put in it. This is
        // the structural half of the guarantee.
        let code = production_code(include_str!("native.rs"));
        assert!(
            code.contains("reason: &'static str"),
            "`NativeStoreError::Refused::reason` must stay `&'static str`: that type is what \
             makes it impossible to carry a value the store returned"
        );

        // `StoreRefusal` is the one thing here that carries a runtime
        // `String`, so its source is what has to be constrained rather than
        // its type. `platform_status` is the only producer of that field,
        // and it may name no variant whose payload is store data — planting
        // a value into one and asserting on the render would only prove
        // today's variants, so this is asserted on the match itself.
        let status_arm = code
            .split("fn platform_status")
            .nth(1)
            .expect("`platform_status` is what builds the only non-static reason here")
            .split("\n    }")
            .next()
            .expect("its body ends at this module's first dedented brace");
        for carries_store_data in ["BadEncoding", "Ambiguous", "TooLong", "Invalid"] {
            assert!(
                !status_arm.contains(carries_store_data),
                "`platform_status` names `keyring::Error::{carries_store_data}`, whose payload \
                 is data the store returned rather than a platform status: it may take a \
                 status only from `PlatformFailure` and `NoStorageAccess`"
            );
        }
        assert!(
            status_arm.contains("PlatformFailure") && status_arm.contains("NoStorageAccess"),
            "`platform_status` must still take the two status-carrying variants, or the \
             widened reason is empty on every platform: {status_arm}"
        );
    }

    /// The same guarantee against real `keyring::Error` values, on the
    /// platforms where the crate is linked at all.
    ///
    /// Not redundant with the source scan above: that one proves the match
    /// *names* no data-carrying variant, and this one proves the value that
    /// comes out of a data-carrying error renders nothing of its payload —
    /// including through `Unavailable::reason`, which is the string
    /// `doctor` prints.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_store_refusals_status_comes_from_no_variant_that_carries_store_data() {
        const PLANTED: &str = "sk-planted-into-a-keyring-error-01234567";

        for err in [
            keyring::Error::BadEncoding(PLANTED.as_bytes().to_vec()),
            keyring::Error::TooLong(PLANTED.to_owned(), 32),
            keyring::Error::Invalid(PLANTED.to_owned(), PLANTED.to_owned()),
            keyring::Error::NoEntry,
        ] {
            assert_eq!(
                backend::platform_status(&err),
                None,
                "`{err:?}` carries data the store returned or a name, not a platform status"
            );
            let refusal = backend::refusal(&err);
            let rendered = format!("{} / {refusal:?}", refusal.reason());
            assert!(!rendered.contains(PLANTED), "got {rendered}");
        }
    }

    /// Every store type's `Debug` is names only, with a credential planted
    /// in the environment the fallback reads from.
    #[test]
    fn no_store_debug_renders_a_credential() {
        const VAR: &str = "GLASSHOUSE_NATIVE_TEST_ONLY_DEBUG_VAR";
        const VALUE: &str = "sk-debug-render-test-abcdefghijklmnop";

        // SAFETY: `VAR` is unique to this test and removed again below.
        unsafe {
            std::env::set_var(VAR, VALUE);
        }

        let reference = SecretRef::Environment {
            var: VAR.to_owned(),
        };
        let rendered = format!(
            "{:?} {:?} {:?} {:?} {:?}",
            PreferNativeSecretStore::detect(),
            PreferNativeSecretStore::without_native(Unavailable::UnsupportedPlatform),
            reference,
            os_credential_for_variable(VAR),
            (Deletion::Removed, Deletion::AlreadyAbsent),
        );

        unsafe {
            std::env::remove_var(VAR);
        }

        assert!(
            !rendered.contains(VALUE),
            "a credential was rendered: {rendered}"
        );
        assert!(rendered.contains(VAR), "the NAME must survive: {rendered}");
    }

    // --- the Secret Service arm can never wait for a user -----------------

    /// This module's Linux backend with its `//` comments stripped — the
    /// slice the two scans below are about, taken between its own `mod`
    /// header and the no-backend arm that follows it.
    ///
    /// A source scan rather than a run-time one because the property is
    /// *"no code path here can raise a prompt"*, and a test can only ever
    /// exercise the paths it thought of. Both scans therefore run on every
    /// platform, including the ones with no Linux compiler in sight.
    fn secret_service_backend() -> String {
        let code = production_code(include_str!("native.rs"));
        let after = code
            .split("#[cfg(target_os = \"linux\")]")
            .nth(1)
            .expect("the Linux backend module")
            .to_owned();
        let scanned = after
            .split("#[cfg(not(any(")
            .next()
            .expect("the no-backend arm follows it")
            .to_owned();
        // Assert what was scanned, rather than trusting a slice: an empty or
        // truncated slice would make every assertion below pass for nothing.
        for landmark in [
            "fn open_service",
            "fn refuse_if_locked",
            "fn unlocked_default_collection",
            "pub fn probe",
            "pub fn delete",
        ] {
            assert!(
                scanned.contains(landmark),
                "the Linux backend slice is truncated: it does not reach `{landmark}`"
            );
        }
        scanned
    }

    /// **The invariant this whole arm exists for.** Nothing in the Secret
    /// Service backend may name an operation that can raise an unlock
    /// prompt, and the connection may only be opened by the constructor that
    /// bounds one.
    ///
    /// `SecretService::connect` is the constructor `keyring` uses and the
    /// reason line 442 was refused: it leaves the prompt timeout unset, and
    /// `dbus-secret-service` then waits a year. `unlock`, `ensure_unlocked`
    /// and `create_collection` are the three call shapes that reach a prompt
    /// even with a bounded timeout — bounded is still a wait, and this sits
    /// on the launch path.
    #[test]
    fn the_secret_service_backend_can_never_wait_for_an_unlock_prompt() {
        let code = secret_service_backend();

        assert!(
            code.contains("connect_with_max_prompt_timeout(ENCRYPTION, PROMPT_TIMEOUT_SECONDS)"),
            "the Linux backend must open its connection with the constructor that bounds a \
             prompt"
        );
        assert!(
            code.contains("const PROMPT_TIMEOUT_SECONDS: u64 = 0;"),
            "the prompt timeout must be zero: `dbus-secret-service` treats zero as `do not \
             raise the prompt at all`, and any other value is a wait on the launch path"
        );
        assert!(
            !code.contains("SecretService::connect("),
            "`SecretService::connect` leaves the prompt timeout unset, which is the exact \
             defect that kept the Secret Service out of this file"
        );
        for forbidden in [
            ".unlock(",
            ".ensure_unlocked(",
            ".unlock_all(",
            ".create_collection(",
        ] {
            assert!(
                !code.contains(forbidden),
                "the Linux backend names `{forbidden}`, which asks the Secret Service to \
                 unlock something and therefore to prompt: a locked collection is refused \
                 here, never opened"
            );
        }
        assert!(
            !code.contains(".message()"),
            "a D-Bus error's message is free text the provider composes; only its `name()`, \
             a fixed `org.freedesktop.*` identifier, may be carried out of here"
        );
        assert!(
            code.contains("EncryptionType::Dh") && !code.contains("EncryptionType::Plain"),
            "the session must be the encrypted one: `Plain` sends a provider credential over \
             the session bus in the clear"
        );
    }

    /// The probe reads `Locked` **before** anything touches an item, and it
    /// is structurally impossible for a later call to skip it.
    ///
    /// `unlocked_default_collection` is the only producer of a collection in
    /// the backend, so every search, read, write and delete is downstream of
    /// the guard by construction. That is asserted here rather than left to
    /// each caller remembering, because "remembering" is what a probe that
    /// searched for a never-written account was doing when it reported a
    /// locked keyring healthy.
    #[test]
    fn the_secret_service_probe_reads_locked_before_it_reads_an_item() {
        let code = secret_service_backend();

        assert_eq!(
            code.matches("get_default_collection(").count(),
            1,
            "`unlocked_default_collection` must be the only place a collection is obtained, \
             or a caller can reach an item without the locked check"
        );

        let guard = code
            .split("fn unlocked_default_collection")
            .nth(1)
            .expect("the one producer of a collection")
            .split("\n    }")
            .next()
            .expect("its body ends at this module's first dedented brace");
        assert!(
            guard.contains("is_locked()") && guard.contains("refuse_if_locked"),
            "the collection's `Locked` property must be read and refused on, before the \
             collection is handed to anything: {guard}"
        );
    }

    /// **Requirement 2, against a `proceed` that stands in for a prompt
    /// nobody answers.** A locked collection is refused, and the refusal
    /// names the locked state.
    ///
    /// The fake panics rather than blocking on purpose. A test that really
    /// waited would reproduce the hang this arm exists to prevent and would
    /// take the suite with it; panicking fails inside the bound and says
    /// exactly what went wrong.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_locked_collection_is_refused_before_anything_can_prompt() {
        // The ceiling the backend promises on the launch path: the bus
        // connect plus a handful of `dbus-secret-service`'s 2-second D-Bus
        // replies, with a prompt contributing nothing because one is never
        // raised. Declared here rather than beside the backend, where only a
        // test would read it and `-D dead-code` would refuse it.
        const PROBE_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

        let started = std::time::Instant::now();
        let refused = backend::refuse_if_locked::<()>(Ok(true), || {
            panic!(
                "the probe went on from a locked collection to an operation the Secret \
                 Service answers with an unlock prompt: where nobody answers it, that call \
                 does not return"
            )
        });
        let elapsed = started.elapsed();

        assert_eq!(refused, Err(backend::COLLECTION_LOCKED));
        assert!(
            backend::COLLECTION_LOCKED.contains("locked"),
            "the refusal must name the locked state: {}",
            backend::COLLECTION_LOCKED
        );
        assert!(
            elapsed < PROBE_BOUND,
            "refusing a locked collection must be immediate, not bounded-but-slow: {elapsed:?}"
        );

        // ... and an unlocked collection is handed straight through, or the
        // guard would refuse every Linux desktop rather than only locked ones.
        assert_eq!(backend::refuse_if_locked(Ok(false), || Ok(7)), Ok(7));
    }

    /// The Linux arm's status is the D-Bus error **name** and comes from no
    /// variant that carries a payload of its own — the same rule the macOS
    /// and Windows arm's `platform_status` obeys, asserted the same way, on
    /// the match rather than on today's values.
    #[test]
    fn a_secret_service_refusals_status_comes_from_no_variant_that_carries_a_payload() {
        let status = secret_service_backend();
        let status = status
            .split("fn platform_status")
            .nth(1)
            .expect("the Linux backend builds a status too")
            .split("\n    }")
            .next()
            .expect("its body ends at this module's first dedented brace");

        for carries_its_own in ["Crypto", "Path(", "Parse", "message"] {
            assert!(
                !status.contains(carries_its_own),
                "`platform_status` names `{carries_its_own}`, which is a payload of the \
                 crate's own rather than a status the provider named: {status}"
            );
        }
        assert!(
            status.contains("Error::Dbus") && status.contains("name()"),
            "the status must still be taken from the D-Bus error's name, or the widened \
             reason says nothing on Linux: {status}"
        );
    }

    /// Every Secret Service refusal is **diagnosis, semicolon, instruction**.
    ///
    /// The user's decision of 2026-09-03 asks for actionable installation or
    /// configuration instructions where no keyring is available. This is what
    /// makes them actually be there, rather than seven sentences describing a
    /// problem to somebody who wanted to fix it — and it reads the source
    /// rather than the values so it holds on macOS and Windows too, where the
    /// Linux constants do not exist to be read.
    #[test]
    fn every_secret_service_refusal_carries_an_instruction() {
        let code = secret_service_backend();

        for name in [
            "NO_SESSION_BUS",
            "NO_PROVIDER",
            "COLLECTION_LOCKED",
            "NO_DEFAULT_COLLECTION",
            "PROMPT_REFUSED",
            "BAD_ENCODING",
            "REFUSED",
        ] {
            let literal = code
                .split(&format!("{name}: &str = "))
                .nth(1)
                .unwrap_or_else(|| panic!("`{name}` is not a classification in this backend"))
                .split("\";")
                .next()
                .expect("a string literal ends at its closing quote");
            assert!(
                literal.contains("; "),
                "`{name}` tells the user what happened and not what to do about it; every \
                 classification here is a diagnosis, a semicolon, and an instruction: {literal}"
            );
        }
    }

    // --- the backend that is linked is the platform's own ----------------

    /// **The mock guard, at run time.** `keyring` 3.x resolves
    /// `keyring::default` to an in-process mock store whenever no backend
    /// feature is enabled, and a mock accepts a credential, hands it back
    /// inside the process, persists nothing, and reports itself as a working
    /// secure store. This asks the backend to build the credential it would
    /// use and refuses to accept a mock one.
    ///
    /// Not redundant with the compile-time guard. `platform_credential_builder`
    /// makes a *missing feature* a build error; this makes a *wrong
    /// credential* a test failure, and the two catch different mistakes —
    /// the second would also catch a future path that went back through
    /// `keyring::Entry::new`, whose builder anything in the process can
    /// replace.
    ///
    /// Deliberately does not need the store to open: what is linked is a
    /// property of the build, not of the session, so this runs and means the
    /// same thing on a runner with no login keychain.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_store_is_built_by_the_platforms_own_backend_and_never_the_mock() {
        let entry = backend::entry(SERVICE, PROBE_ACCOUNT).expect("the backend must name an entry");
        let credential = entry.get_credential();

        assert!(
            credential
                .downcast_ref::<keyring::mock::MockCredential>()
                .is_none(),
            "this build's secure store is `keyring`'s in-process mock: it would take a \
             provider credential, hand it back within the process, persist nothing, and \
             still describe itself as the operating system's store"
        );

        #[cfg(target_os = "macos")]
        assert!(
            credential
                .downcast_ref::<keyring::macos::MacCredential>()
                .is_some(),
            "the credential must be the Keychain's own"
        );
        #[cfg(target_os = "windows")]
        assert!(
            credential
                .downcast_ref::<keyring::windows::WinCredential>()
                .is_some(),
            "the credential must be Credential Manager's own"
        );
    }

    /// The arrangement label must name *this* platform's store. A Windows
    /// user told "the macOS Keychain" would be misinformed in the one place
    /// this line exists to inform them, and the two constants live in
    /// different places, so this is what stops them drifting apart.
    #[test]
    fn the_native_first_label_names_this_platforms_store() {
        assert!(
            NATIVE_FIRST_LABEL.starts_with(backend::LABEL),
            "`{NATIVE_FIRST_LABEL}` must begin by naming this platform's store, \
             `{}`",
            backend::LABEL
        );
        assert!(
            NATIVE_FIRST_LABEL.contains("process environment"),
            "the label must also name the fallback: {NATIVE_FIRST_LABEL}"
        );
        // All three arrangements are told apart, because a user reads this
        // string to find out where their key actually came from.
        for other in [UNSUPPORTED_PLATFORM_LABEL, STORE_UNREACHABLE_LABEL] {
            assert_ne!(NATIVE_FIRST_LABEL, other);
        }
    }

    /// `detect` offers the native store on exactly the platforms with a
    /// backend, and refuses with the *unchangeable* reason everywhere else.
    ///
    /// The negative half is the one that matters: on a target with no
    /// backend a successful `detect` would mean something was linked that
    /// this project has not proven, which is how a mock would first show up
    /// at run time.
    #[test]
    fn detect_offers_a_native_store_on_exactly_the_platforms_with_a_backend() {
        let detected = NativeSecretStore::detect();

        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        assert!(
            !matches!(detected, Err(Unavailable::UnsupportedPlatform)),
            "this platform has a backend, so the only honest refusal is \
             `StoreUnreachable`"
        );

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        assert_eq!(
            detected,
            Err(Unavailable::UnsupportedPlatform),
            "no backend is compiled for this target, so nothing may hand back a store"
        );
    }

    /// **Acceptance 1, gated to the two platforms that have a backend.**
    /// The Secret Service is deliberately not implemented and deliberately
    /// not claimed; this runs where a real store answers, and there is no
    /// arm of it a mock could satisfy, because on every other target the
    /// dependency is not linked at all.
    ///
    /// Skipped, loudly, when the store will not open — a CI runner with no
    /// login keychain, or a Windows session with no credential store, is a
    /// real state, and a test that pretended to have proven something there
    /// would be worse than one that says it could not.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_credential_stored_in_the_native_store_resolves_and_deletes() {
        const VALUE: &str = "sk-keychain-round-trip-test-0123456789abcdef";

        let store = match NativeSecretStore::detect() {
            Ok(store) => store,
            Err(refusal) => {
                eprintln!(
                    "SKIPPED: the native secure store would not open in this session, so this \
                     test proved nothing: {}",
                    refusal.reason()
                );
                return;
            }
        };

        let account = test_account("ROUNDTRIP");
        let reference = os_credential_for_variable(&account);

        let item = KeychainItem::stored(&store, reference.clone(), VALUE);
        assert!(
            os_cli_sees(&account),
            "the platform's own credential CLI must see the item that was just stored"
        );

        assert!(store.is_present(&reference));
        let resolved = store.resolve(&reference).expect("resolve");
        assert!(
            resolved.expose() == VALUE,
            "the stored value must come back unchanged"
        );

        // The same credential, addressed the way `crate::profile::resolve`
        // addresses one: by the variable name a harness expects it in.
        let as_variable = SecretRef::Environment {
            var: account.clone(),
        };
        let via_variable = store.resolve(&as_variable).expect("resolve by variable");
        assert!(via_variable.expose() == VALUE);

        // Acceptance 3, first half.
        assert_eq!(store.delete(&reference).expect("delete"), Deletion::Removed);
        assert!(!store.is_present(&reference));
        assert!(store.resolve(&reference).is_none());
        assert!(
            !os_cli_sees(&account),
            "the platform's own credential CLI must no longer see the item after deletion"
        );

        // Acceptance 3, second half: deleting what is already gone is the
        // desired state, reported rather than raised.
        assert_eq!(
            store
                .delete(&reference)
                .expect("a second delete is not an error"),
            Deletion::AlreadyAbsent
        );

        // The guard has nothing left to remove, which is the point: a third
        // delete on the way out is still not an error.
        drop(item);
    }

    /// Lines 1 and 441: a credential in the OS store beats one exported in
    /// the shell that launched Glasshouse, because the stored one was
    /// chosen deliberately. Both are planted at once so the test cannot
    /// pass by the environment simply being empty.
    ///
    /// The label is asserted against `backend::LABEL` rather than a
    /// literal, so this one test says "prefer the macOS Keychain" on macOS
    /// and "prefer Windows Credential Manager" on Windows without either
    /// claim being written twice.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_native_store_is_preferred_over_the_environment() {
        const STORED: &str = "sk-from-the-keychain-0123456789abcdefghij";
        const EXPORTED: &str = "sk-from-the-environment-abcdefghij0123456";

        let native = match NativeSecretStore::detect() {
            Ok(native) => native,
            Err(refusal) => {
                eprintln!(
                    "SKIPPED: the native secure store would not open in this session: {}",
                    refusal.reason()
                );
                return;
            }
        };

        let account = test_account("PREFERENCE");
        let reference = SecretRef::Environment {
            var: account.clone(),
        };
        let _item = KeychainItem::stored(&native, reference.clone(), STORED);

        // SAFETY: the variable name is unique to this process and is
        // removed below, before every assertion that could fail.
        unsafe {
            std::env::set_var(&account, EXPORTED);
        }

        let store = PreferNativeSecretStore::detect();
        let resolved = store.resolve(&reference).map(|s| s.expose().to_owned());
        let describe = store.describe();
        let source = store.source_of(&reference);

        unsafe {
            std::env::remove_var(&account);
        }

        assert!(
            resolved.as_deref() == Some(STORED),
            "the native store must win"
        );
        assert_eq!(describe, NATIVE_FIRST_LABEL);
        assert_eq!(source, Some(backend::LABEL));
    }

    /// With the native store holding nothing for a name, the environment
    /// still answers it — the fallback half of the same store, on a machine
    /// where the native half is genuinely available.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_environment_answers_what_the_native_store_does_not_hold() {
        const EXPORTED: &str = "sk-only-in-the-environment-0123456789abcd";

        let account = test_account("ENVONLY");

        // SAFETY: the variable name is unique to this process and removed
        // below, before every assertion.
        unsafe {
            std::env::set_var(&account, EXPORTED);
        }

        let store = PreferNativeSecretStore::detect();
        let reference = SecretRef::Environment {
            var: account.clone(),
        };
        let resolved = store.resolve(&reference).map(|s| s.expose().to_owned());
        let source = store.source_of(&reference);

        unsafe {
            std::env::remove_var(&account);
        }

        assert!(resolved.as_deref() == Some(EXPORTED));
        assert_eq!(source, Some("process environment"));
    }

    /// [`Presence`] tells the two answers apart for an item **this binary
    /// wrote**, which is the easy half and the one that must not regress.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn presence_reports_an_item_this_binary_wrote_as_present_and_a_missing_one_as_absent() {
        const VALUE: &str = "sk-presence-round-trip-test-0123456789abcd"; // glasshouse:not-a-secret

        let store = match NativeSecretStore::detect() {
            Ok(store) => store,
            Err(refusal) => {
                eprintln!(
                    "SKIPPED: the native secure store would not open in this session: {}",
                    refusal.reason()
                );
                return;
            }
        };

        let missing = SecretRef::Environment {
            var: test_account("PRESENCE_MISSING"),
        };
        assert_eq!(
            store.presence(&missing),
            Presence::Absent,
            "a name nothing was ever filed under is absent, not refused"
        );

        let reference = SecretRef::Environment {
            var: test_account("PRESENCE"),
        };
        let _item = KeychainItem::stored(&store, reference.clone(), VALUE);
        assert_eq!(
            store.presence(&reference),
            Presence::Present,
            "an item this binary wrote is one this binary reads"
        );
        assert!(
            SecretStore::is_present(&store, &reference),
            "`is_present` is `presence` reduced to one bit and must agree with it"
        );
    }

    /// **The dogfooding defect of 2026-09-06, as a test.** An item created
    /// by another program is *refused*, not *absent* — and reporting it as
    /// absent is what made `glasshouse doctor` say *not set* about a key
    /// that was in the Keychain all along.
    ///
    /// Planted with the platform's own CLI on purpose: an item this binary
    /// wrote would be on its own access control list and would prove
    /// nothing. `security` is the exact tool the old instruction told users
    /// to run, so this is that user's item.
    ///
    /// macOS only, because the mechanism is macOS's: an item's access
    /// control list names the program that created it, and
    /// `silence_authorization_dialogs` has turned the *Allow* dialog this
    /// binary would otherwise be offered into an error. Windows Credential
    /// Manager has no per-item access control of that kind, and the Secret
    /// Service answers a search without decrypting anything — see
    /// `backend::presence` on each.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_item_another_program_created_is_refused_and_never_reported_absent() {
        const PLANTED: &str = "placeholder-not-a-real-key-planted-by-a-test"; // glasshouse:not-a-secret

        /// Removes the planted item however this test leaves.
        struct Planted(String);
        impl Drop for Planted {
            fn drop(&mut self) {
                let _ = std::process::Command::new("security")
                    .args([
                        "delete-generic-password",
                        "-s",
                        SERVICE,
                        "-a",
                        self.0.as_str(),
                    ])
                    .output();
            }
        }

        let store = match NativeSecretStore::detect() {
            Ok(store) => store,
            Err(refusal) => {
                eprintln!(
                    "SKIPPED: the native secure store would not open in this session: {}",
                    refusal.reason()
                );
                return;
            }
        };

        let account = test_account("FOREIGN");
        let planted = std::process::Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                SERVICE,
                "-a",
                account.as_str(),
                "-w",
                PLANTED,
                "-U",
            ])
            .output();
        let Ok(planted_output) = planted else {
            eprintln!("SKIPPED: the `security` CLI could not be run, so nothing was planted");
            return;
        };
        if !planted_output.status.success() {
            eprintln!(
                "SKIPPED: `security add-generic-password` refused, so nothing was planted \
                 (status {})",
                planted_output.status
            );
            return;
        }
        let _cleanup = Planted(account.clone());
        assert!(
            os_cli_sees(&account),
            "the planted item must really be in the keychain for this test to mean anything"
        );

        let reference = SecretRef::Environment { var: account };
        assert_eq!(
            store.presence(&reference),
            Presence::Refused,
            "an item this binary did not create must read as refused, not absent -- reporting \
             it absent is the defect this state exists to name"
        );
        assert!(
            !SecretStore::is_present(&store, &reference),
            "a credential the gateway cannot read is still one it cannot use"
        );
        assert!(
            store.resolve(&reference).is_none(),
            "and it certainly must not resolve"
        );
    }
}
