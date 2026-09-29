//! Files the model has read that changed on disk by something other than
//! its own `edit` or `write` -- a formatter it ran, a background job, the
//! person's editor.
//!
//! **The model's view follows the file instead of going stale.** Without
//! this a formatter run over a file the model had read made every later
//! `edit` of it a stale-version refusal, and the only way back was a fresh
//! `context`: a full re-read, all of it uncached, of a file whose meaning
//! had not changed. Instead every changed line is delivered once, with the
//! cell whose command changed it, and the version the model edits against
//! moves to the new bytes -- the conversation only grows by the change.

use std::path::Path;
use std::rc::Rc;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// The most changed lines one file's change is shown in; past it the diff
/// is not computed and the model is told to read what it needs.
pub(crate) const MAX_CHANGES: usize = 200;

/// The bytes of one version of a file the model has a view of, and what a
/// cheap look at the file compares against before reading it again.
#[derive(Debug, Clone)]
pub(crate) struct Known {
    pub(crate) sha256: String,
    len: u64,
    modified: Option<SystemTime>,
    pub(crate) text: Rc<str>,
}

impl Known {
    /// The file as it is now, or `None` when it cannot be read as text.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        Some(Self {
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            text: text.into(),
        })
    }

    /// Whether the file still has the size and modification time it had
    /// when this was read, in which case it is not read again.
    pub(crate) fn looks_unchanged(&self, path: &Path) -> bool {
        std::fs::metadata(path)
            .is_ok_and(|now| now.len() == self.len && now.modified().ok() == self.modified)
    }
}

/// One step of a line-level edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    /// Old line, new line: the same text.
    Equal(usize, usize),
    /// An old line the change removed.
    Delete(usize),
    /// A new line the change added.
    Insert(usize),
}

/// The shortest line-level edit script from `old` to `new`, or `None` when
/// it is longer than `max_changes`.
///
/// The common head and tail are matched first, so a change confined to one
/// region costs the region, and the middle is Myers' greedy diff bounded by
/// `max_changes`: a rewrite of the whole file is answered quickly with
/// `None` rather than slowly with a diff nobody will read.
pub(crate) fn line_script(old: &[&str], new: &[&str], max_changes: usize) -> Option<Vec<Op>> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
    let middle = myers(&old[prefix..old_end], &new[prefix..new_end], max_changes)?;
    let mut ops = Vec::with_capacity(old.len().max(new.len()));
    ops.extend((0..prefix).map(|i| Op::Equal(i, i)));
    ops.extend(middle.into_iter().map(|op| match op {
        Op::Equal(x, y) => Op::Equal(x + prefix, y + prefix),
        Op::Delete(x) => Op::Delete(x + prefix),
        Op::Insert(y) => Op::Insert(y + prefix),
    }));
    ops.extend((0..suffix).map(|i| Op::Equal(old_end + i, new_end + i)));
    Some(ops)
}

fn myers(a: &[&str], b: &[&str], max_changes: usize) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = max_changes as isize;
    let offset = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let i = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
                v[i + 1]
            } else {
                v[i - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                return Some(backtrack(n, m, &trace, d, offset));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(n: isize, m: isize, trace: &[Vec<isize>], last: isize, offset: isize) -> Vec<Op> {
    let (mut x, mut y) = (n, m);
    let mut ops = Vec::new();
    for d in (0..=last).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let i = (k + offset) as usize;
        let previous_k = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = v[(previous_k + offset) as usize];
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Equal(x as usize, y as usize));
        }
        if d > 0 {
            if x == previous_x {
                ops.push(Op::Insert(previous_y as usize));
            } else {
                ops.push(Op::Delete(previous_x as usize));
            }
        }
        x = previous_x;
        y = previous_y;
    }
    ops.reverse();
    ops
}

