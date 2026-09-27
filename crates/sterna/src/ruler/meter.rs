//! Reading what the gateway counted, and honestly failing to.
//!
//! The producer is `inference-gateway routing-cost --json --since <UNIX>`:
//! JSON Lines, one object per observation, ascending by `observed_at`. An
//! empty window prints nothing and exits 0 -- that is a real, successful
//! zero-row read, not a failure. This process never links against the
//! gateway; it shells out to the binary.
//!
//! **Attribution is by time window alone, never by session.** The gateway
//! records no session id for these rows, so a `--session` filter would match
//! nothing and look identical to an unconfigured meter. Correct attribution
//! instead depends on [`super::attempt`]'s `ATTEMPT_LOCK`: only one
//! attempt's harness-launch-through-meter-read span may be open at a time, so
//! every row observed inside `[from, to]` belongs to the attempt that opened
//! that window.
//!
//! `read` runs `routing-cost` with the attempt's own worktree as its current
//! directory. The gateway reads its own data directory wherever it runs, so
//! the time window, not the directory, is what selects the attempt's rows.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::model::Tokens;

/// One observed exchange, as `inference-gateway routing-cost --json` writes it.
/// Only the columns this module needs are declared; every other key
/// (`session_id`, `harness`, `dispatched_at`, `completed_at`, `outcome`,
/// ...) is ignored by `serde_json` without any attribute here -- `outcome`
/// in particular is deliberately not read: a null-outcome row is still real
/// spend, so nothing here may filter on it.
#[derive(Debug, Deserialize)]
struct ExchangeRow {
    observed_at: i64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    #[serde(default)]
    cached_input_tokens: Option<u64>,
}

/// Where an attempt's meter figures come from.
///
/// `None` is a real variant, not a missing case: no readout was configured,
/// the `inference-gateway` binary could not be launched, or it exited non-zero.
/// Either way `read` returns [`Tokens::default`] and `turns: None`. That is
/// distinct from a meter that *ran* and found nothing in the window --
/// `turns: Some(0)` -- and the distinction is deliberate: collapsing them
/// is exactly the shape of bug this module already had once (see the module
/// doc comment).
#[derive(Debug, Clone)]
pub enum Meter {
    None,
    /// Reads by shelling out to this `inference-gateway` executable.
    Command {
        executable: PathBuf,
    },
}

impl Meter {
    /// The tokens and turn count observed between `from` and `to`,
    /// inclusive. Absent (not zero) when there is no meter, the binary could
    /// not be launched, or it exited non-zero.
    ///
    /// `routing-cost` takes only `--since`, not an upper bound, so this asks
    /// for everything from `from` onward and lets [`Readout::for_window`]
    /// apply the `to` bound locally.
    ///
    /// `dir` becomes the command's current directory.
    pub fn read(&self, dir: &Path, from: SystemTime, to: SystemTime) -> (Tokens, Option<u32>) {
        match self {
            Meter::None => (Tokens::default(), None),
            Meter::Command { executable } => {
                let output = Command::new(executable)
                    .arg("routing-cost")
                    .arg("--json")
                    .arg("--since")
                    .arg(unix_secs(from).to_string())
                    .current_dir(dir)
                    .output();
                match output {
                    Ok(out) if out.status.success() => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        Readout::for_window(stdout.lines(), from, to)
                    }
                    _ => (Tokens::default(), None),
                }
            }
        }
    }
}

/// Sums matching rows of a `routing-cost --json`-shaped JSONL readout.
pub struct Readout;

impl Readout {
    /// Sums the rows in `lines` whose `observed_at` falls within
    /// `[from, to]` into a [`Tokens`] and a turn count (the number of
    /// matching rows). There is no session filter -- see the module doc
    /// comment for why one would be actively wrong here.
    ///
    /// A row missing `input_tokens` or `output_tokens` contributes to
    /// neither running sum -- a null column is absent, never a zero that
    /// would corrupt a total built from other rows. A malformed line is
    /// skipped, not an error: the whole point of this reader is that a bad
    /// line must never be able to look like a spend of nothing.
    pub fn for_window<I, S>(lines: I, from: SystemTime, to: SystemTime) -> (Tokens, Option<u32>)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let from_secs = unix_secs(from);
        let to_secs = unix_secs(to);

        let mut input: Option<u64> = None;
        let mut output: Option<u64> = None;
        let mut cached_input: Option<u64> = None;
        let mut turns: u32 = 0;

        for line in lines {
            let line = line.as_ref().trim();
            if line.is_empty() {
                continue;
            }
            let Ok(row) = serde_json::from_str::<ExchangeRow>(line) else {
                continue;
            };
            if row.observed_at < from_secs || row.observed_at > to_secs {
                continue;
            }

            turns += 1;
            if let Some(v) = row.input_tokens {
                input = Some(input.unwrap_or(0) + v);
            }
            if let Some(v) = row.output_tokens {
                output = Some(output.unwrap_or(0) + v);
            }
            if let Some(v) = row.cached_input_tokens {
                cached_input = Some(cached_input.unwrap_or(0) + v);
            }
        }

