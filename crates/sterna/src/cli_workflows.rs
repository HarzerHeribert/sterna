//! CLI conveniences use the same session dispatch and permission boundary.
use clap::{CommandFactory, Parser};
use serde::Serialize;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

pub fn session_options(args: &[String]) -> Vec<String> {
    let mut args = args.to_vec();
    if !args
        .iter()
        .any(|arg| arg == "--root" || arg.starts_with("--root="))
    {
        args.extend(["--root".into(), ".".into()]);
    }
    args
}

pub fn prepare(args: &[String]) -> Result<Option<Vec<String>>, String> {
    let Some(first) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    let mut forwarded = match first {
        "exec" => {
            let mut rest = args[1..].to_vec();
            if rest.first().is_some_and(|arg| !arg.starts_with('-')) {
                let task = rest.remove(0);
                rest.extend(["--task".into(), task]);
            } else if !rest.iter().any(|arg| {
                matches!(arg.as_str(), "--task" | "--help" | "-h") || arg.starts_with("--task=")
            }) {
                if std::io::stdin().is_terminal() {
                    return Err("exec requires a task or piped stdin".into());
                }
                let mut task = String::new();
                std::io::stdin()
                    .read_to_string(&mut task)
                    .map_err(|e| format!("cannot read stdin: {e}"))?;
                if task.trim().is_empty() {
                    return Err("exec received an empty task on stdin".into());
                }
                rest.extend(["--task".into(), task]);
            }
            rest
        }
        "-p" | "--print" => {
            if args.len() < 2 {
                return Err("--print requires a task".into());
            }
            let mut rest = vec!["--task".into(), args[1].clone()];
            rest.extend_from_slice(&args[2..]);
            rest
        }
        "--continue" => {
            let mut rest = vec!["--resume=".into()];
            rest.extend_from_slice(&args[1..]);
            rest
        }
        option if session_option(option) => args.to_vec(),
        _ => return Ok(None),
    };
    forwarded = session_options(&forwarded);
    Ok(Some(forwarded))
}

fn session_option(option: &str) -> bool {
    let Some(name) = option.strip_prefix("--") else {
        return false;
    };
    let name = name.split('=').next().unwrap_or(name);
    sterna::session::SessionArgs::command()
        .get_arguments()
        .any(|arg| arg.get_long() == Some(name))
}

#[derive(Parser)]
#[command(
    name = "sterna doctor",
    about = "Read-only local diagnostics; never starts a gateway or contacts a provider"
)]
struct DoctorArgs {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    version: &'static str,
    platform: &'static str,
    root: PathBuf,
    ok: bool,
    checks: Vec<Check>,
}

pub fn doctor(args: &[String]) -> i32 {
    let args = match DoctorArgs::try_parse_from(
        std::iter::once("sterna doctor".into()).chain(args.iter().cloned()),
    ) {
        Ok(args) => args,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            return code;
        }
    };
    let mut checks = Vec::new();
    let root = match args.root.canonicalize() {
        Ok(root) if root.is_dir() => {
            checks.push(Check {
                name: "project",
                status: "ok",
                detail: "Project directory exists".into(),
            });
            root
        }
        _ => {
            checks.push(Check {
                name: "project",
                status: "error",
                detail: "Project root does not exist or is not a directory".into(),
            });
            args.root
        }
    };
    for line in sterna::relocate::left_alone(Some(&root)) {
        checks.push(Check {
            name: "rename",
            status: "warning",
            detail: line,
        });
    }
    let store = sterna::settings::Store::new(&root);
    let mut settings: Option<(
        sterna::config::SternaConfig,
        std::collections::BTreeMap<String, String>,
    )> = None;
    let mut full_access = false;
    match store
        .as_ref()
        .map_err(|e| e.clone())
        .and_then(|store| store.load(None))
    {
        Ok(loaded) => {
            checks.push(Check {name:"config",status:if loaded.config.model.parent.is_some(){"ok"}else{"warning"},detail:if loaded.config.model.parent.is_some(){"Native/legacy configuration parses; parent model configured".into()}else{"Configuration parses; supply --model or sterna config local model.parent to start a task".into()}});
            full_access = loaded
                .values
                .get("sandbox")
                .and_then(|table| table.get("level"))
                .and_then(toml::Value::as_str)
                .is_some_and(|level| level == "full");
            settings = Some((loaded.config, loaded.origins));
        }
        Err(_) => checks.push(Check {
            name: "config",
            status: "error",
            detail: "Configuration is invalid or unreadable; no values exposed".into(),
        }),
    }
    let mut project = sterna::project::load(&root);
    project.settings = store
        .as_ref()
        .ok()
        .and_then(|store| store.permissions().ok())
        .flatten();
    let profile = sterna::sandbox::profile::Profile::from_project(&project);
    checks.push(Check {
        name: "permissions",
        status: if profile.diagnostics().is_empty() {
            "ok"
        } else {
            "warning"
        },
        detail: format!(
            "{} file rules, {} pre-approved commands, {} MCP grants, {} diagnostics",
            profile.rule_count(),
            profile.pre_approved().len(),
            profile.mcp_tool_count(),
            profile.diagnostics().len()
        ),
    });
    if let Some((config, origins)) = settings.as_ref() {
        checks.push(inert_settings(config, origins));
    }
    for program in ["inference-gateway", "git", "rg", "fd", "jq"] {
        let found = find_executable(program);
        let attached =
            program == "inference-gateway" && std::env::var_os("ANTHROPIC_BASE_URL").is_some();
        checks.push(Check {
            name: program,
            status: if found.is_some() || attached {
                "ok"
            } else {
                "warning"
            },
            detail: if attached {
                "Gateway endpoint supplied in environment (value redacted; connectivity not probed)"
                    .into()
            } else {
                found
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Not found on PATH".into())
            },
        });
    }
    if let Some((config, _)) = settings.as_ref() {
        checks.push(decisions_check(&config.decisions));
    }
    checks.push(sandbox_check(full_access));
    let report = Report {
        schema_version: 1,
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        root,
        ok: !checks.iter().any(|check| check.status == "error"),
        checks,
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("report is serializable")
        );
    } else {
        println!(
            "Sterna {} diagnostics ({})\nProject: {}",
            report.version,
            report.platform,
            report.root.display()
        );
        for check in &report.checks {
            println!("{} {}: {}", check.status, check.name, check.detail);
        }
    }
    if report.ok { 0 } else { 1 }
}