/// One file's change as the model is shown it and as the runtime follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Delta {
    /// For each line of the old version (0-based), its line in the new
    /// version, or `None` for a line the change removed.
    pub(crate) map: Vec<Option<usize>>,
    /// The new version's lines the change added, 0-based, with their text:
    /// the model is shown them, so they are lines it has seen.
    pub(crate) added: Vec<(usize, String)>,
    /// Every changed line, removed ones before added ones in each run, each
    /// with its own 1-based line number.
    pub(crate) rendered: String,
    /// How many lines were removed or added.
    pub(crate) changed: usize,
}

/// The change from `old` to `new`, or `None` when more than `max_changes`
/// lines changed.
pub(crate) fn delta(old: &str, new: &str, max_changes: usize) -> Option<Delta> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = line_script(&a, &b, max_changes)?;
    let mut out = Delta {
        map: vec![None; a.len()],
        added: Vec::new(),
        rendered: String::new(),
        changed: 0,
    };
    let (mut removed, mut inserted) = (Vec::new(), Vec::new());
    let flush = |out: &mut Delta, removed: &mut Vec<usize>, inserted: &mut Vec<usize>| {
        for &x in removed.iter() {
            out.rendered
                .push_str(&format!("-{:>5} | {}\n", x + 1, a[x]));
        }
        for &y in inserted.iter() {
            out.rendered
                .push_str(&format!("+{:>5} | {}\n", y + 1, b[y]));
            out.added.push((y, b[y].to_string()));
        }
        out.changed += removed.len() + inserted.len();
        removed.clear();
        inserted.clear();
    };
    for op in ops {
        match op {
            Op::Equal(x, y) => {
                flush(&mut out, &mut removed, &mut inserted);
                out.map[x] = Some(y);
            }
            Op::Delete(x) => removed.push(x),
            Op::Insert(y) => inserted.push(y),
        }
    }
    flush(&mut out, &mut removed, &mut inserted);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &[&str], new: &[&str], ops: &[Op]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match *op {
                Op::Equal(x, _) => Some(old[x].to_string()),
                Op::Insert(y) => Some(new[y].to_string()),
                Op::Delete(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_script_rebuilds_the_new_version_and_is_minimal() {
        let old = ["a", "b", "c", "d", "e", "f", "g"];
        let new = ["a", "B", "c", "d", "x", "e", "f"];
        let ops = line_script(&old, &new, 50).unwrap();
        assert_eq!(apply(&old, &new, &ops), new);
        let changes = ops.iter().filter(|op| !matches!(op, Op::Equal(..))).count();
        assert_eq!(changes, 4, "{ops:?}");
    }

    #[test]
    fn scattered_changes_stay_scattered_and_lines_keep_their_place() {
        let old: Vec<String> = (0..300).map(|i| format!("line {i}")).collect();
        let mut new = old.clone();
        new[10] = "changed ten".to_string();
        new.insert(200, "inserted".to_string());
        new.remove(290);
        let (a, b): (Vec<&str>, Vec<&str>) = (
            old.iter().map(String::as_str).collect(),
            new.iter().map(String::as_str).collect(),
        );
        let d = delta(&a.join("\n"), &b.join("\n"), 50).unwrap();
        assert_eq!(d.changed, 4, "{}", d.rendered);
        assert!(
            d.rendered
                .contains("-   11 | line 10\n+   11 | changed ten\n")
        );
        assert!(d.rendered.contains("+  201 | inserted\n"));
        assert_eq!(d.map[5], Some(5));
        assert_eq!(d.map[199], Some(199));
        assert_eq!(d.map[200], Some(201));
        assert_eq!(d.map[289], None);
        assert_eq!(d.map[299], Some(299));
        assert_eq!(
            d.added,
            vec![
                (10, "changed ten".to_string()),
                (200, "inserted".to_string())
            ]
        );
    }

    #[test]
    fn a_rewrite_past_the_bound_is_not_diffed() {
        let old: Vec<String> = (0..100).map(|i| format!("a{i}")).collect();
        let new: Vec<String> = (0..100).map(|i| format!("b{i}")).collect();
        let (a, b) = (old.join("\n"), new.join("\n"));
        assert!(delta(&a, &b, 50).is_none());
        assert!(delta(&a, &b, 200).is_some());
    }
}
