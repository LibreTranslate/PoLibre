// PoLibre - fill untranslated gettext .po strings automatically.
// Copyright (C) 2026 PoLibre contributors
// SPDX-License-Identifier: AGPL-3.0-or-later
//
//! Minimal, format-preserving gettext PO parser/writer.
//!
//! Entries that are not modified are written back byte-for-byte, so running
//! PoLibre produces the smallest possible diff.

use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// Raw comment lines (`#`, `#.`, `#:`, `#,`, `#|`) in original order.
    pub comment_lines: Vec<String>,
    /// Raw msgctxt / msgid / msgid_plural lines.
    pub key_lines: Vec<String>,
    /// Raw msgstr lines.
    pub msgstr_lines: Vec<String>,

    pub flags: Vec<String>,
    pub references: Vec<String>,
    pub extracted_comments: Vec<String>,
    pub msgctxt: Option<String>,
    pub msgid: String,
    pub msgid_plural: Option<String>,
    /// One element for singular entries, `nplurals` elements for plural ones.
    pub msgstr: Vec<String>,
    pub obsolete: bool,

    /// Set when this entry must be re-rendered.
    pub modified: bool,
    /// Translator comment to add when re-rendering.
    pub new_comment: Option<String>,
}

impl Entry {
    pub fn is_header(&self) -> bool {
        self.msgid.is_empty() && self.msgctxt.is_none() && !self.obsolete
    }
    pub fn is_fuzzy(&self) -> bool {
        self.flags.iter().any(|f| f == "fuzzy")
    }
    pub fn is_untranslated(&self) -> bool {
        self.msgstr.iter().all(|s| s.is_empty())
    }
    /// Should this entry be sent for translation?
    pub fn needs_translation(&self, include_fuzzy: bool) -> bool {
        if self.obsolete || self.is_header() || self.msgid.trim().is_empty() {
            return false;
        }
        self.is_untranslated() || (include_fuzzy && self.is_fuzzy())
    }

    /// Fill this entry with a machine translation, optionally mark it fuzzy
    /// (needs review) and attach a translator comment.
    pub fn set_translation(&mut self, forms: Vec<String>, comment: Option<String>, fuzzy: bool) {
        self.msgstr = forms;
        if fuzzy {
            if !self.is_fuzzy() {
                self.flags.insert(0, "fuzzy".into());
            }
        } else {
            // A final translation: drop an old fuzzy flag and its #| lines.
            self.flags.retain(|f| f != "fuzzy");
            self.comment_lines.retain(|l| !l.starts_with("#|"));
        }
        self.new_comment = comment;
        self.modified = true;
    }

    /// Whether PoLibre filled this entry (it carries the machine-translation note).
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn auto_note(&self) -> Option<&str> {
        self.comment_lines
            .iter()
            .find(|l| crate::is_auto_comment(l))
            .map(|l| l.trim_start_matches('#').trim())
    }

    /// The source text before it last changed, from gettext's `#| msgid`
    /// lines (written by msgmerge when it marks an entry fuzzy).
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn previous_msgid(&self) -> Option<String> {
        let mut out: Option<String> = None;
        let mut in_msgid = false;
        for l in &self.comment_lines {
            let Some(rest) = l.strip_prefix("#|") else {
                continue;
            };
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix("msgid ") {
                out = Some(unquote(v));
                in_msgid = true;
            } else if rest.starts_with('"') && in_msgid {
                if let Some(o) = out.as_mut() {
                    o.push_str(&unquote(rest));
                }
            } else {
                in_msgid = false; // #| msgctxt, #| msgid_plural
            }
        }
        out
    }

