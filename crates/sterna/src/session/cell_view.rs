//! The screen's account of one cell's calls, moved out of `session.rs` on
//! 2026-09-23 for the size ratchet. Nothing here decides anything: it is
//! the one string the workbench's chain of calls is drawn from.

use super::*;

/// One line per call the cell made, in order: the tool it ran, the path,
/// command or source it was given, and how it ended.
pub(super) fn execution(record: &CellRecord) -> String {
    if record.calls.is_empty() {
        "No tool calls ran in this cell.".into()
    } else {
        record
            .calls
            .iter()
            .enumerate()
            .map(|(i, call)| {
                let status = match &call.ended {
                    crate::runtime::outcome::Ended::Ok if call.tool == "agent.run" => {
                        "started".to_string()
                    }
                    crate::runtime::outcome::Ended::Ok => "returned".to_string(),
                    crate::runtime::outcome::Ended::Threw { class } => {
                        format!("failed · {class}")
                    }
                    crate::runtime::outcome::Ended::Denied { rule } => {
                        format!("denied · {rule}")
                    }
                };
                // A lifted call shows what the model asked for and what
                // sterna ran for it, so the screen never implies the model
                // reached for a capability it did not name
                // (`semantic-command-lifting.md`, *TUI, ledger and
                // telemetry*).
                let ran = match &call.lifted_from {
                    Some(written) => format!("{written} ↳ {}", call.tool),
                    None => call.tool.clone(),
                };
                format!(
                    "{} {}{} · {status}",
                    if i + 1 == record.calls.len() {
                        "└─"
                    } else {
                        "├─"
                    },
                    ran,
                    call.args
                        .get("path")
                        .map(|path| format!(
                            " {}",
                            std::path::Path::new(path)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        ))
                        .or_else(|| call
                            .args
                            .get("command")
                            .map(|command| format!(" {command}")))
                        .or_else(|| call.args.get("source").map(|source| format!(" {source}")))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