        (
            Tokens {
                input,
                output,
                cached_input,
            },
            Some(turns),
        )
    }
}

fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the two `#[cfg(unix)]` tests below write a fake readout script,
    // so on Windows this import has no user and `warnings = deny` makes an
    // unused one a build failure rather than a lint.
    #[cfg(unix)]
    use std::fs;
    use std::time::Duration;

    fn secs(n: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(n)
    }

    #[test]
    fn sums_rows_inside_the_window_and_excludes_rows_outside_it() {
        let lines = [
            r#"{"observed_at":100,"input_tokens":10,"output_tokens":5,"cached_input_tokens":2}"#,
            r#"{"observed_at":200,"input_tokens":20,"output_tokens":8}"#,
            r#"{"observed_at":300,"input_tokens":999,"output_tokens":999}"#, // after `to`
            r#"{"observed_at":10,"input_tokens":999,"output_tokens":999}"#,  // before `from`
        ];
        let (tokens, turns) = Readout::for_window(lines, secs(50), secs(250));
        assert_eq!(tokens.total(), Some(10 + 5 + 2 + 20 + 8));
        assert_eq!(turns, Some(2));
    }

    #[test]
    fn no_matching_rows_is_a_real_zero_turn_count_not_none() {
        let lines: [&str; 0] = [];
        let (tokens, turns) = Readout::for_window(lines, secs(0), secs(200));
        assert_eq!(tokens.total(), None);
        assert_eq!(turns, Some(0));
    }

    #[test]
    fn a_null_token_column_is_not_a_zero() {
        let lines = [
            r#"{"observed_at":100,"input_tokens":40,"output_tokens":10}"#,
            r#"{"observed_at":110,"input_tokens":null,"output_tokens":null}"#,
        ];
        let (tokens, turns) = Readout::for_window(lines, secs(0), secs(200));
        assert_eq!(
            tokens.total(),
            Some(50),
            "the null row must not zero a sum built from real rows"
        );
        assert_eq!(turns, Some(2));
    }

    #[test]
    fn a_null_outcome_row_still_counts() {
        let lines = [r#"{"observed_at":100,"input_tokens":10,"output_tokens":5,"outcome":null}"#];
        let (tokens, turns) = Readout::for_window(lines, secs(0), secs(200));
        assert_eq!(tokens.total(), Some(15));
        assert_eq!(turns, Some(1));
    }

    #[test]
    fn no_meter_yields_absent_tokens_and_turns() {
        let (tokens, turns) = Meter::None.read(Path::new("."), secs(0), secs(200));
        assert_eq!(tokens.total(), None);
        assert_eq!(turns, None);
    }

    #[test]
    fn a_command_that_cannot_launch_yields_absent_tokens_and_turns() {
        let meter = Meter::Command {
            executable: PathBuf::from("/definitely/does/not/exist/inference-gateway"),
        };
        let (tokens, turns) = meter.read(Path::new("."), secs(0), secs(200));
        assert_eq!(tokens.total(), None);
        assert_eq!(turns, None);
    }

    /// Unix only: the fake readout is a shell script. The Windows sterna
    /// cell runs every other test in this module.
    #[cfg(unix)]
    #[test]
    fn a_command_that_prints_a_row_in_window_is_summed() {
        let dir =
            std::env::temp_dir().join(format!("sterna-ruler-meter-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fake_gateway = dir.join("fake_gateway.sh");
        fs::write(
            &fake_gateway,
            "#!/bin/sh\necho '{\"observed_at\":100,\"input_tokens\":7,\"output_tokens\":3}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_gateway).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_gateway, perms).unwrap();

        let meter = Meter::Command {
            executable: fake_gateway,
        };
        let (tokens, turns) = meter.read(&dir, secs(0), secs(200));
        assert_eq!(tokens.total(), Some(10));
        assert_eq!(turns, Some(1));
    }

    /// The zero-row case: a meter that *is* configured, launches fine, and
    /// truthfully reports nothing happened in the window. This must read as
    /// `turns: Some(0)`, never `None` -- `None` means "no meter", and
    /// collapsing the two is the exact defect the module doc comment
    /// describes.
    /// Unix only: the fake readout is a shell script. The Windows sterna
    /// cell runs every other test in this module.
    #[cfg(unix)]
    #[test]
    fn an_empty_successful_readout_is_zero_turns_not_absent() {
        let dir =
            std::env::temp_dir().join(format!("sterna-ruler-meter-empty-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fake_gateway = dir.join("fake_gateway.sh");
        fs::write(&fake_gateway, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_gateway).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_gateway, perms).unwrap();

        let meter = Meter::Command {
            executable: fake_gateway,
        };
        let (tokens, turns) = meter.read(&dir, secs(0), secs(200));
        assert_eq!(tokens.total(), None);
        assert_eq!(
            turns,
            Some(0),
            "a meter that ran and found nothing is zero turns, not an absent meter"
        );
    }
}