    /// The translator comments (`# …` lines), one per line, without
    /// PoLibre's own "Translated by …" note.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn translator_comments(&self) -> String {
        self.comment_lines
            .iter()
            .take_while(|l| is_translator_comment(l))
            .filter(|l| !crate::is_auto_comment(l))
            .map(|l| l.strip_prefix("# ").unwrap_or(l.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Replaces the translator comments; PoLibre's note is kept after them.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn set_translator_comments(&mut self, text: &str) {
        let n = self
            .comment_lines
            .iter()
            .take_while(|l| is_translator_comment(l))
            .count();
        let notes: Vec<String> = self.comment_lines[..n]
            .iter()
            .filter(|l| crate::is_auto_comment(l))
            .cloned()
            .collect();
        let mut lines: Vec<String> = text
            .trim_end()
            .lines()
            .map(|l| {
                if l.trim().is_empty() {
                    "#".to_string()
                } else {
                    format!("# {}", l.trim_end())
                }
            })
            .collect();
        if text.trim().is_empty() {
            lines.clear();
        }
        lines.extend(notes);
        self.comment_lines.splice(..n, lines);
        self.modified = true;
    }

    /// Replace PoLibre's "Translated by …" note (none removes it).
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn set_auto_note(&mut self, note: Option<String>) {
        self.comment_lines.retain(|l| !crate::is_auto_comment(l));
        if let Some(n) = note {
            // After the other translator comments, as when rendering.
            let at = self
                .comment_lines
                .iter()
                .take_while(|l| is_translator_comment(l))
                .count();
            self.comment_lines.insert(at, format!("# {}", n));
        }
        self.new_comment = None;
        self.modified = true;
    }

    /// Replace the translation with text typed by a person.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn set_manual(&mut self, forms: Vec<String>) {
        self.msgstr = forms;
        self.modified = true;
    }

    /// Mark or unmark the entry as needing review. Clearing it also removes the
    /// machine-translation note and the previous-msgid (`#|`) lines, like gettext
    /// editors do when a translation is approved.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn set_needs_review(&mut self, on: bool) {
        if on {
            if !self.is_fuzzy() {
                self.flags.insert(0, "fuzzy".into());
            }
        } else {
            self.flags.retain(|f| f != "fuzzy");
            self.comment_lines
                .retain(|l| !crate::is_auto_comment(l) && !l.starts_with("#|"));
            self.new_comment = None;
        }
        self.modified = true;
    }

    fn render(&self, width: usize, out: &mut Vec<String>) {
        if !self.modified {
            out.extend(self.comment_lines.iter().cloned());
            out.extend(self.key_lines.iter().cloned());
            out.extend(self.msgstr_lines.iter().cloned());
            return;
        }
        let flags_line = if self.flags.is_empty() {
            None
        } else {
            Some(format!("#, {}", self.flags.join(", ")))
        };
        let mut comments: Vec<String> = Vec::new();
        let mut flags_done = false;
        // Translator comments ("# ...") come first per the PO spec.
        let n_translator = self
            .comment_lines
            .iter()
            .take_while(|l| is_translator_comment(l))
            .count();
        for l in &self.comment_lines[..n_translator] {
            // Replace a previous machine-translation note instead of stacking them.
            if self.new_comment.is_some() && crate::is_auto_comment(l) {
                continue;
            }
            comments.push(l.clone());
        }
        if let Some(c) = &self.new_comment {
            comments.push(format!("# {}", c));
        }
        for l in &self.comment_lines[n_translator..] {
            if l.starts_with("#,") {
                if !flags_done {
                    if let Some(f) = &flags_line {
                        comments.push(f.clone());
                    }
                    flags_done = true;
                }
                continue;
            }
            if l.starts_with("#|") && !flags_done {
                if let Some(f) = &flags_line {
                    comments.push(f.clone());
                }
                flags_done = true;
            }
            comments.push(l.clone());
        }
        if !flags_done {
            if let Some(f) = &flags_line {
                comments.push(f.clone());
            }
        }
        out.extend(comments);
        out.extend(self.key_lines.iter().cloned());
        if self.msgid_plural.is_some() {
            for (i, s) in self.msgstr.iter().enumerate() {
                out.extend(render_field(&format!("msgstr[{}]", i), s, width));
            }
        } else {
            out.extend(render_field(
                "msgstr",
                self.msgstr.first().map(|s| s.as_str()).unwrap_or(""),
                width,
            ));
        }
    }
}

fn is_translator_comment(l: &str) -> bool {
    l == "#"
        || (l.starts_with("# "))
        || (l.starts_with('#') && !matches!(l.chars().nth(1), Some('.' | ':' | ',' | '|' | '~')))
}

#[derive(Debug, Clone)]
pub enum Item {
    Entry(Entry),
    Raw(String),
}

#[derive(Debug, Clone)]
pub struct PoFile {
    pub items: Vec<Item>,
    pub trailing_newline: bool,
    pub wrap_width: usize,
}

