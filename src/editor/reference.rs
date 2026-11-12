// SPDX-License-Identifier: AGPL-3.0-or-later
//! Another language's translations of the same strings, shown for comparison.

use crate::po::PoFile;
use std::collections::HashMap;
use std::path::PathBuf;

/// One translation in the reference language.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub text: String,
    pub needs_review: bool,
}

pub struct Reference {
    pub code: String,
    pub name: String,
    by_key: HashMap<(Option<String>, String), Shown>,
}

impl Reference {
    /// Reads the language's files. A string found in several files is taken
    /// from the file named like `prefer` (e.g. both "django.po").
    pub fn load(
        code: &str,
        name: &str,
        files: &[PathBuf],
        prefer: Option<&std::ffi::OsStr>,
    ) -> Reference {
        let mut files = files.to_vec();
        files.sort_by_key(|f| f.file_name() != prefer);
        let mut by_key = HashMap::new();
        for f in &files {
            let Ok(po) = PoFile::load(f) else { continue };
            for e in po.entries() {
                if e.obsolete || e.is_header() || e.is_untranslated() {
                    continue;
                }
                let text = crate::engine::show_forms(&e.msgstr, "  ·  ");
                by_key
                    .entry((e.msgctxt.clone(), e.msgid.clone()))
                    .or_insert(Shown {
                        text,
                        needs_review: e.is_fuzzy(),
                    });
            }
        }
        Reference {
            code: code.to_string(),
            name: name.to_string(),
            by_key,
        }
    }

    pub fn get(&self, msgctxt: Option<&str>, msgid: &str) -> Option<&Shown> {
        self.by_key
            .get(&(msgctxt.map(String::from), msgid.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_by_context_and_text() {
        let dir = std::env::temp_dir().join(format!("polibre-ref-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        let head = "msgid \"\"\nmsgstr \"\"\n\n";
        let js = dir.join("a/djangojs.po");
        let py = dir.join("b/django.po");
        std::fs::write(
            &js,
            format!("{head}msgid \"Open\"\nmsgstr \"Ouvrir (js)\"\n"),
        )
        .unwrap();
        std::fs::write(&py, format!("{head}msgid \"Open\"\nmsgstr \"Ouvrir\"\n\nmsgctxt \"door\"\nmsgid \"Open\"\nmsgstr \"Ouvert\"\n\n#, fuzzy\nmsgid \"Close\"\nmsgstr \"Fermer\"\n\nmsgid \"Save\"\nmsgstr \"\"\n")).unwrap();
        let r = Reference::load(
            "fr",
            "French",
            &[js, py],
            Some(std::ffi::OsStr::new("django.po")),
        );
        assert_eq!(
            r.get(None, "Open").unwrap().text,
            "Ouvrir",
            "the same file name wins"
        );
        assert_eq!(r.get(Some("door"), "Open").unwrap().text, "Ouvert");
        assert!(r.get(None, "Close").unwrap().needs_review);
        assert!(
            r.get(None, "Save").is_none(),
            "untranslated strings aren't shown"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
