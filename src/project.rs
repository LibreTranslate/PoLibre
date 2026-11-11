// SPDX-License-Identifier: AGPL-3.0-or-later
//! Project scanning: find .po files, their languages, the source tree that
//! their `#:` references point into, a short project summary and glossaries.

use crate::po::PoFile;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "env",
    "site-packages",
    "dist-packages",
    ".tox",
    ".cache",
    "target",
    "__pycache__",
    ".mypy_cache",
    "bower_components",
    "vendor",
];

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PoInfo {
    pub path: PathBuf,
    pub lang: String,
    pub total: usize,
    pub untranslated: usize,
    pub fuzzy: usize,
    /// Translated and not waiting for review.
    pub reviewed: usize,
}

#[derive(Debug, Clone, Default)]
pub struct LangInfo {
    pub code: String,
    pub name: String,
    pub files: Vec<PoInfo>,
}

impl LangInfo {
    pub fn untranslated(&self) -> usize {
        self.files.iter().map(|f| f.untranslated).sum()
    }
    pub fn fuzzy(&self) -> usize {
        self.files.iter().map(|f| f.fuzzy).sum()
    }
    pub fn total(&self) -> usize {
        self.files.iter().map(|f| f.total).sum()
    }
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn reviewed(&self) -> usize {
        self.files.iter().map(|f| f.reviewed).sum()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Project {
    pub root: PathBuf,
    pub languages: BTreeMap<String, LangInfo>,
    pub source_root: Option<PathBuf>,
    pub summary: String,
    pub name: String,
}

pub fn find_po_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.is_file() {
        if root.extension().map(|e| e == "po").unwrap_or(false) {
            out.push(root.to_path_buf());
        }
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) && !(name.starts_with('.') && name.len() > 1)
                {
                    stack.push(p);
                }
            } else if name.ends_with(".po") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Language of a .po file: `<lang>/LC_MESSAGES/x.po`, `<lang>.po`, or the
/// header's `Language:` field.
pub fn po_language(path: &Path, po: &PoFile) -> Option<String> {
    let parent = path.parent()?;
    if parent
        .file_name()
        .map(|n| n == "LC_MESSAGES")
        .unwrap_or(false)
    {
        if let Some(l) = parent.parent().and_then(|p| p.file_name()) {
            return Some(l.to_string_lossy().to_string());
        }
    }
    if let Some(h) = po.header_field("Language") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    let stem = path.file_stem()?.to_string_lossy().to_string();
    if stem.len() <= 10
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '@')
    {
        return Some(stem);
    }
    None
}

pub fn scan(root: &Path, include_fuzzy: bool) -> Result<Project, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {}", root.display(), e))?;
    let mut langs: BTreeMap<String, LangInfo> = BTreeMap::new();
    let mut sample_refs: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for path in find_po_files(&root) {
        let Ok(po) = PoFile::load(&path) else {
            continue;
        };
        let Some(lang) = po_language(&path, &po) else {
            continue;
        };
        let mut info = PoInfo {
            path: path.clone(),
            lang: lang.clone(),
            total: 0,
            untranslated: 0,
            fuzzy: 0,
            reviewed: 0,
        };
        let mut refs = Vec::new();
        for e in po.entries() {
            if e.obsolete || e.is_header() {
                continue;
            }
            info.total += 1;
            if e.needs_translation(include_fuzzy) {
                info.untranslated += 1;
            }
            if e.is_fuzzy() && !e.is_untranslated() {
                info.fuzzy += 1;
            }
            if !e.is_fuzzy() && !e.is_untranslated() {
                info.reviewed += 1;
            }
            if refs.len() < 40 {
                refs.extend(e.references.iter().take(1).cloned());
            }
        }
        if sample_refs.len() < 6 && !refs.is_empty() {
            sample_refs.push((path.clone(), refs));
        }
        let li = langs.entry(lang.clone()).or_insert_with(|| LangInfo {
            code: lang.clone(),
            name: crate::langs::name(&lang),
            files: vec![],
        });
        li.files.push(info);
    }
    let source_root = detect_source_root(&root, &sample_refs);
    let name = source_root
        .as_ref()
        .unwrap_or(&root)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let summary = detect_summary(source_root.as_deref().unwrap_or(&root), &root);
    Ok(Project {
        root,
        languages: langs,
        source_root,
        summary,
        name,
    })
}

fn ref_path(r: &str) -> &str {
    // "app/admin.py:104" -> "app/admin.py"; also handle paths without line numbers.
    match r.rsplit_once(':') {
        Some((p, l)) if l.chars().all(|c| c.is_ascii_digit()) => p,
        _ => r,
    }
}