impl PoFile {
    /// Marks one entry (found by context and source text) as reviewed in the
    /// file on disk. Returns false if the entry isn't there.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn approve_in_file(path: &Path, msgctxt: Option<&str>, msgid: &str) -> io::Result<bool> {
        let mut po = Self::load(path)?;
        let Some(e) = po
            .entries_mut()
            .find(|e| !e.obsolete && e.msgid == msgid && e.msgctxt.as_deref() == msgctxt)
        else {
            return Ok(false);
        };
        if e.is_fuzzy() {
            e.set_needs_review(false);
            po.save(path)?;
        }
        Ok(true)
    }

    pub fn load(path: &Path) -> io::Result<PoFile> {
        let text = fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.items.iter().filter_map(|i| match i {
            Item::Entry(e) => Some(e),
            _ => None,
        })
    }
    #[allow(dead_code)]
    pub fn entries_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.items.iter_mut().filter_map(|i| match i {
            Item::Entry(e) => Some(e),
            _ => None,
        })
    }

    pub fn header(&self) -> Option<&Entry> {
        self.entries().find(|e| e.is_header())
    }

    pub fn header_field(&self, name: &str) -> Option<String> {
        let h = self.header()?;
        let text = h.msgstr.first()?;
        for line in text.lines() {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case(name) {
                    return Some(v.trim().to_string());
                }
            }
        }
        None
    }

    /// Returns (nplurals, plural expression).
    pub fn plural_forms(&self) -> (usize, String) {
        if let Some(pf) = self.header_field("Plural-Forms") {
            let mut n = 2;
            let mut expr = String::new();
            for part in pf.split(';') {
                let part = part.trim();
                if let Some(v) = part.strip_prefix("nplurals=") {
                    n = v.trim().parse().unwrap_or(2);
                } else if let Some(v) = part.strip_prefix("plural=") {
                    expr = v.trim().to_string();
                }
            }
            return (n.max(1), expr);
        }
        (2, "n != 1".into())
    }

    pub fn parse(text: &str) -> PoFile {
        let mut items = Vec::new();
        let mut cur: Option<Entry> = None;
        // which field the next continuation line belongs to
        #[derive(Clone, Copy, PartialEq)]
        enum F {
            None,
            Ctxt,
            Id,
            IdPlural,
            Str(usize),
            Prev,
        }
        let mut field = F::None;
        let mut max_len = 0usize;

        fn flush(items: &mut Vec<Item>, cur: &mut Option<Entry>) {
            if let Some(e) = cur.take() {
                items.push(Item::Entry(e));
            }
        }

        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            let trimmed = line.trim();
            if line.starts_with('"') {
                max_len = max_len.max(line.chars().count());
            }
            if trimmed.is_empty() {
                flush(&mut items, &mut cur);
                items.push(Item::Raw(line.to_string()));
                field = F::None;
                continue;
            }
            // Obsolete entries: keep raw, mark obsolete.
            let (obsolete, body) = if let Some(rest) = line.strip_prefix("#~") {
                (true, rest.trim_start())
            } else {
                (false, line)
            };
            let is_comment = !obsolete && line.starts_with('#');
            let has_msg = cur
                .as_ref()
                .map(|e| !e.msgstr_lines.is_empty())
                .unwrap_or(false);

            if is_comment {
                if has_msg {
                    flush(&mut items, &mut cur);
                }
                let e = cur.get_or_insert_with(Entry::default);
                e.comment_lines.push(line.to_string());
                if let Some(f) = line.strip_prefix("#,") {
                    e.flags.extend(
                        f.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                    );
                } else if let Some(r) = line.strip_prefix("#:") {
                    e.references
                        .extend(r.split_whitespace().map(|s| s.to_string()));
                } else if let Some(c) = line.strip_prefix("#.") {
                    e.extracted_comments.push(c.trim().to_string());
                } else if line.starts_with("#|") {
                    field = F::Prev;
                }
                continue;
            }

            let starts_key = body.starts_with("msgctxt") || body.starts_with("msgid");
            if starts_key && has_msg {
                flush(&mut items, &mut cur);
            }
            let e = cur.get_or_insert_with(Entry::default);
            if obsolete {
                e.obsolete = true;
            }

            let (kw, rest) = match body.find('"') {
                Some(i) => (body[..i].trim(), &body[i..]),
                None => (body.trim(), ""),
            };
            let val = unquote(rest);
            match kw {
                "msgctxt" => {
                    field = F::Ctxt;
                    e.msgctxt = Some(val);
                    e.key_lines.push(line.to_string());
                }
                "msgid" => {
                    field = F::Id;
                    e.msgid = val;
                    e.key_lines.push(line.to_string());
                }
                "msgid_plural" => {
                    field = F::IdPlural;
                    e.msgid_plural = Some(val);
                    e.key_lines.push(line.to_string());
                }
                k if k.starts_with("msgstr") => {
                    let idx = k
                        .strip_prefix("msgstr[")
                        .and_then(|s| s.strip_suffix(']'))
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(0);
                    while e.msgstr.len() <= idx {
                        e.msgstr.push(String::new());
                    }
                    e.msgstr[idx] = val;
                    field = F::Str(idx);
                    e.msgstr_lines.push(line.to_string());
                }
                "" => {
                    // continuation line
                    match field {
                        F::Ctxt => {
                            e.msgctxt.get_or_insert_with(String::new).push_str(&val);
                            e.key_lines.push(line.to_string());
                        }
                        F::Id => {
                            e.msgid.push_str(&val);
                            e.key_lines.push(line.to_string());
                        }
                        F::IdPlural => {
                            e.msgid_plural
                                .get_or_insert_with(String::new)
                                .push_str(&val);
                            e.key_lines.push(line.to_string());
                        }
                        F::Str(i) => {
                            e.msgstr[i].push_str(&val);
                            e.msgstr_lines.push(line.to_string());
                        }
                        F::Prev | F::None => {
                            e.comment_lines.push(line.to_string());
                        }
                    }
                }
                _ => {
                    // Unknown line; keep it verbatim attached to the entry.
                    e.comment_lines.push(line.to_string());
                }
            }
        }
        flush(&mut items, &mut cur);
        // gettext wraps at 79 columns; if the file has longer string lines it
        // was probably written without wrapping (or with a larger width).
        let wrap_width = if max_len > 120 { usize::MAX } else { 79 };
        PoFile {
            items,
            trailing_newline: text.ends_with('\n'),
            wrap_width,
        }
    }

    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        for it in &self.items {
            match it {
                Item::Raw(s) => lines.push(s.clone()),
                Item::Entry(e) => e.render(self.wrap_width, &mut lines),
            }
        }
        let mut s = lines.join("\n");
        if self.trailing_newline {
            s.push('\n');
        }
        s
    }

    /// Atomically write the file (temp file + rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("po.polibre.tmp");
        fs::write(&tmp, self.render())?;
        fs::rename(&tmp, path)
    }
}

