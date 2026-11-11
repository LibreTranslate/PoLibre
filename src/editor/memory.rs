// SPDX-License-Identifier: AGPL-3.0-or-later
//! Translation memory: reviewed translations already in a language's files,
//! looked up by exact or similar source text.

use crate::po::PoFile;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Sources sharing fewer words than this (Dice coefficient) aren't shown.
const MIN_SCORE: f32 = 0.6;

struct Unit {
    file: PathBuf,
    msgctxt: Option<String>,
    source: String,
    translation: String,
    words: Vec<String>,
}

/// A reviewed translation whose source is the same as, or close to, another.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub source: String,
    pub translation: String,
    /// File it comes from (its name only).
    pub file: String,
    /// 1.0 for the same text; otherwise the share of words in common.
    pub score: f32,
}

#[derive(Default)]
pub struct Memory {
    units: Vec<Unit>,
    /// Lower-cased word -> units containing it, to find candidates quickly.
    index: HashMap<String, Vec<usize>>,
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Dice coefficient of two word lists (as multisets).
fn similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut rest: Vec<&String> = b.iter().collect();
    let mut common = 0;
    for w in a {
        if let Some(i) = rest.iter().position(|x| *x == w) {
            rest.swap_remove(i);
            common += 1;
        }
    }
    2.0 * common as f32 / (a.len() + b.len()) as f32
}

impl Memory {
    /// Reads the reviewed, singular translations of the given files.
    pub fn build(files: &[PathBuf]) -> Memory {
        let mut m = Memory::default();
        for f in files {
            let Ok(po) = PoFile::load(f) else { continue };
            for e in po.entries() {
                if e.obsolete
                    || e.is_header()
                    || e.is_fuzzy()
                    || e.is_untranslated()
                    || e.msgid_plural.is_some()
                {
                    continue;
                }
                let i = m.units.len();
                let w = words(&e.msgid);
                for x in &w {
                    let list = m.index.entry(x.clone()).or_default();
                    if list.last() != Some(&i) {
                        list.push(i);
                    }
                }
                m.units.push(Unit {
                    file: f.clone(),
                    msgctxt: e.msgctxt.clone(),
                    source: e.msgid.clone(),
                    translation: e.msgstr[0].clone(),
                    words: w,
                });
            }
        }
        m
    }

    /// The best matches for `source`, leaving out the entry itself (same file,
    /// context and text). Identical translations are listed once.
    pub fn lookup(
        &self,
        source: &str,
        msgctxt: Option<&str>,
        file: &Path,
        max: usize,
    ) -> Vec<Match> {
        let w = words(source);
        let mut candidates: Vec<usize> = w
            .iter()
            .filter_map(|x| self.index.get(x))
            .flatten()
            .copied()
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        let mut out: Vec<Match> = Vec::new();
        for i in candidates {
            let u = &self.units[i];
            if u.file == file && u.source == source && u.msgctxt.as_deref() == msgctxt {
                continue;
            }
            let score = if u.source == source {
                1.0
            } else {
                similarity(&w, &u.words).min(0.99)
            };
            if score < MIN_SCORE
                || out
                    .iter()
                    .any(|m| m.translation == u.translation && m.source == u.source)
            {
                continue;
            }
            out.push(Match {
                source: u.source.clone(),
                translation: u.translation.clone(),
                file: u
                    .file
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default(),
                score,
            });
        }
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.source.len().cmp(&b.source.len()))
        });
        out.truncate(max);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("msgid \"\"\nmsgstr \"\"\n\n{}", body)).unwrap();
        p
    }

    #[test]
    fn finds_exact_and_similar_reviewed_translations() {
        let dir = std::env::temp_dir().join(format!("polibre-tm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = write(&dir, "django.po", "msgid \"Delete the project\"\nmsgstr \"Elimina il progetto\"\n\n#, fuzzy\nmsgid \"Delete the task\"\nmsgstr \"Elimina l'attività\"\n\nmsgid \"Save\"\nmsgstr \"\"\n");
        let b = write(&dir, "djangojs.po", "msgid \"Delete the project\"\nmsgstr \"Cancella il progetto\"\n\nmsgid \"Rename the project\"\nmsgstr \"Rinomina il progetto\"\n\nmsgid \"Upload images\"\nmsgstr \"Carica immagini\"\n");
        let m = Memory::build(&[a.clone(), b.clone()]);

        // The entry itself is left out; the same text in the other file is an exact match.
        let r = m.lookup("Delete the project", None, &a, 5);
        assert_eq!(
            r[0],
            Match {
                source: "Delete the project".into(),
                translation: "Cancella il progetto".into(),
                file: "djangojs.po".into(),
                score: 1.0
            }
        );
        // Similar: 2 of 3 words in common. Fuzzy and untranslated entries are never used.
        assert!(r
            .iter()
            .any(|x| x.source == "Rename the project" && (x.score - 0.67).abs() < 0.01));
        assert!(!r.iter().any(|x| x.source == "Delete the task"));
        // Nothing alike.
        assert!(m.lookup("Upload failed", None, &a, 5).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn similarity_counts_repeated_words_once_each() {
        let s = |a: &str, b: &str| similarity(&words(a), &words(b));
        assert_eq!(s("a b", "a b"), 1.0);
        assert_eq!(s("the the", "the"), 2.0 / 3.0);
        assert_eq!(s("x", ""), 0.0);
    }
}
