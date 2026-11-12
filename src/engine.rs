// SPDX-License-Identifier: AGPL-3.0-or-later
//! The translation job runner shared by the CLI and the GUI. It reports
//! progress through a channel of [`Event`]s and can be cancelled at any time
//! through an atomic flag. Files are saved after every batch, so an
//! interrupted run keeps everything translated so far.

use crate::config::{ProviderConfig, ProviderKind};
use crate::langs;
use crate::po::{Entry, Item, PoFile};
use crate::project::{Glossary, SourceCache};
use crate::providers::{self, Cancel, LtLanguages};
use crate::validate::{self, Src};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct JobSettings {
    pub source_root: Option<PathBuf>,
    pub project_name: String,
    pub summary: String,
    pub source_lang: String,
    pub include_fuzzy: bool,
    pub add_comment: bool,
    /// Mark machine translations fuzzy, so they need review before use.
    pub mark_fuzzy: bool,
    /// 0 = provider default
    pub batch_size: usize,
    /// Maximum number of entries to translate in this run.
    pub limit: Option<usize>,
    pub dry_run: bool,
    pub glossary_file: Option<PathBuf>,
    pub context_lines: usize,
    pub retry_rejected: bool,
    /// Send `Event::Debug` entries with every request, reply and check.
    pub debug: bool,
    /// Entries not to translate: (file, msgctxt, msgid), e.g. translations
    /// already waiting for the user's decision in the GUI.
    pub skip: HashSet<(PathBuf, Option<String>, String)>,
}

impl JobSettings {
    fn wanted(&self, path: &std::path::Path, e: &Entry) -> bool {
        e.needs_translation(self.include_fuzzy)
            && (self.skip.is_empty()
                || !self.skip.contains(&(path.to_path_buf(), e.msgctxt.clone(), e.msgid.clone())))
    }
}

impl Default for JobSettings {
    fn default() -> Self {
        JobSettings {
            source_root: None,
            project_name: String::new(),
            summary: String::new(),
            source_lang: "en".into(),
            include_fuzzy: false,
            add_comment: true,
            mark_fuzzy: true,
            batch_size: 0,
            limit: None,
            dry_run: false,
            glossary_file: None,
            context_lines: 2,
            retry_rejected: true,
            debug: false,
            skip: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Target {
    pub lang: String,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub translated: usize,
    pub rejected: usize,
    pub failed: usize,
    pub skipped_langs: Vec<String>,
    pub files_changed: usize,
    pub cancelled: bool,
    pub fatal: Option<String>,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // some fields are only used by one of the two front-ends
pub enum Event {
    Info(String),
    Warn(String),
    Error(String),
    /// Total number of entries that will be translated.
    Plan {
        total: usize,
        files: usize,
    },
    FileStart {
        path: PathBuf,
        lang: String,
        count: usize,
    },
    /// The strings currently being sent to the provider.
    Working {
        lang: String,
        texts: Vec<String>,
    },
    Translated {
        lang: String,
        path: PathBuf,
        msgctxt: Option<String>,
        msgid: String,
        /// The translation; several forms for plural entries.
        forms: Vec<String>,
    },
    Rejected {
        lang: String,
        path: PathBuf,
        msgctxt: Option<String>,
        msgid: String,
        reason: String,
    },
    /// Entries that could not be processed because a request failed.
    Failed {
        count: usize,
    },
    FileDone {
        path: PathBuf,
        translated: usize,
    },
    /// Detailed trace for the debug console (only sent when `JobSettings::debug` is on).
    Debug {
        title: String,
        body: String,
    },
    Finished(Summary),
}

/// One entry selected for translation.
#[derive(Debug, Clone)]
struct Work {
    idx: usize,
    msgid: String,
    msgid_plural: Option<String>,
    msgctxt: Option<String>,
    flags: Vec<String>,
    refs: Vec<String>,
    notes: Vec<String>,
}

impl Work {
    fn from_entry(idx: usize, e: &Entry) -> Work {
        Work {
            idx,
            msgid: e.msgid.clone(),
            msgid_plural: e.msgid_plural.clone(),
            msgctxt: e.msgctxt.clone(),
            flags: e.flags.iter().filter(|f| *f != "fuzzy").cloned().collect(),
            refs: e.references.clone(),
            notes: e.extracted_comments.clone(),
        }
    }
}

struct Ctx<'a> {
    provider: &'a ProviderConfig,
    settings: &'a JobSettings,
    tx: &'a Sender<Event>,
    cancel: &'a Cancel,
    sources: SourceCache,
    comment: Option<String>,
}

impl Ctx<'_> {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn debug(&self, title: impl Into<String>, body: impl FnOnce() -> String) {
        if self.settings.debug {
            self.send(Event::Debug {
                title: title.into(),
                body: body(),
            });
        }
    }
}

/// Sends provider notices (and debug output, if on) from this thread to `tx`
/// until the returned guard is dropped.
fn install_sinks(tx: &Sender<Event>, debug: bool) -> DebugGuard {
    let tx2 = tx.clone();
    providers::set_notice(Some(Arc::new(move |m| {
        let _ = tx2.send(Event::Info(m));
    })));
    if debug {
        let tx2 = tx.clone();
        providers::set_debug(Some(Arc::new(move |title, body| {
            let _ = tx2.send(Event::Debug { title, body });
        })));
    }
    DebugGuard
}

/// Turns provider-level debug output off again when a run ends.
struct DebugGuard;
impl Drop for DebugGuard {
    fn drop(&mut self) {
        providers::set_debug(None);
        providers::set_notice(None);
    }
}

fn short(s: &str, n: usize) -> String {
    let s = s.replace('\n', "⏎");
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s
    }
}