/// Whether Jev -- the decision model every small judgement goes to -- will
/// answer: a configured model, or the default the session takes when the
/// gateway serves a TypeSafe account. Without one every decision question
/// is inert and Sterna runs its fallbacks, which is a warning, not an error.
fn decisions_check(decisions: &sterna::config::DecisionsConfig) -> Check {
    let mode = decisions.mode.as_str();
    if decisions.mode == sterna::config::DecisionMode::Off {
        return Check {
            name: "decisions",
            status: "warning",
            detail: "[decisions] mode is `off`: no decision question is asked and Sterna runs every fallback".into(),
        };
    }
    if let Some(model) = &decisions.model {
        return Check {
            name: "decisions",
            status: "ok",
            detail: format!("decision model `{model}`, mode {mode}"),
        };
    }
    let serves_typesafe = find_executable("inference-gateway")
        .and_then(|gateway| {
            std::process::Command::new(gateway)
                .args(["entitlements", "--json"])
                .output()
                .ok()
        })
        .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out.stdout).ok())
        .is_some_and(|listing| {
            listing["accounts"].as_array().is_some_and(|accounts| {
                accounts.iter().any(|account| {
                    account["provider"].as_str() == Some("typesafe")
                        && account["selectable"].as_bool() != Some(false)
                })
            })
        });
    if serves_typesafe {
        Check {
            name: "decisions",
            status: "ok",
            detail: format!(
                "decision model `{}` by default (the gateway serves a TypeSafe account), mode {mode}",
                sterna::decide::DEFAULT_MODEL
            ),
        }
    } else {
        Check {
            name: "decisions",
            status: "warning",
            detail: "no decision model: [decisions] model is unset and the gateway serves no TypeSafe account, so every decision question is inert; add a typesafe account to the gateway or set `decisions.model`".into(),
        }
    }
}