pub fn unquote(s: &str) -> String {
    let s = s.trim();
    let inner = s.strip_prefix('"').unwrap_or(s);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    unescape(inner)
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('a') => out.push('\x07'),
                Some('b') => out.push('\x08'),
                Some('f') => out.push('\x0c'),
                Some('v') => out.push('\x0b'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Render `keyword "value"` the way GNU gettext does (wrap at `width`).
pub fn render_field(keyword: &str, value: &str, width: usize) -> Vec<String> {
    let esc = escape(value);
    let internal_nl = value.trim_end_matches('\n').contains('\n');
    if !internal_nl && keyword.chars().count() + esc.chars().count() + 3 <= width {
        return vec![format!("{} \"{}\"", keyword, esc)];
    }
    let mut lines = vec![format!("{} \"\"", keyword)];
    // Split after each escaped newline.
    let mut segments: Vec<String> = Vec::new();
    let mut rest = esc.as_str();
    while let Some(i) = rest.find("\\n") {
        segments.push(rest[..i + 2].to_string());
        rest = &rest[i + 2..];
    }
    if !rest.is_empty() {
        segments.push(rest.to_string());
    }
    let max = width.saturating_sub(2).max(10);
    for seg in segments {
        // break points: after spaces
        let mut cur = String::new();
        let mut word = String::new();
        let mut words: Vec<String> = Vec::new();
        for ch in seg.chars() {
            word.push(ch);
            if ch == ' ' {
                words.push(std::mem::take(&mut word));
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        for w in words {
            if !cur.is_empty() && cur.chars().count() + w.chars().count() > max {
                lines.push(format!("\"{}\"", cur));
                cur.clear();
            }
            cur.push_str(&w);
        }
        if !cur.is_empty() {
            lines.push(format!("\"{}\"", cur));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# Header comment
msgid ""
msgstr ""
"Language: it\n"
"Plural-Forms: nplurals=2; plural=n != 1;\n"

#: app/admin.py:104
msgid "Cannot restore default logo"
msgstr "Non è possibile ripristinare il logo predefinito"

#: app/a.py:1 app/b.py:2
#, python-format
msgid ""
"Restart required. Please restart WebODM to fully disable %(plugin)s and "
"everything else"
msgstr ""

msgid "One file"
msgid_plural "%d files"
msgstr[0] ""
msgstr[1] ""

#~ msgid "Old"
#~ msgstr ""
"#;

    #[test]
    fn roundtrip_identity() {
        let po = PoFile::parse(SAMPLE);
        assert_eq!(po.render(), SAMPLE);
    }

    #[test]
    fn parse_fields() {
        let po = PoFile::parse(SAMPLE);
        let es: Vec<_> = po.entries().collect();
        assert_eq!(es.len(), 5);
        assert!(es[0].is_header());
        assert_eq!(po.plural_forms().0, 2);
        assert_eq!(es[2].references, vec!["app/a.py:1", "app/b.py:2"]);
        assert!(es[2].msgid.ends_with("%(plugin)s and everything else"));
        assert!(es[2].needs_translation(false));
        assert!(es[3].msgid_plural.is_some());
        assert!(es[4].obsolete);
        assert!(!es[4].needs_translation(false));
    }

    #[test]
    fn modify_entry() {
        let mut po = PoFile::parse(SAMPLE);
        let e = po.entries_mut().nth(2).unwrap();
        e.set_translation(vec!["Riavvio necessario. Riavviare WebODM per disabilitare completamente %(plugin)s e tutto il resto".into()], Some(crate::auto_comment("opus", "2026-11-11", true)), true);
        let out = po.render();
        assert!(out.contains("# Translated by opus on 2026-11-11, needs review\n#: app/a.py:1 app/b.py:2\n#, fuzzy, python-format\nmsgid"));
        let re = PoFile::parse(&out);
        let e = re.entries().nth(2).unwrap();
        assert!(e.is_fuzzy());
        assert!(e.msgstr[0].ends_with("e tutto il resto"));
        for l in out.lines() {
            assert!(l.chars().count() <= 79, "line too long: {}", l);
        }
    }

    #[test]
    fn escape_roundtrip() {
        let s = "a \"q\" \\ b\nc\t";
        assert_eq!(unescape(&escape(s)), s);
    }
}

#[cfg(test)]
mod corpus_tests {
    use super::*;

    #[test]
    fn review_clears_flag_note_and_previous() {
        let text = "msgid \"\"\nmsgstr \"\"\n\n# keep me\n# Translated by opus on 2026-11-11, needs review\n#: a.py:1\n#, fuzzy, python-format\n#| msgid \"Old\"\nmsgid \"Hello %(n)s\"\nmsgstr \"Ciao %(n)s\"\n";
        let mut po = PoFile::parse(text);
        let e = po.entries_mut().nth(1).unwrap();
        assert!(e.auto_note().is_some());
        e.set_needs_review(false);
        let out = po.render();
        assert!(out.contains("# keep me\n#: a.py:1\n#, python-format\nmsgid \"Hello %(n)s\"\nmsgstr \"Ciao %(n)s\"\n"), "{}", out);
        let e = po.entries_mut().nth(1).unwrap();
        e.set_manual(vec!["Salve %(n)s".into()]);
        e.set_needs_review(true);
        assert!(po
            .render()
            .contains("#, fuzzy, python-format\nmsgid \"Hello %(n)s\"\nmsgstr \"Salve %(n)s\""));
    }

    #[test]
    fn previous_msgid_from_hash_bar_lines() {
        let text = "msgid \"\"\nmsgstr \"\"\n\n#, fuzzy\n#| msgctxt \"x\"\n#| msgid \"\"\n#| \"Old \"\n#| \"text\\n\"\n#| msgid_plural \"Olds\"\nmsgid \"New text\"\nmsgstr \"\"\n\nmsgid \"Plain\"\nmsgstr \"\"\n";
        let po = PoFile::parse(text);
        let mut e = po.entries().skip(1);
        assert_eq!(
            e.next().unwrap().previous_msgid().as_deref(),
            Some("Old text\n")
        );
        assert_eq!(e.next().unwrap().previous_msgid(), None);
    }

    #[test]
    fn translator_comments_are_edited_in_place() {
        let text = "msgid \"\"\nmsgstr \"\"\n\n# keep short\n# Translated by opus on 2026-11-11, needs review\n#. dev note\n#: a.py:1\n#, fuzzy\nmsgid \"Hi\"\nmsgstr \"Ciao\"\n";
        let mut po = PoFile::parse(text);
        let e = po.entries_mut().nth(1).unwrap();
        assert_eq!(e.translator_comments(), "keep short");
        e.set_translator_comments("button label\n\nmax 10 chars\n");
        assert_eq!(e.translator_comments(), "button label\n\nmax 10 chars");
        assert!(po.render().contains("# button label\n#\n# max 10 chars\n# Translated by opus on 2026-11-11, needs review\n#. dev note\n#: a.py:1\n#, fuzzy\nmsgid"), "{}", po.render());
        po.entries_mut()
            .nth(1)
            .unwrap()
            .set_translator_comments("  ");
        assert!(
            po.render()
                .contains("\n\n# Translated by opus on 2026-11-11, needs review\n#. dev note"),
            "{}",
            po.render()
        );
    }

    #[test]
    fn approve_one_entry_in_a_file() {
        let dir = std::env::temp_dir().join(format!("polibre-approve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("django.po");
        std::fs::write(&path, "msgid \"\"\nmsgstr \"\"\n\n# Translated by opus on 2026-11-11, needs review\n#, fuzzy\nmsgctxt \"menu\"\nmsgid \"Open\"\nmsgstr \"Apri\"\n\n#, fuzzy\nmsgid \"Open\"\nmsgstr \"Aperto\"\n").unwrap();
        assert!(PoFile::approve_in_file(&path, Some("menu"), "Open").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\n\nmsgctxt \"menu\"\nmsgid \"Open\"\nmsgstr \"Apri\"\n"),
            "{}",
            text
        );
        assert!(
            text.contains("#, fuzzy\nmsgid \"Open\"\nmsgstr \"Aperto\""),
            "the other entry is untouched"
        );
        assert!(!PoFile::approve_in_file(&path, None, "Missing").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn auto_note_is_replaced_or_removed() {
        let text = "msgid \"\"\nmsgstr \"\"\n\n# keep me\n# Translated by opus on 2026-11-11, needs review\n#: a.py:1\n#, fuzzy\nmsgid \"Hi\"\nmsgstr \"Ciao\"\n";
        let mut po = PoFile::parse(text);
        let e = po.entries_mut().nth(1).unwrap();
        e.set_auto_note(Some(
            "Translated by libretranslate on 2026-11-12, needs review".into(),
        ));
        assert_eq!(
            e.auto_note(),
            Some("Translated by libretranslate on 2026-11-12, needs review")
        );
        assert!(po.render().contains("# keep me\n# Translated by libretranslate on 2026-11-12, needs review\n#: a.py:1\n#, fuzzy\nmsgid"), "{}", po.render());
        po.entries_mut().nth(1).unwrap().set_auto_note(None);
        assert!(
            po.render()
                .contains("# keep me\n#: a.py:1\n#, fuzzy\nmsgid"),
            "{}",
            po.render()
        );
    }

    /// Every .po file under $POLIBRE_CORPUS must round-trip byte-for-byte.
    #[test]
    fn corpus_roundtrip() {
        let Ok(dir) = std::env::var("POLIBRE_CORPUS") else {
            return;
        };
        let mut n = 0;
        for p in crate::project::find_po_files(std::path::Path::new(&dir)) {
            let text = std::fs::read_to_string(&p).unwrap();
            let po = super::PoFile::parse(&text);
            assert_eq!(po.render(), text, "roundtrip mismatch: {}", p.display());
            n += 1;
        }
        eprintln!("{} files round-tripped", n);
    }
}