pub fn run(
    provider: ProviderConfig,
    settings: JobSettings,
    targets: Vec<Target>,
    tx: Sender<Event>,
    cancel: Cancel,
) -> Summary {
    let start = Instant::now();
    let mut summary = Summary::default();
    let comment = auto_note(&provider, &settings);
    let mut ctx = Ctx {
        provider: &provider,
        settings: &settings,
        tx: &tx,
        cancel: &cancel,
        sources: SourceCache::default(),
        comment,
    };
    let _guard = install_sinks(&tx, settings.debug);
    if settings.debug {
        ctx.debug("Run settings", || {
            let key = if provider.api_key.is_empty() { "(none)".to_string() } else { providers::mask(&provider.api_key) };
            let mut b = format!(
                "Service: {}\nModel: {}\nServer: {}\nAPI key: {}\nSource language: {}\nStrings per request: {}\nLimit: {}\nPreview only: {}\nRedo fuzzy entries: {}\nReview note: {}\nRetry rejected: {}\nSource code folder: {}\nCode lines around each reference: {}\nGlossary file: {}\n\nLanguages:\n",
                provider.kind.id(),
                if provider.model.is_empty() { "(service default)" } else { &provider.model },
                if provider.url.is_empty() { "(n/a)" } else { &provider.url },
                key,
                settings.source_lang,
                if settings.batch_size == 0 { "automatic".to_string() } else { settings.batch_size.to_string() },
                settings.limit.map(|l| l.to_string()).unwrap_or_else(|| "none".into()),
                settings.dry_run,
                settings.include_fuzzy,
                ctx.comment.as_deref().unwrap_or("(off)"),
                settings.retry_rejected,
                settings.source_root.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(none)".into()),
                settings.context_lines,
                settings.glossary_file.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(none)".into()),
            );
            for t in &targets {
                b.push_str(&format!("  {} ({})\n", langs::name(&t.lang), t.lang));
                for f in &t.files {
                    b.push_str(&format!("    {}\n", f.display()));
                }
            }
            b
        });
    }

    // Map languages for LibreTranslate and drop unsupported ones.
    let mut lt_codes: HashMap<String, String> = HashMap::new();
    let mut targets = targets;
    if provider.kind == ProviderKind::Libretranslate {
        let langs = match LtLanguages::fetch(&provider.url, &settings.source_lang) {
            Ok(l) => l,
            Err(e) => {
                ctx.send(Event::Warn(format!(
                    "Unable to get the server's language list ({}), so the built-in list is used",
                    e
                )));
                LtLanguages::fallback()
            }
        };
        targets.retain(|t| match langs.map(&t.lang) {
            Some(code) => {
                lt_codes.insert(t.lang.clone(), code);
                true
            }
            None => {
                ctx.send(Event::Warn(format!(
                    "{} ({}) was skipped because the LibreTranslate server doesn't support it",
                    langs::name(&t.lang),
                    t.lang
                )));
                summary.skipped_langs.push(t.lang.clone());
                false
            }
        });
    }

    // Plan
    let mut total = 0usize;
    let mut nfiles = 0usize;
    for t in &targets {
        for f in &t.files {
            if let Ok(po) = PoFile::load(f) {
                let n = po
                    .entries()
                    .filter(|e| settings.wanted(f, e))
                    .count();
                if n > 0 {
                    nfiles += 1;
                }
                total += n;
            }
        }
    }
    if let Some(l) = settings.limit {
        total = total.min(l);
    }
    ctx.send(Event::Plan {
        total,
        files: nfiles,
    });
    let mut remaining = settings.limit.unwrap_or(usize::MAX);
    let mut consecutive_failures = 0;

    'outer: for t in &targets {
        if remaining == 0 {
            break;
        }
        let glossary = run_glossary(&ctx, &t.lang, &t.files);
        if provider.kind.is_llm() {
            ctx.send(Event::Info(format!(
                "{}: using {} terms already translated in the project as a glossary",
                langs::name(&t.lang),
                glossary.len()
            )));
        }
        if let Some(code) = lt_codes.get(&t.lang) {
            ctx.debug(
                format!("{}: LibreTranslate code", langs::name(&t.lang)),
                || format!("{} → {}", t.lang, code),
            );
        }
        for path in &t.files {
            if ctx.cancelled() || remaining == 0 {
                break 'outer;
            }
            let mut po = match PoFile::load(path) {
                Ok(p) => p,
                Err(e) => {
                    ctx.send(Event::Error(format!("{}: {}", path.display(), e)));
                    continue;
                }
            };
            let (nplurals, plural_expr) = po.plural_forms();
            let mut work: Vec<Work> = po
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| match it {
                    Item::Entry(e) if settings.wanted(path, e) => {
                        Some(Work::from_entry(i, e))
                    }
                    _ => None,
                })
                .collect();
            if work.is_empty() {
                continue;
            }
            work.truncate(remaining);
            remaining -= work.len();
            ctx.send(Event::FileStart {
                path: path.clone(),
                lang: t.lang.clone(),
                count: work.len(),
            });

            let bs = if settings.batch_size > 0 {
                settings.batch_size
            } else if provider.kind.is_llm() {
                20
            } else {
                25
            };
            ctx.debug(
                format!("{} · {}", langs::name(&t.lang), path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()),
                || {
                    format!(
                        "File: {}\nStrings to translate: {}\nPlural forms: {} ({})\nRequests: {} of up to {} strings",
                        path.display(),
                        work.len(),
                        nplurals,
                        if plural_expr.is_empty() { "not set" } else { &plural_expr },
                        work.len().div_ceil(bs),
                        bs
                    )
                },
            );
            let mut file_translated = 0;
            for batch in work.chunks(bs) {
                if ctx.cancelled() {
                    break;
                }
                ctx.send(Event::Working {
                    lang: t.lang.clone(),
                    texts: batch.iter().map(|w| w.msgid.clone()).collect(),
                });
                let result = if provider.kind.is_llm() {
                    translate_llm(&mut ctx, &t.lang, batch, nplurals, &plural_expr, &glossary)
                } else {
                    translate_lt(&ctx, &t.lang, &lt_codes[&t.lang], batch, nplurals)
                };
                let results = match result {
                    Ok(r) => {
                        consecutive_failures = 0;
                        r
                    }
                    Err(e) => {
                        if ctx.cancelled() {
                            break;
                        }
                        consecutive_failures += 1;
                        summary.failed += batch.len();
                        ctx.send(Event::Error(format!(
                            "Unable to translate {} strings: {}",
                            batch.len(),
                            e
                        )));
                        ctx.send(Event::Failed { count: batch.len() });
                        if consecutive_failures >= 3 {
                            summary.fatal = Some(format!("Stopped because 3 requests in a row couldn't be completed. Last problem: {}", e));
                            break 'outer;
                        }
                        continue;
                    }
                };
                let mut changed = false;
                for (w, r) in batch.iter().zip(results) {
                    match r {
                        Ok(forms) => {
                            if let Item::Entry(e) = &mut po.items[w.idx] {
                                let shown = forms.clone();
                                e.set_translation(
                                    forms,
                                    ctx.comment.clone(),
                                    ctx.settings.mark_fuzzy,
                                );
                                changed = true;
                                file_translated += 1;
                                summary.translated += 1;
                                ctx.send(Event::Translated {
                                    lang: t.lang.clone(),
                                    path: path.clone(),
                                    msgctxt: w.msgctxt.clone(),
                                    msgid: w.msgid.clone(),
                                    forms: shown,
                                });
                            }
                        }
                        Err(reason) => {
                            summary.rejected += 1;
                            ctx.send(Event::Rejected {
                                lang: t.lang.clone(),
                                path: path.clone(),
                                msgctxt: w.msgctxt.clone(),
                                msgid: w.msgid.clone(),
                                reason,
                            });
                        }
                    }
                }
                if changed && !settings.dry_run {
                    if let Err(e) = po.save(path) {
                        summary.fatal = Some(format!("Unable to save {}: {}", path.display(), e));
                        ctx.send(Event::Error(summary.fatal.clone().unwrap()));
                        break 'outer;
                    }
                }
            }
            if file_translated > 0 {
                summary.files_changed += 1;
            }
            ctx.send(Event::FileDone {
                path: path.clone(),
                translated: file_translated,
            });
        }
    }
    summary.cancelled = ctx.cancelled();
    summary.elapsed = start.elapsed();
    ctx.send(Event::Finished(summary.clone()));
    summary
}