/// Settings a person actually set that cannot take effect, each named with
/// the prerequisite that is missing.
///
/// The invariant: **a setting that does nothing says so where a person
/// looks.** `sterna config local` saves any key it recognises and `doctor`
/// only asked whether the file parsed, so on 2026-09-19 five `[helpers]`
/// keys were set, saved, reported `ok`, and did nothing all session — the
/// only place the truth appeared was inside the system prompt, which a
/// person never reads.
///
/// **Only a configured key is reported**, read from `origins`: several of
/// these features are on by default with no model behind them, so warning
/// on the effective value alone would make a stock installation complain
/// about settings nobody chose, and a standing complaint is not a signal.
fn inert_settings(
    config: &sterna::config::SternaConfig,
    origins: &std::collections::BTreeMap<String, String>,
) -> Check {
    let chosen = |key: &str| origins.get(key).is_some_and(|origin| origin != "built-in");
    let any_chosen = |keys: &[&str]| keys.iter().any(|key| chosen(key));
    let mut inert: Vec<String> = Vec::new();
    // `session::system::system_manifest` writes the matching `Unavailable:`
    // line on exactly this predicate.
    if config.helpers.enabled
        && config.helpers.model.is_none()
        && any_chosen(&[
            "helpers.enabled",
            "helpers.preflight",
            "helpers.preflight_scope",
            "helpers.completion_check",
            "helpers.acceptance_list",
        ])
    {
        inert.push(
            "[helpers] is enabled but names no model, so preflight, completion_check, acceptance_list and every helper.* call are inert; set `helpers.model`"
                .into(),
        );
    }
    // `[decisions] mode` beyond `off` needs a model for the same reason the
    // runtime binds `decide` on one.
    if config.decisions.mode != sterna::config::DecisionMode::Off
        && config.decisions.model.is_none()
        && chosen("decisions.mode")
    {
        inert.push(
            "[decisions] mode is not `off` but names no model, so no decision is ever asked; set `decisions.model`"
                .into(),
        );
    }
    if config.web.enabled
        && !config.web.search_configured()
        && config.web.allow_domains.is_empty()
        && chosen("web.enabled")
    {
        inert.push(
            "[web] is enabled but allows no domain and configures no search provider, so both web.fetch and web.search refuse"
                .into(),
        );
    }
    if inert.is_empty() {
        Check {
            name: "settings",
            status: "ok",
            detail: "Every configured feature has the prerequisite it needs".into(),
        }
    } else {
        Check {
            name: "settings",
            status: "warning",
            detail: inert.join(" | "),
        }
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).find_map(|dir| {
        let path = dir.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        });
        executable(&path).then_some(path)
    })
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Which confinement a session started on this configuration would apply --
/// not which backend was compiled in.
///
/// The invariant: **this check answers for the configuration in hand.** It
/// said "Seatbelt backend compiled" until 2026-09-19, which is a fact about
/// the build and true no matter what the person had configured; someone who
/// had set the rung and the grant read it as confirmation and spent twelve
/// cells finding out otherwise.
fn sandbox_check(bypassed: bool) -> Check {
    if bypassed {
        return Check {
            name: "sandbox",
            status: "warning",
            // Precise about what the bit actually does, because
            // `Profile::container_mode` is the SAME field as
            // `os_sandbox_bypassed`: turning it on grants reads
            // filesystem-wide and drops debuggers from the never-grantable
            // set. A surface that said only "no OS confinement" here would
            // be the same comforting half-truth this check exists to end.
            detail: "None for this configuration: `sandbox.level` is `full`, so Sterna applies no OS confinement to the children it spawns and this machine is the boundary, network included, and a debugger is admitted. Every deny pattern and the rest of the never-grantable set refuse exactly as they do confined.".into(),
        };
    }
    #[cfg(target_os = "linux")]
    {
        let abi = sterna::sandbox::linux::landlock_abi();
        Check {
            name: "sandbox",
            status: if abi >= 3 { "ok" } else { "error" },
            detail: format!(
                "Landlock confines every child this configuration spawns; ABI {abi}, and at least ABI 3 is required for tool spawning. Network enforcement is reported separately by the session."
            ),
        }
    }
    #[cfg(target_os = "macos")]
    {
        Check {
            name: "sandbox",
            status: "ok",
            detail: "Seatbelt confines every child this configuration spawns; per-command enforcement occurs at spawn".into(),
        }
    }
    #[cfg(target_os = "windows")]
    {
        // The object-manager access check enforces every file grant
        // unconditionally, but the missing `internetClient` capability is
        // enforced by the Windows Filtering Platform through the Windows
        // Firewall service (`MpsSvc`) -- a service, not the access check --
        // so `sterna::sandbox::windows::network_isolation` measures it rather
        // than assuming it. A fully-confined machine reported "warning"
        // regardless was this check's own complaint restated: shouting about
        // something that is fine has a cost too.
        //
        // **`sterna::`, not `crate::`, like the Linux arm above.** This file is
        // `mod cli_workflows` in `main.rs` and in nothing else, so `crate::`
        // is the BINARY's root, which declares no `sandbox` -- and no check
        // on this host compiles it, so the error surfaced on the Windows cell
        // and nowhere earlier.
        use sterna::sandbox::windows::NetworkIsolation;
        let isolation = sterna::sandbox::windows::network_isolation();
        let status = match isolation {
            NetworkIsolation::EnforcedByFirewall => "ok",
            NetworkIsolation::NotEnforced | NetworkIsolation::Unknown => "warning",
        };
        Check {
            name: "sandbox",
            status,
            detail: format!(
                "An AppContainer confines every child this configuration spawns; network isolation is {isolation}"
            ),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Check {
            name: "sandbox",
            status: "error",
            detail: "No supported OS sandbox backend; tool spawning is refused".into(),
        }
    }
}