/// Find the directory that `#:` references are relative to. Candidates are
/// the project root, every ancestor of each .po file, and (for robustness)
/// ancestors of the project root.
pub fn detect_source_root(root: &Path, samples: &[(PathBuf, Vec<String>)]) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![root.to_path_buf()];
    for (po, _) in samples {
        let mut d = po.parent();
        while let Some(x) = d {
            if !candidates.contains(&x.to_path_buf()) {
                candidates.push(x.to_path_buf());
            }
            d = x.parent();
            if candidates.len() > 64 {
                break;
            }
        }
    }
    let mut best: Option<(usize, PathBuf)> = None;
    for c in candidates {
        let mut hits = 0;
        for (_, refs) in samples {
            for r in refs {
                if c.join(ref_path(r)).is_file() {
                    hits += 1;
                }
            }
        }
        if hits > 0 && best.as_ref().map(|(h, _)| hits > *h).unwrap_or(true) {
            best = Some((hits, c));
        }
    }
    best.map(|(_, p)| p)
}

/// A short description of the project from its README (or package metadata).
pub fn detect_summary(dir: &Path, fallback: &Path) -> String {
    for d in [dir, fallback] {
        for name in [
            "README.md",
            "README.rst",
            "README.txt",
            "README",
            "readme.md",
        ] {
            if let Ok(t) = fs::read_to_string(d.join(name)) {
                let s = summarize_readme(&t);
                if !s.is_empty() {
                    return s;
                }
            }
        }
        if let Ok(t) = fs::read_to_string(d.join("package.json")) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                if let Some(desc) = v["description"].as_str() {
                    if !desc.trim().is_empty() {
                        return desc.trim().to_string();
                    }
                }
            }
        }
    }
    String::new()
}

fn strip_md_links(s: &str) -> String {
    // [text](url) -> text ; drop bare images
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(j) = after.find("](") {
            if let Some(k) = after[j + 2..].find(')') {
                out.push_str(&after[..j]);
                rest = &after[j + 2 + k + 1..];
                continue;
            }
        }
        out.push('[');
        rest = after;
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "")
}

pub fn summarize_readme(text: &str) -> String {
    let mut paras: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        let l = line.trim();
        let junk = l.starts_with('<')
            || l.starts_with("![")
            || l.starts_with("[![")
            || l.starts_with('#')
            || l.starts_with('>')
            || l.starts_with("```")
            || l.starts_with('|')
            || l.starts_with("=")
            || l.starts_with("--")
            || l.starts_with(".. ")
            || l.starts_with('*')
            || l.starts_with("- ");
        if l.starts_with('#') && !paras.is_empty() {
            break; // only describe what comes before the first section
        }
        if l.is_empty() || junk {
            if !cur.trim().is_empty() {
                paras.push(std::mem::take(&mut cur));
                if paras.len() >= 2 {
                    break;
                }
            }
            cur.clear();
            continue;
        }
        cur.push_str(&strip_md_links(l));
        cur.push(' ');
    }
    if !cur.trim().is_empty() && paras.len() < 2 {
        paras.push(cur);
    }
    let paras: Vec<String> = paras
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| p.split_whitespace().count() >= 6)
        .collect();
    let mut s = paras.join(" ");
    if s.chars().count() > 600 {
        let cut: String = s.chars().take(600).collect();
        s = match cut.rfind(". ") {
            Some(i) if i > 200 => cut[..=i].to_string(),
            _ => format!("{}…", cut),
        };
    }
    s.trim().to_string()
}

// ---------------------------------------------------------------------------
// Source context
// ---------------------------------------------------------------------------

/// Cache of source files used to build code context for LLM prompts.
#[derive(Default)]
pub struct SourceCache {
    files: HashMap<PathBuf, Option<Vec<String>>>,
}