/// The "Translated by <model> on <date>" note for new translations, if enabled.
pub fn auto_note(provider: &ProviderConfig, settings: &JobSettings) -> Option<String> {
    settings.add_comment.then(|| {
        let by = if provider.kind.is_llm() && !provider.model.is_empty() {
            provider.model.as_str()
        } else {
            provider.kind.id()
        };
        crate::auto_comment(by)
    })
}

/// A translation for display, with plural forms told apart (joining them
/// with a separator would look like alternatives):
/// "Singular: …" / "Plural: …", or "Form 1: …" for languages with more forms.
pub fn show_forms(forms: &[String], sep: &str) -> String {
    if forms.len() == 1 {
        return forms[0].clone();
    }
    forms
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{}: {}", form_label(i, forms.len()), f))
        .collect::<Vec<_>>()
        .join(sep)
}

/// The name of plural form `k` of `n`: "Singular"/"Plural" for languages
/// with two forms, otherwise "Form 1", "Form 2", …
pub fn form_label(k: usize, n: usize) -> String {
    match (n, k) {
        (2, 0) => "Singular".into(),
        (2, _) => "Plural".into(),
        _ => format!("Form {}", k + 1),
    }
}

/// The glossary sent to LLMs: terms already translated in the language's
/// files plus the user's glossary file. Empty for LibreTranslate.
fn run_glossary(ctx: &Ctx, lang: &str, files: &[PathBuf]) -> Glossary {
    if !ctx.provider.kind.is_llm() {
        return Glossary::default();
    }
    let mut glossary = Glossary::from_po_files(files);
    if let Some(g) = &ctx.settings.glossary_file {
        match Glossary::load_user(g, lang) {
            Ok(user) => glossary.merge(user),
            Err(e) => ctx.send(Event::Warn(format!(
                "Unable to read the glossary file: {}",
                e
            ))),
        }
    }
    glossary
}

