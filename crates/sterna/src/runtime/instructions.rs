//! Task-scoped gate between path discovery and side effects.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::project::instructions;
use crate::sandbox::profile::Profile;
use crate::tools::invoke::Args;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInstructions {
    pub text: String,
    /// The applicable policy could not be loaded completely. The blocked
    /// call must remain blocked; acknowledging this does not make it safe.
    pub fatal: bool,
}

#[derive(Debug, Default)]
pub(crate) struct InstructionContext {
    enabled: bool,
    known: BTreeMap<(PathBuf, PathBuf), String>,
    pending_documents: Vec<((PathBuf, PathBuf), String)>,
    pending: Option<PendingInstructions>,
    /// The index's scan budget ran out once this task and the model was told;
    /// the notice is delivered once, never as a stop (see `gate`).
    budget_noticed: bool,
}

impl InstructionContext {
    pub(crate) fn enable(&mut self, profile: &Profile) {
        self.enabled = true;
        let root = instructions::docs_for_paths(profile, &[profile.root().to_path_buf()]);
        if root.complete {
            self.known.extend(
                root.documents
                    .into_iter()
                    .map(|doc| ((doc.path, doc.scope), doc.text)),
            );
        } else {
            self.pending = Some(PendingInstructions {
                text: render_omissions(&root.omissions),
                fatal: true,
            });
        }
    }

    pub(crate) fn gate(&mut self, profile: &Profile, tool: &str, args: &Args) -> bool {
        if !self.enabled {
            return false;
        }
        if self.pending.is_some() {
            return true;
        }

        let mut budget_notice = None;
        let load = if matches!(tool, "bash" | "bg.run" | "bg.watch" | "context") {
            let index = instructions::index(profile);
            // A budget the scan ran out of is a notice, not a stop: the
            // index enumerated every directory it reached, shallow scopes
            // first, and what it did not reach is data by volume (the
            // 2026-09-13 full-suite run: a downloaded dataset of 60,000
            // files ended a 32-cell task here). A document that exists and
            // cannot be read stays fatal below.
            let budget_only = index.only_budget_omissions();
            if !index.complete && !budget_only {
                self.pending = Some(PendingInstructions {
                    text: render_omissions(&index.omissions),
                    fatal: true,
                });
                return true;
            }
            if budget_only && !self.budget_noticed {
                self.budget_noticed = true;
                let mut notice = render_omissions(&index.omissions);
                notice.push_str(
                    "\nThe instruction index stopped at its scan budget, shallow directories \
                     first; instruction files deeper than the scanned set are not loaded. \
                     The blocked call did not run; it may be repeated.\n",
                );
                budget_notice = Some(notice);
            }
            let mut paths = index.paths;
            paths.push(profile.root().to_path_buf());
            instructions::docs_for_paths(profile, &paths)
        } else {
            let paths = tool_paths(profile, tool, args);
            if paths.is_empty() {
                return false;
            }
            instructions::docs_for_paths(profile, &paths)
        };

        let fresh: Vec<_> = load
            .documents
            .into_iter()
            .filter(|doc| self.known.get(&(doc.path.clone(), doc.scope.clone())) != Some(&doc.text))
            .collect();
        let fatal = !load.complete;
        if fresh.is_empty() && !fatal && budget_notice.is_none() {
            return false;
        }
        self.pending_documents = if fatal {
            Vec::new()
        } else {
            fresh
                .iter()
                .map(|doc| ((doc.path.clone(), doc.scope.clone()), doc.text.clone()))
                .collect()
        };
        let mut text = String::from("## Newly applicable project instructions\n");
        if fatal {
            text.push_str("\nThe applicable instruction set could not be loaded completely. The blocked call did not run.\n");
        }
        if !fatal {
            for doc in &fresh {
                let replacement = if self
                    .known
                    .contains_key(&(doc.path.clone(), doc.scope.clone()))
                {
                    " This full document replaces the earlier version from the same path and scope."
                } else {
                    ""
                };
                text.push_str(&format!(
                    "\n### `{}` (scope `{}`)\n\n{replacement}\n\n{}\n",
                    doc.path.display(),
                    doc.scope.display(),
                    doc.text.trim_end()
                ));
            }
        }
        if !load.omissions.is_empty() {
            text.push_str(&render_omissions(&load.omissions));
        }
        if let Some(notice) = budget_notice {
            text.push_str(&notice);
        }
        self.pending = Some(PendingInstructions { text, fatal });
        true
    }

    pub(crate) fn instruction_file_written(
        &mut self,
        profile: &Profile,
        tool: &str,
        args: &Args,
    ) -> bool {
        if !self.enabled || !matches!(tool, "write" | "edit") {
            return false;
        }
        let Some(path) = args.get("path").map(PathBuf::from) else {
            return false;
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        if name != "AGENTS.md" && name != "CLAUDE.md" {
            return false;
        }
        self.gate(profile, tool, args)
    }

    pub(crate) fn pending(&self) -> Option<PendingInstructions> {
        self.pending.clone()
    }

    pub(crate) fn acknowledge(&mut self) {
        let fatal = self.pending.as_ref().is_some_and(|pending| pending.fatal);
        if !fatal {
            self.known.extend(self.pending_documents.drain(..));
            self.pending = None;
        }
    }
}

fn tool_paths(profile: &Profile, tool: &str, args: &Args) -> Vec<PathBuf> {
    match tool {
        "read" | "write" | "context" | "edit" => {
            args.get("path").map(PathBuf::from).into_iter().collect()
        }
        "grep" => vec![
            args.get("path")
                .map(PathBuf::from)
                .unwrap_or_else(|| profile.root().to_path_buf()),
        ],
        "glob" => vec![
            args.get("path")
                .map(PathBuf::from)
                .unwrap_or_else(|| profile.root().to_path_buf()),
        ],
        _ => Vec::new(),
    }
}

fn render_omissions(omissions: &[instructions::InstructionOmission]) -> String {
    let mut out = String::from("\n## Unavailable instruction policy\n");
    if omissions.is_empty() {
        out.push_str("\nThe instruction index is incomplete.\n");
    }
    for omission in omissions {
        let path = omission
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(index)".into());
        out.push_str(&format!("\n- `{path}`: {}\n", omission.reason));
    }
    out
}
