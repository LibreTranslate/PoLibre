// SPDX-License-Identifier: AGPL-3.0-or-later
//! Word-level difference between two texts, for showing how a source string
//! changed since it was translated.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Same,
    Removed,
    Added,
}

/// Splits into words, runs of spaces and single punctuation marks, so that
/// joining the pieces gives back the text.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut kind = 0u8; // 1 word, 2 space
    for (i, c) in s.char_indices() {
        let k = if c.is_alphanumeric() || c == '_' {
            1
        } else if c.is_whitespace() {
            2
        } else {
            3
        };
        if i > start && (k != kind || k == 3) {
            out.push(&s[start..i]);
            start = i;
        }
        kind = k;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The pieces of `old` and `new` in order, each marked same, removed or added.
/// Neighbouring pieces of the same kind are merged.
pub fn word_diff(old: &str, new: &str) -> Vec<(Op, String)> {
    let a = tokens(old);
    let b = tokens(new);
    // Longest common subsequence table, filled from the end.
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out: Vec<(Op, String)> = Vec::new();
    let mut push = |op: Op, t: &str| match out.last_mut() {
        Some((o, s)) if *o == op => s.push_str(t),
        _ => out.push((op, t.to_string())),
    };
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            push(Op::Same, a[i]);
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Removed text first, then what replaced it.
            push(Op::Removed, a[i]);
            i += 1;
        } else {
            push(Op::Added, b[j]);
            j += 1;
        }
    }
    merge_changes(out)
}

/// Joins changes separated only by spaces into one removed and one added
/// piece, so "[-a][+b] [-c][+d]" reads as "[-a c][+b d]".
fn merge_changes(pieces: Vec<(Op, String)>) -> Vec<(Op, String)> {
    let mut out: Vec<(Op, String)> = Vec::new();
    let (mut removed, mut added) = (String::new(), String::new());
    let mut gap = String::new(); // spaces seen since the last change
    let flush = |out: &mut Vec<(Op, String)>, removed: &mut String, added: &mut String| {
        if !removed.is_empty() {
            out.push((Op::Removed, std::mem::take(removed)));
        }
        if !added.is_empty() {
            out.push((Op::Added, std::mem::take(added)));
        }
    };
    for (op, t) in pieces {
        match op {
            Op::Same if t.trim().is_empty() && !(removed.is_empty() && added.is_empty()) => {
                gap.push_str(&t)
            }
            Op::Same => {
                flush(&mut out, &mut removed, &mut added);
                out.push((Op::Same, std::mem::take(&mut gap) + &t));
            }
            Op::Removed | Op::Added => {
                if !gap.is_empty() {
                    // Spaces between two changes belong to both sides.
                    if !removed.is_empty() {
                        removed.push_str(&gap);
                    }
                    if !added.is_empty() {
                        added.push_str(&gap);
                    }
                    gap.clear();
                }
                if op == Op::Removed {
                    &mut removed
                } else {
                    &mut added
                }
                .push_str(&t);
            }
        }
    }
    flush(&mut out, &mut removed, &mut added);
    if !gap.is_empty() {
        out.push((Op::Same, gap));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(d: &[(Op, String)]) -> String {
        d.iter()
            .map(|(op, t)| match op {
                Op::Same => t.clone(),
                Op::Removed => format!("[-{}]", t),
                Op::Added => format!("[+{}]", t),
            })
            .collect()
    }

    #[test]
    fn marks_changed_words() {
        assert_eq!(
            show(&word_diff(
                "Plugin added successfully",
                "%(plugin)s installed successfully"
            )),
            "[-Plugin added][+%(plugin)s installed] successfully"
        );
        assert_eq!(
            show(&word_diff("Invalid boundaries", "Invalid payload.")),
            "Invalid [-boundaries][+payload.]"
        );
        assert_eq!(show(&word_diff("Same text", "Same text")), "Same text");
        assert_eq!(show(&word_diff("", "New")), "[+New]");
        assert_eq!(
            show(&word_diff("Cannot create new task. Please try again later.", "The plugin repository returned an invalid response. Please try again later.")),
            "[-Cannot create new task][+The plugin repository returned an invalid response]. Please try again later."
        );
        assert_eq!(
            show(&word_diff(
                "Invalid response: %(error)s",
                "Invalid CRS: %(line)s"
            )),
            "Invalid [-response][+CRS]: %([-error][+line])s"
        );
    }

    #[test]
    fn pieces_rebuild_both_texts() {
        let (old, new) = (
            "Shows the amount of green vegetation.",
            "Shows how much green there is, roughly.",
        );
        let d = word_diff(old, new);
        let side = |keep: Op| {
            d.iter()
                .filter(|(o, _)| *o == Op::Same || *o == keep)
                .map(|(_, t)| t.as_str())
                .collect::<String>()
        };
        assert_eq!(side(Op::Removed), old);
        assert_eq!(side(Op::Added), new);
    }
}