/// Translates a single entry on request (the editor's "Suggest" button) with
/// the same prompt, context and checks as a run. `files` are the language's
/// .po files, used for the glossary. Notices and debug output go to `tx`.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
pub fn suggest(
    provider: &ProviderConfig,
    settings: &JobSettings,
    lang: &str,
    files: &[PathBuf],
    entry: &Entry,
    nplurals: usize,
    plural_expr: &str,
    tx: &Sender<Event>,
    cancel: &Cancel,
) -> Result<Vec<String>, String> {
    let _guard = install_sinks(tx, settings.debug);
    let mut ctx = Ctx {
        provider,
        settings,
        tx,
        cancel,
        sources: SourceCache::default(),
        comment: None,
    };
    let work = [Work::from_entry(0, entry)];
    let results = if provider.kind.is_llm() {
        let glossary = run_glossary(&ctx, lang, files);
        translate_llm(&mut ctx, lang, &work, nplurals, plural_expr, &glossary)?
    } else {
        let langs = LtLanguages::fetch(&provider.url, &settings.source_lang)
            .unwrap_or_else(|_| LtLanguages::fallback());
        let code = langs.map(lang).ok_or_else(|| {
            format!(
                "the LibreTranslate server doesn't support {}",
                langs::name(lang)
            )
        })?;
        translate_lt(&ctx, lang, &code, &work, nplurals)?
    };
    results
        .into_iter()
        .next()
        .unwrap_or_else(|| Err("no translation returned".into()))
}

// ---------------------------------------------------------------------------
// LLM
// ---------------------------------------------------------------------------

pub fn system_prompt(s: &JobSettings, lang: &str, nplurals: usize, plural_expr: &str) -> String {
    let tgt = langs::name(lang);
    let src = langs::name(&s.source_lang);
    let name = if s.project_name.is_empty() {
        "a software project".to_string()
    } else {
        format!("\"{}\"", s.project_name)
    };
    let mut p = format!(
        "You are an expert software localizer. You translate user interface strings of {name} from {src} to {tgt} (locale code: {lang}).\n"
    );
    if !s.summary.trim().is_empty() {
        p.push_str(&format!("\nAbout the software: {}\n", s.summary.trim()));
    }
    p.push_str(&format!(
        r#"
Rules:
1. Translate each item's "text" naturally and idiomatically, as it should read in the {tgt} user interface of this software. Follow the conventions (formality, capitalization, punctuation) of the existing translation and use the terms from "glossary" consistently.
2. Preserve exactly, character for character: placeholders (%s, %d, %(name)s, {{name}}, {{0}}, {{{{ var }}}}, ${{var}}), HTML/XML tags and their attributes, HTML entities, URLs, e-mail addresses, and any leading/trailing spaces and line breaks. Placeholders may be reordered only if grammar requires it.
3. Do not translate product names, brand names, code identifiers, file formats or units, unless a translation is well established.
4. "context" disambiguates the meaning, "note" is a comment from the developers, "source" shows the code where the string is used (the line marked with ">"). Use them to pick the correct meaning and grammatical form, but never translate or copy them.
5. Keep roughly the same length as the original; UI space is limited.
6. Items with "plural" need {nplurals} plural forms: return an array of exactly {nplurals} strings, where index i is the form used when the gettext expression `{plural_expr}` evaluates to i.
7. Reply with ONLY a JSON object that maps every item "id" to its translation (a string, or an array for plural items). No markdown fences, no comments, no explanations.
"#
    ));
    p
}

fn build_items(
    ctx: &mut Ctx,
    batch: &[Work],
    retry: Option<&HashMap<usize, (Value, String)>>,
) -> Vec<Value> {
    let mut items = Vec::new();
    for (i, w) in batch.iter().enumerate() {
        if let Some(r) = retry {
            if !r.contains_key(&i) {
                continue;
            }
        }
        let mut o = Map::new();
        o.insert("id".into(), json!(i.to_string()));
        o.insert("text".into(), json!(w.msgid));
        if let Some(p) = &w.msgid_plural {
            o.insert("plural".into(), json!(p));
        }
        if let Some(c) = &w.msgctxt {
            o.insert("context".into(), json!(c));
        }
        let notes: Vec<&String> = w.notes.iter().filter(|n| !n.is_empty()).collect();
        if !notes.is_empty() {
            o.insert(
                "note".into(),
                json!(notes
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")),
            );
        }
        let fmt: Vec<&String> = w
            .flags
            .iter()
            .filter(|f| f.ends_with("-format") && !f.starts_with("no-"))
            .collect();
        if !fmt.is_empty() {
            o.insert("format".into(), json!(fmt));
        }
        if let Some(root) = ctx.settings.source_root.clone() {
            let mut snippets = Vec::new();
            for r in w.refs.iter().take(2) {
                if let Some(s) = ctx.sources.snippet(&root, r, ctx.settings.context_lines) {
                    snippets.push(format!("{}\n{}", r, s));
                }
            }
            if !snippets.is_empty() {
                o.insert("source".into(), json!(snippets));
            }
        }
        if let Some(r) = retry {
            let (prev, why) = &r[&i];
            o.insert("previous_translation".into(), prev.clone());
            o.insert("rejected_because".into(), json!(why));
        }
        items.push(Value::Object(o));
    }
    items
}

/// Extract the first JSON object from an LLM reply.
pub fn extract_json(reply: &str) -> Result<Map<String, Value>, String> {
    let t = reply.trim();
    let start = t.find('{').ok_or("reply contains no JSON object")?;
    let end = t.rfind('}').ok_or("reply contains no JSON object")?;
    if end <= start {
        return Err("reply contains no JSON object".into());
    }
    let v: Value = serde_json::from_str(&t[start..=end])
        .map_err(|e| format!("invalid JSON in reply: {}", e))?;
    match v {
        Value::Object(m) => {
            // Accept {"translations": {...}} wrappers.
            if m.len() == 1 {
                if let Some(Value::Object(inner)) = m.values().next() {
                    if inner.keys().all(|k| k.parse::<usize>().is_ok())
                        && !m.keys().all(|k| k.parse::<usize>().is_ok())
                    {
                        return Ok(inner.clone());
                    }
                }
            }
            Ok(m)
        }
        _ => Err("reply is not a JSON object".into()),
    }
}

fn value_forms(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(a) => a.iter().map(|x| x.as_str().map(String::from)).collect(),
        _ => None,
    }
}

