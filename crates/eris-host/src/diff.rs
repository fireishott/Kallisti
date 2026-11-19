//! Line diffs in the shape a phone renders.
//!
//! One file, one hunk list, and every line tagged `context`, `add` or `remove`.
//! The contract calls this `turn.artifact.diff`; the daemon builds it here so a
//! surface never has to diff two strings to draw a change.
//!
//! The algorithm is a longest-common-subsequence walk over lines, which is more
//! than a summary needs and exactly what hunks need: a count can be computed
//! from set arithmetic, but the lines themselves cannot.

use serde_json::{Value, json};

/// How many unchanged lines are shown around a change.
const CONTEXT: usize = 3;

/// One file's diff, as the contract states it.
#[must_use]
pub fn file(path: &str, before: &str, after: &str) -> Value {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let script = script(&old, &new);

    let mut added = 0usize;
    let mut removed = 0usize;
    for (kind, _) in &script {
        match kind {
            Kind::Add => added += 1,
            Kind::Remove => removed += 1,
            Kind::Context => {}
        }
    }

    json!({
        "path": path,
        "added": added,
        "removed": removed,
        "hunks": hunks(&script),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Context,
    Add,
    Remove,
}

/// The per-line script: what happened to each line, in order.
fn script(old: &[&str], new: &[&str]) -> Vec<(Kind, String)> {
    // LCS table, then walk it back into edits. The table is O(n*m) in memory,
    // which is fine for the file sizes this daemon edits and is stated here
    // rather than discovered later on a large file.
    let mut lengths = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lengths[i][j] = if old[i] == new[j] {
                lengths[i + 1][j + 1] + 1
            } else {
                lengths[i + 1][j].max(lengths[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            out.push((Kind::Context, old[i].to_owned()));
            i += 1;
            j += 1;
        } else if lengths[i + 1][j] >= lengths[i][j + 1] {
            out.push((Kind::Remove, old[i].to_owned()));
            i += 1;
        } else {
            out.push((Kind::Add, new[j].to_owned()));
            j += 1;
        }
    }
    while i < old.len() {
        out.push((Kind::Remove, old[i].to_owned()));
        i += 1;
    }
    while j < new.len() {
        out.push((Kind::Add, new[j].to_owned()));
        j += 1;
    }
    out
}

/// Group the script into hunks, keeping [`CONTEXT`] unchanged lines around a
/// change and folding everything further away.
fn hunks(script: &[(Kind, String)]) -> Vec<Value> {
    let changed: Vec<usize> = script
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != Kind::Context)
        .map(|(index, _)| index)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for index in changed {
        let start = index.saturating_sub(CONTEXT);
        let end = (index + CONTEXT + 1).min(script.len());
        match ranges.last_mut() {
            Some((_, last_end)) if start <= *last_end => *last_end = (*last_end).max(end),
            _ => ranges.push((start, end)),
        }
    }

    ranges
        .into_iter()
        .map(|(start, end)| {
            // Where in each file this hunk begins, counted from the lines the
            // script has already accounted for.
            let old_before = script[..start]
                .iter()
                .filter(|(kind, _)| *kind != Kind::Add)
                .count();
            let new_before = script[..start]
                .iter()
                .filter(|(kind, _)| *kind != Kind::Remove)
                .count();
            let slice = &script[start..end];
            let old_lines = slice.iter().filter(|(kind, _)| *kind != Kind::Add).count();
            let new_lines = slice
                .iter()
                .filter(|(kind, _)| *kind != Kind::Remove)
                .count();
            let lines: Vec<Value> = slice
                .iter()
                .map(|(kind, text)| {
                    json!({
                        "kind": match kind {
                            Kind::Context => "context",
                            Kind::Add => "add",
                            Kind::Remove => "remove",
                        },
                        "text": text,
                    })
                })
                .collect();
            json!({
                "old_start": old_before + 1,
                "old_lines": old_lines,
                "new_start": new_before + 1,
                "new_lines": new_lines,
                "lines": lines,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::file;

    #[test]
    fn a_changed_line_is_one_hunk_with_both_sides_and_context() {
        let before = "one\ntwo\nthree\nfour\nfive\n";
        let after = "one\ntwo\nTHREE\nfour\nfive\n";
        let diff = file("notes.txt", before, after);
        assert_eq!(diff["path"], "notes.txt");
        assert_eq!(diff["added"], 1);
        assert_eq!(diff["removed"], 1);
        let hunks = diff["hunks"].as_array().expect("hunks");
        assert_eq!(hunks.len(), 1);
        let lines = hunks[0]["lines"].as_array().expect("lines");
        let kinds: Vec<&str> = lines
            .iter()
            .map(|line| line["kind"].as_str().unwrap_or_default())
            .collect();
        assert!(kinds.contains(&"context"));
        assert!(kinds.contains(&"add"));
        assert!(kinds.contains(&"remove"));
        let added = lines
            .iter()
            .find(|line| line["kind"] == "add")
            .expect("an added line");
        assert_eq!(added["text"], "THREE");
    }

    #[test]
    fn an_unchanged_file_has_no_hunks_and_a_new_file_is_all_additions() {
        let same = file("a.txt", "x\n", "x\n");
        assert_eq!(same["added"], 0);
        assert_eq!(same["removed"], 0);
        assert!(same["hunks"].as_array().expect("hunks").is_empty());

        let created = file("a.txt", "", "x\ny\n");
        assert_eq!(created["added"], 2);
        assert_eq!(created["removed"], 0);
    }

    /// Two changes far apart are two hunks, not one that spans the file.
    #[test]
    fn distant_changes_are_separate_hunks() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let after = "A\nb\nc\nd\ne\nf\ng\nh\ni\nJ\n";
        let diff = file("x.txt", before, after);
        assert_eq!(diff["hunks"].as_array().expect("hunks").len(), 2);
        assert_eq!(diff["added"], 2);
        assert_eq!(diff["removed"], 2);
    }
}