impl SourceCache {
    /// The referenced line +/- `radius` lines, formatted with line numbers.
    pub fn snippet(&mut self, root: &Path, reference: &str, radius: usize) -> Option<String> {
        let (p, line) = reference.rsplit_once(':')?;
        let line: usize = line.parse().ok()?;
        let path = root.join(p);
        let lines = self
            .files
            .entry(path.clone())
            .or_insert_with(|| {
                let meta = fs::metadata(&path).ok()?;
                if meta.len() > 5_000_000 {
                    return None;
                }
                let bytes = fs::read(&path).ok()?;
                Some(
                    String::from_utf8_lossy(&bytes)
                        .lines()
                        .map(|l| l.to_string())
                        .collect(),
                )
            })
            .as_ref()?;
        if line == 0 || line > lines.len() {
            return None;
        }
        let start = line.saturating_sub(radius + 1);
        let end = (line + radius).min(lines.len());
        let mut out = String::new();
        for (i, l) in lines[start..end].iter().enumerate() {
            let n = start + i + 1;
            let mut t: String = l.chars().take(240).collect();
            if l.chars().count() > 240 {
                t.push('…');
            }
            out.push_str(&format!(
                "{}{:>5}| {}\n",
                if n == line { ">" } else { " " },
                n,
                t
            ));
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Glossary
// ---------------------------------------------------------------------------

/// Terminology already established in the project's translations for one
/// language (short, reviewed entries), plus user supplied glossary terms.
#[derive(Default, Clone)]
pub struct Glossary {
    terms: Vec<(String, String)>,
}

impl Glossary {
    pub fn from_po_files(files: &[PathBuf]) -> Glossary {
        let mut map: BTreeMap<String, String> = BTreeMap::new();
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
                let id = e.msgid.trim();
                let words = id.split_whitespace().count();
                if words == 0 || words > 3 || id.len() > 40 || id.contains(['%', '{', '<', '\n']) {
                    continue;
                }
                if !id.chars().any(|c| c.is_alphabetic()) {
                    continue;
                }
                let tr = e.msgstr[0].trim();
                if tr.is_empty() {
                    continue;
                }
                map.entry(id.to_string()).or_insert_with(|| tr.to_string());
            }
        }
        Glossary {
            terms: map.into_iter().collect(),
        }
    }

    /// Load a user glossary: CSV/TSV lines `source,translation` (optionally
    /// `lang,source,translation`, filtered by `lang`), or a .po file.
    pub fn load_user(path: &Path, lang: &str) -> Result<Glossary, String> {
        if path.extension().map(|e| e == "po").unwrap_or(false) {
            return Ok(Self::from_po_files(&[path.to_path_buf()]));
        }
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let mut terms = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let sep = if l.contains('\t') { '\t' } else { ',' };
            let cols: Vec<String> = l
                .split(sep)
                .map(|c| c.trim().trim_matches('"').to_string())
                .collect();
            match cols.len() {
                2 => terms.push((cols[0].clone(), cols[1].clone())),
                n if n >= 3 => {
                    if crate::langs::normalize(&cols[0])
                        .eq_ignore_ascii_case(&crate::langs::normalize(lang))
                    {
                        terms.push((cols[1].clone(), cols[2].clone()));
                    }
                }
                _ => {}
            }
        }
        Ok(Glossary { terms })
    }

    pub fn merge(&mut self, other: Glossary) {
        // user terms take precedence
        for (k, v) in other.terms {
            self.terms.retain(|(a, _)| !a.eq_ignore_ascii_case(&k));
            self.terms.push((k, v));
        }
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    /// Terms that occur (as whole words) in any of the given texts.
    pub fn relevant(&self, texts: &[&str], max: usize) -> Vec<(String, String)> {
        let lowered: Vec<String> = texts.iter().map(|t| t.to_lowercase()).collect();
        let mut out: Vec<(String, String)> = Vec::new();
        for (k, v) in &self.terms {
            let kl = k.to_lowercase();
            if lowered
                .iter()
                .any(|t| contains_word(t, &kl) && t.trim() != kl)
            {
                out.push((k.clone(), v.clone()));
            }
        }
        // Prefer longer (more specific) terms.
        out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        out.truncate(max);
        out
    }
}

fn contains_word(hay: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(i) = hay[start..].find(needle) {
        let a = start + i;
        let b = a + needle.len();
        let before = hay[..a]
            .chars()
            .next_back()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        let after = hay[b..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        if before && after {
            return true;
        }
        start = a + needle.len().max(1);
        if start >= hay.len() {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readme_summary() {
        let t = "<img src=x>\n\n![b](c) [![d](e)](f)\n\n> **Now with**\n\nA user-friendly, commercial grade software for drone image processing. See [ODX](http://x).\n\n# Getting\n * [Download](x)\n";
        let s = summarize_readme(t);
        assert!(s.starts_with("A user-friendly"), "{}", s);
        assert!(s.contains("See ODX."));
    }
    #[test]
    fn words() {
        assert!(contains_word("open the project", "project"));
        assert!(!contains_word("open the projects", "project"));
    }
}