type ItemResult = Result<Vec<String>, String>;

fn translate_llm(
    ctx: &mut Ctx,
    lang: &str,
    batch: &[Work],
    nplurals: usize,
    plural_expr: &str,
    glossary: &Glossary,
) -> Result<Vec<ItemResult>, String> {
    let system = system_prompt(ctx.settings, lang, nplurals, plural_expr);
    let texts: Vec<&str> = batch.iter().map(|w| w.msgid.as_str()).collect();
    let gl: Map<String, Value> = glossary
        .relevant(&texts, 40)
        .into_iter()
        .map(|(k, v)| (k, json!(v)))
        .collect();

    let mut results: Vec<Option<ItemResult>> = vec![None; batch.len()];
    let mut retry: Option<HashMap<usize, (Value, String)>> = None;
    let attempts = if ctx.settings.retry_rejected { 2 } else { 1 };
    for attempt in 0..attempts {
        if ctx.cancelled() {
            return Err("cancelled".into());
        }
        let items = build_items(ctx, batch, retry.as_ref());
        let mut payload = Map::new();
        if !gl.is_empty() {
            payload.insert("glossary".into(), Value::Object(gl.clone()));
        }
        payload.insert("items".into(), Value::Array(items));
        let mut user = String::new();
        if retry.is_some() {
            user.push_str("Some of your previous translations were rejected by automatic checks. Translate these items again, fixing the problem described in \"rejected_because\".\n\n");
        }
        user.push_str(&serde_json::to_string_pretty(&Value::Object(payload)).unwrap());
        let n_items = retry.as_ref().map(|r| r.len()).unwrap_or(batch.len());
        ctx.debug(
            format!(
                "{} · prompt for {} {}{}",
                langs::name(lang),
                n_items,
                if n_items == 1 { "string" } else { "strings" },
                if attempt > 0 {
                    " (second attempt for rejected strings)"
                } else {
                    ""
                }
            ),
            || {
                format!(
                    "SYSTEM PROMPT ({} characters)\n\n{}\n\nUSER PROMPT ({} characters)\n\n{}",
                    system.len(),
                    system,
                    user.len(),
                    user
                )
            },
        );

        // One extra try for transport / parse errors on the first attempt.
        let mut reply = None;
        let mut last_err = String::new();
        for try_no in 0..2 {
            let t0 = Instant::now();
            let r = providers::complete(ctx.provider, &system, &user, ctx.cancel);
            if ctx.cancelled() {
                return Err("cancelled".into());
            }
            let r = r.and_then(|raw| {
                ctx.debug(
                    format!(
                        "Model reply ({:.1}s, {} characters)",
                        t0.elapsed().as_secs_f32(),
                        raw.len()
                    ),
                    || raw.clone(),
                );
                extract_json(&raw)
            });
            match r {
                Ok(m) => {
                    ctx.debug("Reply parsed", || {
                        format!(
                            "Found {} answers with ids: {}",
                            m.len(),
                            m.keys().cloned().collect::<Vec<_>>().join(", ")
                        )
                    });
                    reply = Some(m);
                    break;
                }
                Err(e) => {
                    ctx.debug(
                        if try_no == 0 {
                            "Request couldn't be used; trying once more"
                        } else {
                            "Request couldn't be used"
                        },
                        || e.clone(),
                    );
                    last_err = e;
                }
            }
        }
        let Some(map) = reply else {
            if attempt == 0 {
                return Err(last_err);
            }
            // keep earlier rejections
            break;
        };

        let mut next_retry = HashMap::new();
        let mut processed = Vec::new();
        for (i, w) in batch.iter().enumerate() {
            if let Some(r) = &retry {
                if !r.contains_key(&i) {
                    continue;
                }
            }
            processed.push(i);
            let res = match map.get(&i.to_string()) {
                None => Err("missing from the model's reply".to_string()),
                Some(v) => match value_forms(v) {
                    None => Err("reply value is not a string or array of strings".to_string()),
                    Some(mut forms) => {
                        if w.msgid_plural.is_none()
                            && forms.len() > 1
                            && forms.iter().all(|f| f == &forms[0])
                        {
                            forms.truncate(1);
                        }
                        let src = Src {
                            msgid: &w.msgid,
                            msgid_plural: w.msgid_plural.as_deref(),
                            flags: &w.flags,
                            lang,
                            nplurals,
                        };
                        validate::check(&src, &forms)
                    }
                },
            };
            if let Err(why) = &res {
                next_retry.insert(
                    i,
                    (
                        map.get(&i.to_string()).cloned().unwrap_or(Value::Null),
                        why.clone(),
                    ),
                );
            }
            results[i] = Some(res);
        }
        misalignment_check(batch, &mut results, &mut next_retry);
        ctx.debug(
            format!(
                "Checks: {} accepted, {} rejected",
                processed
                    .iter()
                    .filter(|&&i| matches!(results[i], Some(Ok(_))))
                    .count(),
                processed
                    .iter()
                    .filter(|&&i| matches!(results[i], Some(Err(_))))
                    .count()
            ),
            || {
                processed
                    .iter()
                    .map(|&i| {
                        let raw = match map.get(&i.to_string()) {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => v.to_string(),
                            None => "(missing)".into(),
                        };
                        let head = format!("[{}] {}", i, short(&batch[i].msgid, 90));
                        match &results[i] {
                            Some(Ok(forms)) => {
                                let fin = forms.join(" | ");
                                let raw_flat =
                                    value_forms(map.get(&i.to_string()).unwrap_or(&Value::Null))
                                        .map(|f| f.join(" | "))
                                        .unwrap_or(raw);
                                if fin == raw_flat {
                                    format!("{}\n    ✔ {}", head, short(&fin, 200))
                                } else {
                                    format!(
                                        "{}\n    ✔ {}\n      repaired from: {}",
                                        head,
                                        short(&fin, 200),
                                        short(&raw_flat, 200)
                                    )
                                }
                            }
                            Some(Err(why)) => format!(
                                "{}\n    ✖ {}\n      reply: {}",
                                head,
                                why,
                                short(&raw, 200)
                            ),
                            None => format!("{}\n    (no result)", head),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        );
        if next_retry.is_empty() {
            break;
        }
        if attempt + 1 < attempts {
            ctx.debug(
                format!("Asking again for {} rejected strings", next_retry.len()),
                || {
                    next_retry
                        .iter()
                        .map(|(i, (_, why))| format!("[{}] {}", i, why))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            );
        }
        retry = Some(next_retry);
    }
    Ok(results
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("no translation returned".into())))
        .collect())
}

/// If two different (multi-word) sources get the exact same translation,
/// the model most likely shifted its answers: reject both.
fn misalignment_check(
    batch: &[Work],
    results: &mut [Option<ItemResult>],
    retry: &mut HashMap<usize, (Value, String)>,
) {
    let norm = |s: &str| {
        s.trim()
            .trim_end_matches(['.', ':', '!', '?'])
            .to_lowercase()
    };
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut bad = Vec::new();
    for (i, r) in results.iter().enumerate() {
        if let Some(Ok(forms)) = r {
            if batch[i].msgid.split_whitespace().count() < 3 {
                continue;
            }
            let key = forms.join("\u{1}").trim().to_string();
            if let Some(&j) = seen.get(&key) {
                if norm(&batch[i].msgid) != norm(&batch[j].msgid) {
                    bad.push(i);
                    bad.push(j);
                }
            } else {
                seen.insert(key, i);
            }
        }
    }
    for i in bad {
        let prev = match &results[i] {
            Some(Ok(f)) => json!(f.join(" | ")),
            _ => Value::Null,
        };
        let why = "the same translation was returned for different source texts (answers may be misaligned)".to_string();
        results[i] = Some(Err(why.clone()));
        retry.insert(i, (prev, why));
    }
}

// ---------------------------------------------------------------------------
// LibreTranslate
// ---------------------------------------------------------------------------

fn translate_lt(
    ctx: &Ctx,
    lang: &str,
    lt_code: &str,
    batch: &[Work],
    nplurals: usize,
) -> Result<Vec<ItemResult>, String> {
    // Collect unique texts, split into HTML and plain-text requests.
    let mut texts: Vec<String> = Vec::new();
    for w in batch {
        texts.push(w.msgid.clone());
        if let Some(p) = &w.msgid_plural {
            texts.push(p.clone());
        }
    }
    texts.sort();
    texts.dedup();
    // Texts with markup or placeholders are sent as HTML, with placeholders
    // wrapped in <span translate="no"> so the engine leaves them alone.
    let flags_of: HashMap<&str, &Vec<String>> = batch
        .iter()
        .flat_map(|w| {
            std::iter::once(w.msgid.as_str())
                .chain(w.msgid_plural.as_deref())
                .map(move |t| (t, &w.flags))
        })
        .collect();
    let needs_html = |s: &str| {
        !validate::placeholders(s, flags_of.get(s).map(|f| f.as_slice()).unwrap_or(&[])).is_empty()
    };
    let (html, plain): (Vec<String>, Vec<String>) = texts.into_iter().partition(|t| needs_html(t));
    let mut map: HashMap<String, String> = HashMap::new();
    if !plain.is_empty() {
        ctx.debug(
            format!(
                "{} · {} plain-text {}",
                langs::name(lang),
                plain.len(),
                if plain.len() == 1 {
                    "string"
                } else {
                    "strings"
                }
            ),
            || plain.join("\n"),
        );
        let out = providers::libretranslate(
            ctx.provider,
            &plain,
            &ctx.settings.source_lang,
            lt_code,
            false,
            ctx.cancel,
        )?;
        for (s, t) in plain.into_iter().zip(out) {
            map.insert(s, t);
        }
    }
    if !html.is_empty() {
        let protected: Vec<String> = html
            .iter()
            .map(|t| {
                protect(
                    t,
                    flags_of
                        .get(t.as_str())
                        .map(|f| f.as_slice())
                        .unwrap_or(&[]),
                )
            })
            .collect();
        ctx.debug(
            format!(
                "{} · {} {} with placeholders or markup (sent as HTML)",
                langs::name(lang),
                html.len(),
                if html.len() == 1 { "string" } else { "strings" }
            ),
            || {
                html.iter()
                    .zip(&protected)
                    .map(|(s, p)| format!("{}\n    sent as: {}", s, p))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        );
        let out = providers::libretranslate(
            ctx.provider,
            &protected,
            &ctx.settings.source_lang,
            lt_code,
            true,
            ctx.cancel,
        )?;
        let mut restored = Vec::new();
        for (s, t) in html.into_iter().zip(out) {
            let had_tags = validate::placeholders(&s, &[])
                .iter()
                .any(|x| x.starts_with("tag:"));
            let u = unprotect(&t, had_tags);
            restored.push(format!("{}\n    received: {}\n    restored: {}", s, t, u));
            map.insert(s, u);
        }
        ctx.debug("Placeholders restored", || restored.join("\n"));
    }
    let results: Vec<ItemResult> = batch
        .iter()
        .map(|w| {
            let one = map.get(&w.msgid).cloned().unwrap_or_default();
            let forms = match &w.msgid_plural {
                None => vec![one],
                Some(p) => {
                    let many = map.get(p).cloned().unwrap_or_default();
                    if nplurals == 1 {
                        vec![many]
                    } else {
                        let mut v = vec![one];
                        v.extend(std::iter::repeat_n(many, nplurals - 1));
                        v
                    }
                }
            };
            let src = Src {
                msgid: &w.msgid,
                msgid_plural: w.msgid_plural.as_deref(),
                flags: &w.flags,
                lang,
                nplurals,
            };
            validate::check(&src, &forms)
                .map_err(|e| (e, forms.join(" | ")))
                .map_err(|(e, raw)| format!("{}\u{0}{}", e, raw))
        })
        .collect();
    ctx.debug(
        format!(
            "Checks: {} accepted, {} rejected",
            results.iter().filter(|r| r.is_ok()).count(),
            results.iter().filter(|r| r.is_err()).count()
        ),
        || {
            batch
                .iter()
                .zip(&results)
                .enumerate()
                .map(|(i, (w, r))| {
                    let raw = map.get(&w.msgid).cloned().unwrap_or_default();
                    let head = format!("[{}] {}", i, short(&w.msgid, 90));
                    match r {
                        Ok(forms) => {
                            let fin = forms.join(" | ");
                            if w.msgid_plural.is_none() && fin != raw {
                                format!(
                                    "{}\n    ✔ {}\n      repaired from: {}",
                                    head,
                                    short(&fin, 200),
                                    short(&raw, 200)
                                )
                            } else {
                                format!("{}\n    ✔ {}", head, short(&fin, 200))
                            }
                        }
                        Err(e) => {
                            let (why, got) = e.split_once('\u{0}').unwrap_or((e.as_str(), ""));
                            format!("{}\n    ✖ {}\n      reply: {}", head, why, short(got, 200))
                        }
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        },
    );
    Ok(results
        .into_iter()
        .map(|r| r.map_err(|e| e.split('\u{0}').next().unwrap_or("").to_string()))
        .collect())
}

const NO_OPEN: &str = "<span translate=\"no\">";
const NO_CLOSE: &str = "</span>";

/// Prepare a string for LibreTranslate's HTML mode: escape plain text and
/// wrap placeholders / code identifiers in non-translatable spans.
pub fn protect(text: &str, flags: &[String]) -> String {
    let has_tags = validate::placeholders(text, &[])
        .iter()
        .any(|t| t.starts_with("tag:"));
    let mut tokens: Vec<String> = validate::placeholders(text, flags)
        .into_iter()
        .filter(|t| t.starts_with("ph:") || t.starts_with("code:") || t.starts_with("url:"))
        .map(|t| t.split_once(':').unwrap().1.to_string())
        .filter(|t| text.contains(t.as_str()))
        .collect();
    tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));
    tokens.dedup();
    // Replace with private-use sentinels first so tokens never overlap.
    let mut s = text.to_string();
    for (i, t) in tokens.iter().enumerate() {
        s = s.replace(t.as_str(), &format!("\u{E000}{}\u{E001}", i));
    }
    if !has_tags {
        s = s
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
    }
    for (i, t) in tokens.iter().enumerate() {
        let tok = if has_tags {
            t.clone()
        } else {
            t.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        s = s.replace(
            &format!("\u{E000}{}\u{E001}", i),
            &format!("{}{}{}", NO_OPEN, tok, NO_CLOSE),
        );
    }
    s
}

pub fn unprotect(text: &str, had_tags: bool) -> String {
    let mut s = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(NO_OPEN) {
        s.push_str(&rest[..i]);
        let after = &rest[i + NO_OPEN.len()..];
        match after.find(NO_CLOSE) {
            Some(j) => {
                s.push_str(&after[..j]);
                rest = &after[j + NO_CLOSE.len()..];
            }
            None => {
                rest = after;
            }
        }
    }
    s.push_str(rest);
    if !had_tags {
        s = s
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skipped_entries_are_not_translated_again() {
        let dir = std::env::temp_dir().join(format!("polibre-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("django.po");
        std::fs::write(&path, "msgid \"\"\nmsgstr \"\"\n\nmsgid \"Save\"\nmsgstr \"\"\n\nmsgid \"Open\"\nmsgstr \"\"\n").unwrap();
        let (url, seen) = mock_server(|path, body| match path {
            "/languages" => json!([{"code": "en", "targets": ["it"]}, {"code": "it", "targets": ["en"]}]).to_string(),
            _ => {
                let q: Value = serde_json::from_str(body).unwrap();
                json!({"translatedText": q["q"].as_array().unwrap().iter().map(|x| format!("it {}", x.as_str().unwrap())).collect::<Vec<_>>()}).to_string()
            }
        });
        let provider = ProviderConfig { kind: ProviderKind::Libretranslate, model: String::new(), api_key: String::new(), url };
        let mut settings = JobSettings { dry_run: true, ..Default::default() };
        settings.skip.insert((path.clone(), None, "Save".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = run(provider, settings, vec![Target { lang: "it".into(), files: vec![path.clone()] }], tx, cancel);
        assert_eq!(s.translated, 1);
        let got: Vec<String> = rx.try_iter().filter_map(|e| match e { Event::Translated { msgid, .. } => Some(msgid), _ => None }).collect();
        assert_eq!(got, vec!["Open"]);
        assert!(seen.lock().unwrap().iter().all(|b| !b.contains("Save")), "Save wasn't sent");
        assert!(std::fs::read_to_string(&path).unwrap().contains("msgid \"Open\"\nmsgstr \"\""), "a preview writes nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plural_forms_are_labelled() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(show_forms(&f(&["Ciao"]), "\n"), "Ciao");
        assert_eq!(
            show_forms(&f(&["Correggi l'errore qui sotto.", "Correggi gli errori qui sotto."]), "\n"),
            "Singular: Correggi l'errore qui sotto.\nPlural: Correggi gli errori qui sotto."
        );
        assert_eq!(show_forms(&f(&["a", "b", "c"]), " · "), "Form 1: a · Form 2: b · Form 3: c");
    }

    #[test]
    fn protect_roundtrip() {
        let f = vec!["python-format".to_string()];
        let p = protect("Cannot convert %(file)s: %(reason)s & more", &f);
        assert_eq!(p, "Cannot convert <span translate=\"no\">%(file)s</span>: <span translate=\"no\">%(reason)s</span> &amp; more");
        assert_eq!(
            unprotect(&p, false),
            "Cannot convert %(file)s: %(reason)s & more"
        );
        let p = protect("Click <b>%(n)s</b>", &f);
        assert_eq!(unprotect(&p, true), "Click <b>%(n)s</b>");
    }
    #[test]
    fn json_extraction() {
        let m = extract_json("```json\n{\"0\": \"Ciao\", \"1\": [\"a\", \"b\"]}\n```").unwrap();
        assert_eq!(m["0"], "Ciao");
        let m = extract_json("{\"translations\": {\"0\": \"x\"}}").unwrap();
        assert_eq!(m["0"], "x");
        assert!(extract_json("no json").is_err());
    }

    /// A one-shot local HTTP server: answers each request with `reply(path, body)`
    /// and records the request bodies. Returns its base URL.
    fn mock_server(
        reply: fn(&str, &str) -> String,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                let out = reply(&path, &body);
                seen2.lock().unwrap().push(body);
                let mut w = stream;
                let _ = write!(
                    w,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    out.len(),
                    out
                );
            }
        });
        (url, seen)
    }

    fn entry(msgid: &str) -> Entry {
        Entry {
            msgid: msgid.into(),
            msgstr: vec![String::new()],
            flags: vec!["fuzzy".into(), "python-format".into()],
            ..Default::default()
        }
    }

    #[test]
    fn suggest_with_an_llm_service() {
        let (url, seen) = mock_server(|path, _| {
            assert_eq!(path, "/chat/completions");
            json!({"choices": [{"message": {"content": "{\"0\": \"Ciao %(name)s\"}"}}]}).to_string()
        });
        let provider = ProviderConfig {
            kind: ProviderKind::OpenaiCompatible,
            model: "test".into(),
            api_key: String::new(),
            url,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = suggest(
            &provider,
            &JobSettings::default(),
            "it",
            &[],
            &entry("Hello %(name)s"),
            2,
            "n != 1",
            &tx,
            &cancel,
        );
        assert_eq!(out, Ok(vec!["Ciao %(name)s".to_string()]));
        // The request carries the string and its format.
        let body = seen.lock().unwrap()[0].clone();
        assert!(
            body.contains("Hello %(name)s") && body.contains("python-format"),
            "{}",
            body
        );
    }

    #[test]
    fn suggest_rejects_a_broken_translation() {
        // The model drops the placeholder twice (first answer and the retry).
        let (url, _) = mock_server(|_, _| {
            json!({"choices": [{"message": {"content": "{\"0\": \"Ciao\"}"}}]}).to_string()
        });
        let provider = ProviderConfig {
            kind: ProviderKind::OpenaiCompatible,
            model: "test".into(),
            api_key: String::new(),
            url,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = suggest(
            &provider,
            &JobSettings::default(),
            "it",
            &[],
            &entry("Hello %(name)s"),
            2,
            "",
            &tx,
            &cancel,
        );
        assert!(out.is_err_and(|e| e.contains("%(name)s")));
    }

    #[test]
    fn a_slow_suggestion_can_be_cancelled() {
        use std::io::Read;
        // A model that takes 10 s to answer.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 8192];
                let _ = s.read(&mut buf);
                std::thread::sleep(Duration::from_secs(10));
            }
        });
        let provider = ProviderConfig {
            kind: ProviderKind::OpenaiCompatible,
            model: "test".into(),
            api_key: String::new(),
            url,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c2.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let t0 = Instant::now();
        let out = suggest(&provider, &JobSettings::default(), "it", &[], &entry("Hello %(name)s"), 2, "", &tx, &cancel);
        assert_eq!(out, Err("cancelled".to_string()));
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn suggest_with_libretranslate() {
        let (url, _) = mock_server(|path, body| match path {
            "/languages" => {
                json!([{"code": "en", "targets": ["it"]}, {"code": "it", "targets": ["en"]}])
                    .to_string()
            }
            _ => {
                // Placeholders arrive protected; translate around them.
                let q: Value = serde_json::from_str(body).unwrap();
                let t = q["q"][0].as_str().unwrap().replace("Hello", "Ciao");
                json!({"translatedText": [t]}).to_string()
            }
        });
        let provider = ProviderConfig {
            kind: ProviderKind::Libretranslate,
            model: String::new(),
            api_key: String::new(),
            url,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = suggest(
            &provider,
            &JobSettings::default(),
            "it",
            &[],
            &entry("Hello %(name)s"),
            2,
            "",
            &tx,
            &cancel,
        );
        assert_eq!(out, Ok(vec!["Ciao %(name)s".to_string()]));
    }
}
