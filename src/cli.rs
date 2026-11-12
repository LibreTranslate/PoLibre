// SPDX-License-Identifier: AGPL-3.0-or-later
//! Command line interface. Everything the GUI can do is available here, and
//! every option can also be given through an environment variable so the
//! tool works unattended (e.g. in a GitHub Actions workflow).

use crate::config::{Config, ProviderConfig, ProviderKind, ProviderSettings};
use crate::engine::{self, Event, JobSettings, Summary, Target};
use crate::langs;
use crate::project;
use crate::providers::{self, LtLanguages};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

const HELP: &str = r##"PoLibre - fill untranslated gettext .po strings with LLMs or LibreTranslate

USAGE:
    polibre                            Open the graphical interface
    polibre gui [PATH]                 Open the graphical interface on PATH
    polibre scan [PATH]                List languages, files and untranslated counts
    polibre translate [PATH] [OPTS]    Translate untranslated entries
    polibre providers                  Show detected / available providers
    polibre models [OPTS]              List models offered by a provider
    polibre config [show|path] [OPTS]  Show or save default settings
    polibre check [PATH] [-l LANGS]    Run the validators on existing translations
                                       (--only-auto: only PoLibre entries)
    polibre prompt [PATH] -l LANG      Print the LLM prompt for the first batch (debug)

PATH defaults to the current directory; it can be a project folder, a locale
folder or a single .po file.

PROVIDER OPTIONS (also for `config` and `models`):
    -p, --provider ID       claude-cli | gemini-cli | codex-cli | anthropic | google |
                            openai | openai-compatible | libretranslate
                            (default: saved config, else first detected)  [POLIBRE_PROVIDER]
    -m, --model ID          Model id, e.g. claude-opus-5-5, opus, gemini-2.5-pro, gpt-5
                                                                           [POLIBRE_MODEL]
    -k, --api-key KEY       API key                                        [POLIBRE_API_KEY]
        --api-key-file F    Read the API key from a file
        --url URL           LibreTranslate / OpenAI-compatible base URL    [POLIBRE_URL]
    Provider keys are also read from ANTHROPIC_API_KEY, GEMINI_API_KEY, GOOGLE_API_KEY,
    OPENAI_API_KEY, LIBRETRANSLATE_API_KEY.

TRANSLATE OPTIONS:
    -l, --lang LIST         Languages to translate, comma separated, or "all"
                            (default: all)                                 [POLIBRE_LANGS]
        --source-lang CODE  Language of the msgids (default: en)           [POLIBRE_SOURCE_LANG]
        --source-root DIR   Folder #: references are relative to (default: auto-detect)
        --context TEXT      Short description of the software (default: from README)
        --context-file F    Read the description from a file
        --context-lines N   Source lines shown around each reference (default: 2)
        --glossary FILE     Extra glossary: CSV/TSV "source,translation" or
                            "lang,source,translation", or a .po file
        --batch-size N      Entries per request (default: 20 LLM, 25 LibreTranslate)
        --limit N           Translate at most N entries in this run
        --include-fuzzy     Also re-translate entries already marked fuzzy
        --no-comment        Don't add the "# Translated by <model>" comment
        --no-fuzzy          Don't mark translations fuzzy (they won't need review)
        --no-retry          Don't re-ask the LLM about translations that fail validation
        --debug             Print every request, model reply and check to stderr [POLIBRE_DEBUG]
        --dry-run           Translate and show results, but don't write files
        --report FILE       Write a JSON report of all translated/rejected entries
        --fail-on-reject    Exit with code 3 if any entry was rejected or failed
        --save              Save provider/model/key/url as defaults
        --no-config         Ignore saved settings (recommended in CI)
        --progress MODE     auto | bar | plain | quiet (default: auto)
    -y, --yes               Accepted for compatibility; never prompts anyway

Machine translations are marked "#, fuzzy" (so they are not used until a
human reviews them; --no-fuzzy turns this off) and get a
"# Translated by <model>" comment.

EXIT CODES: 0 success, 1 error, 2 bad usage, 3 rejected entries (--fail-on-reject),
            130 interrupted
"##;

#[derive(Default, Debug)]
struct Opts {
    positional: Vec<String>,
    provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    url: Option<String>,
    langs: Option<String>,
    source_lang: Option<String>,
    source_root: Option<String>,
    context: Option<String>,
    context_lines: Option<usize>,
    glossary: Option<String>,
    batch_size: Option<usize>,
    limit: Option<usize>,
    include_fuzzy: bool,
    no_comment: bool,
    no_fuzzy: bool,
    no_retry: bool,
    debug: bool,
    dry_run: bool,
    report: Option<String>,
    fail_on_reject: bool,
    save: bool,
    no_config: bool,
    progress: Option<String>,
    json: bool,
    only_auto: bool,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let mut val = |name: &str| -> Result<String, String> {
            if let Some(v) = &inline {
                return Ok(v.clone());
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", name))
        };
        let num = |s: String, name: &str| -> Result<usize, String> {
            s.parse().map_err(|_| format!("{} expects a number", name))
        };
        match flag.as_str() {
            "-p" | "--provider" => o.provider = Some(val(&flag)?),
            "-m" | "--model" => o.model = Some(val(&flag)?),
            "-k" | "--api-key" => o.api_key = Some(val(&flag)?),
            "--api-key-file" => {
                let f = val(&flag)?;
                o.api_key = Some(
                    std::fs::read_to_string(&f)
                        .map_err(|e| format!("{}: {}", f, e))?
                        .trim()
                        .to_string(),
                );
            }
            "--url" => o.url = Some(val(&flag)?),
            "-l" | "--lang" | "--langs" | "--languages" => o.langs = Some(val(&flag)?),
            "--source-lang" => o.source_lang = Some(val(&flag)?),
            "--source-root" => o.source_root = Some(val(&flag)?),
            "--context" => o.context = Some(val(&flag)?),
            "--context-file" => {
                let f = val(&flag)?;
                o.context = Some(
                    std::fs::read_to_string(&f)
                        .map_err(|e| format!("{}: {}", f, e))?
                        .trim()
                        .to_string(),
                );
            }
            "--context-lines" => o.context_lines = Some(num(val(&flag)?, &flag)?),
            "--glossary" => o.glossary = Some(val(&flag)?),
            "--batch-size" => o.batch_size = Some(num(val(&flag)?, &flag)?),
            "--limit" => o.limit = Some(num(val(&flag)?, &flag)?),
            "--include-fuzzy" => o.include_fuzzy = true,
            "--no-comment" => o.no_comment = true,
            "--no-fuzzy" => o.no_fuzzy = true,
            "--no-retry" => o.no_retry = true,
            "--debug" => o.debug = true,
            "--dry-run" | "-n" => o.dry_run = true,
            "--report" => o.report = Some(val(&flag)?),
            "--fail-on-reject" => o.fail_on_reject = true,
            "--save" => o.save = true,
            "--no-config" => o.no_config = true,
            "--progress" => o.progress = Some(val(&flag)?),
            "-q" | "--quiet" => o.progress = Some("quiet".into()),
            "--json" => o.json = true,
            "--only-auto" => o.only_auto = true,
            "-y" | "--yes" => {}
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {}", s)),
            _ => o.positional.push(a.clone()),
        }
        i += 1;
    }
    // Environment fallbacks.
    o.provider = o.provider.or_else(|| env("POLIBRE_PROVIDER"));
    o.model = o.model.or_else(|| env("POLIBRE_MODEL"));
    o.url = o.url.or_else(|| env("POLIBRE_URL"));
    o.langs = o.langs.or_else(|| env("POLIBRE_LANGS"));
    o.source_lang = o.source_lang.or_else(|| env("POLIBRE_SOURCE_LANG"));
    Ok(o)
}

pub fn main(args: Vec<String>) -> i32 {
    if args
        .iter()
        .any(|a| a == "-h" || a == "--help" || a == "help")
        || args.is_empty()
    {
        print!("{}", HELP);
        return 0;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("polibre {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let (cmd, rest) = match args[0].as_str() {
        "scan" | "translate" | "providers" | "models" | "config" | "prompt" | "check" => {
            (args[0].as_str(), &args[1..])
        }
        // `polibre PATH --lang it` is shorthand for translate
        _ => ("translate", &args[..]),
    };
    let opts = match parse(rest) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {}\nRun `polibre --help` for usage.", e);
            return 2;
        }
    };
    let res = match cmd {
        "scan" => cmd_scan(&opts),
        "providers" => cmd_providers(&opts),
        "models" => cmd_models(&opts),
        "config" => cmd_config(&opts),
        "prompt" => cmd_prompt(&opts),
        "check" => cmd_check(&opts),
        _ => cmd_translate(&opts),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}error:{} {}", c(RED), c(RESET), e);
            1
        }
    }
}

// ---------------------------------------------------------------------------
// styling helpers
// ---------------------------------------------------------------------------

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}
fn c(code: &str) -> &str {
    if color_enabled() {
        code
    } else {
        ""
    }
}

fn trunc(s: &str, n: usize) -> String {
    let one: String = s.replace('\n', "⏎").replace('\t', " ");
    if one.chars().count() <= n {
        one
    } else {
        let mut t: String = one.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
        .clamp(40, 200)
}

fn bar(done: usize, total: usize, width: usize) -> String {
    let frac = if total == 0 {
        1.0
    } else {
        done as f64 / total as f64
    };
    let full = (frac * width as f64).round() as usize;
    format!(
        "{}{}",
        "█".repeat(full.min(width)),
        "░".repeat(width - full.min(width))
    )
}

// ---------------------------------------------------------------------------
// provider resolution
// ---------------------------------------------------------------------------

fn load_config(o: &Opts) -> Config {
    if o.no_config || env("POLIBRE_NO_CONFIG").is_some() {
        Config::default()
    } else {
        Config::load()
    }
}

fn resolve_provider(o: &Opts, cfg: &Config) -> Result<ProviderConfig, String> {
    let kind = match &o.provider {
        Some(p) => ProviderKind::from_id(p).ok_or_else(|| {
            format!(
                "unknown provider '{}'. Valid: {}",
                p,
                ProviderKind::ALL
                    .iter()
                    .map(|k| k.id())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?,
        None => match cfg.provider {
            Some(k) => k,
            None => providers::detect().first().map(|d| d.kind).ok_or(
                "no provider configured or detected; use --provider (see `polibre providers`)",
            )?,
        },
    };
    let mut s: ProviderSettings = cfg.provider_settings(kind);
    if let Some(m) = &o.model {
        s.model = m.clone();
    }
    if let Some(k) = &o.api_key {
        s.api_key = k.clone();
    }
    if let Some(u) = &o.url {
        s.url = u.clone();
    }
    Ok(ProviderConfig::resolve(kind, &s))
}

fn save_provider(o: &Opts, cfg: &mut Config, p: &ProviderConfig) -> Result<PathBuf, String> {
    cfg.provider = Some(p.kind);
    let e = cfg.providers.entry(p.kind).or_default();
    if let Some(m) = &o.model {
        e.model = m.clone();
    }
    if let Some(k) = &o.api_key {
        e.api_key = k.clone();
    }
    if let Some(u) = &o.url {
        e.url = u.clone();
    }
    if let Some(sl) = &o.source_lang {
        cfg.source_lang = sl.clone();
    }
    if let Some(b) = o.batch_size {
        cfg.batch_size = b;
    }
    if o.no_comment {
        cfg.add_comment = false;
    }
    if o.no_fuzzy {
        cfg.mark_fuzzy = false;
    }
    if o.include_fuzzy {
        cfg.include_fuzzy = true;
    }
    cfg.save().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

fn project_path(o: &Opts) -> PathBuf {
    PathBuf::from(o.positional.first().cloned().unwrap_or_else(|| ".".into()))
}

fn cmd_providers(o: &Opts) -> Result<i32, String> {
    let det = providers::detect();
    let cfg = load_config(o);
    if o.json {
        let v: Vec<serde_json::Value> = ProviderKind::ALL
            .iter()
            .map(|k| {
                let d = det.iter().find(|d| d.kind == *k);
                serde_json::json!({"id": k.id(), "label": k.label(), "detected": d.is_some(),
                    "how": d.map(|d| d.how.clone()), "models": k.suggested_models()})
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(0);
    }
    println!("{}Providers{}", c(BOLD), c(RESET));
    for k in ProviderKind::ALL {
        let d = det.iter().find(|d| d.kind == k);
        let mark = if d.is_some() {
            format!("{}✓{}", c(GREEN), c(RESET))
        } else {
            " ".into()
        };
        let default = if cfg.provider == Some(k) {
            format!(" {}(default){}", c(CYAN), c(RESET))
        } else {
            String::new()
        };
        println!(
            "  {} {:<18} {}{}{}",
            mark,
            k.id(),
            k.label(),
            default,
            d.map(|d| format!("  {}{}{}", c(DIM), d.how, c(RESET)))
                .unwrap_or_default()
        );
    }
    println!("\nSuggested models: claude-cli: opus|sonnet|haiku, anthropic: claude-opus-5-5, ...");
    println!("Run `polibre models -p <provider>` to list models from the provider.");
    Ok(0)
}

fn cmd_models(o: &Opts) -> Result<i32, String> {
    let cfg = load_config(o);
    let p = resolve_provider(o, &cfg)?;
    let models = providers::list_models(&p)?;
    for m in models {
        println!(
            "{}",
            if m.is_empty() {
                "(provider default)".to_string()
            } else {
                m
            }
        );
    }
    Ok(0)
}

fn cmd_config(o: &Opts) -> Result<i32, String> {
    let mut cfg = Config::load();
    let sub = o.positional.first().map(|s| s.as_str()).unwrap_or("show");
    if sub == "path" {
        println!(
            "{}",
            crate::config::config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        return Ok(0);
    }
    let changing = o.provider.is_some()
        || o.model.is_some()
        || o.api_key.is_some()
        || o.url.is_some()
        || o.source_lang.is_some()
        || o.batch_size.is_some()
        || o.no_comment
        || o.no_fuzzy
        || o.include_fuzzy;
    if sub == "set" || changing {
        let p = resolve_provider(o, &cfg)?;
        let path = save_provider(o, &mut cfg, &p)?;
        println!("Saved settings to {}", path.display());
    }
    let mut shown = cfg.clone();
    for s in shown.providers.values_mut() {
        if s.api_key.len() > 8 {
            s.api_key = format!("{}…{}", &s.api_key[..4], &s.api_key[s.api_key.len() - 2..]);
        }
    }
    println!("{}", serde_json::to_string_pretty(&shown).unwrap());
    Ok(0)
}

fn cmd_scan(o: &Opts) -> Result<i32, String> {
    let root = project_path(o);
    let proj = project::scan(&root, o.include_fuzzy)?;
    let cfg = load_config(o);
    let lt = if o.provider.as_deref().and_then(ProviderKind::from_id)
        == Some(ProviderKind::Libretranslate)
    {
        let p = resolve_provider(o, &cfg)?;
        Some(
            LtLanguages::fetch(&p.url, o.source_lang.as_deref().unwrap_or("en"))
                .unwrap_or_else(|_| LtLanguages::fallback()),
        )
    } else {
        None
    };
    if o.json {
        let langs: Vec<serde_json::Value> = proj
            .languages
            .values()
            .map(|l| {
                serde_json::json!({
                    "code": l.code, "name": l.name, "untranslated": l.untranslated(), "fuzzy": l.fuzzy(), "total": l.total(),
                    "libretranslate": lt.as_ref().map(|lt| lt.map(&l.code)),
                    "files": l.files.iter().map(|f| serde_json::json!({"path": f.path, "untranslated": f.untranslated, "fuzzy": f.fuzzy, "total": f.total})).collect::<Vec<_>>()
                })
            })
            .collect();
        let v = serde_json::json!({"root": proj.root, "source_root": proj.source_root, "name": proj.name, "summary": proj.summary, "languages": langs});
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(0);
    }
    println!(
        "{}Project{}      {}",
        c(BOLD),
        c(RESET),
        proj.root.display()
    );
    println!(
        "{}Source root{}  {}",
        c(BOLD),
        c(RESET),
        proj.source_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(not found - no code context)".into())
    );
    println!(
        "{}Summary{}      {}\n",
        c(BOLD),
        c(RESET),
        trunc(&proj.summary, 200)
    );
    if proj.languages.is_empty() {
        println!("No .po files found.");
        return Ok(0);
    }
    println!(
        "  {:<10} {:<28} {:>12} {:>7} {:>7}  {}",
        "CODE",
        "LANGUAGE",
        "UNTRANSLATED",
        "FUZZY",
        "TOTAL",
        if lt.is_some() { "LIBRETRANSLATE" } else { "" }
    );
    for l in proj.languages.values() {
        let u = l.untranslated();
        let ucol = if u == 0 { GREEN } else { YELLOW };
        let ltcol = match &lt {
            Some(lt) => match lt.map(&l.code) {
                Some(c2) => format!("{}✓ {}{}", c(GREEN), c2, c(RESET)),
                None => format!("{}✗ unsupported{}", c(RED), c(RESET)),
            },
            None => String::new(),
        };
        println!(
            "  {:<10} {:<28} {}{:>12}{} {:>7} {:>7}  {}",
            l.code,
            trunc(&l.name, 28),
            c(ucol),
            u,
            c(RESET),
            l.fuzzy(),
            l.total(),
            ltcol
        );
    }
    Ok(0)
}

struct Prepared {
    provider: ProviderConfig,
    settings: JobSettings,
    targets: Vec<Target>,
    project: project::Project,
}

fn prepare(o: &Opts) -> Result<Prepared, String> {
    let mut cfg = load_config(o);
    let provider = resolve_provider(o, &cfg)?;
    if o.save {
        let p = save_provider(o, &mut cfg, &provider)?;
        eprintln!("Saved settings to {}", p.display());
    }
    let include_fuzzy = o.include_fuzzy || cfg.include_fuzzy;
    let root = project_path(o);
    let proj = project::scan(&root, include_fuzzy)?;
    if proj.languages.is_empty() {
        return Err(format!("no .po files found in {}", root.display()));
    }
    let wanted: Option<Vec<String>> = match o.langs.as_deref() {
        None | Some("all") | Some("") => None,
        Some(l) => Some(
            l.split([',', ' '])
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
        ),
    };
    let mut targets = Vec::new();
    if let Some(w) = &wanted {
        for code in w {
            let found = proj
                .languages
                .keys()
                .find(|k| langs::normalize(k).eq_ignore_ascii_case(&langs::normalize(code)));
            match found {
                Some(k) => targets.push(Target {
                    lang: k.clone(),
                    files: proj.languages[k]
                        .files
                        .iter()
                        .map(|f| f.path.clone())
                        .collect(),
                }),
                None => {
                    return Err(format!(
                        "language '{}' not found in project. Available: {}",
                        code,
                        proj.languages
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            }
        }
    } else {
        for (k, l) in &proj.languages {
            if l.untranslated() > 0 {
                targets.push(Target {
                    lang: k.clone(),
                    files: l.files.iter().map(|f| f.path.clone()).collect(),
                });
            }
        }
    }
    let source_root = match &o.source_root {
        Some(s) => Some(
            Path::new(s)
                .canonicalize()
                .map_err(|e| format!("--source-root {}: {}", s, e))?,
        ),
        None => proj.source_root.clone(),
    };
    let settings = JobSettings {
        source_root,
        project_name: proj.name.clone(),
        summary: o.context.clone().unwrap_or_else(|| proj.summary.clone()),
        source_lang: o
            .source_lang
            .clone()
            .unwrap_or_else(|| cfg.source_lang.clone()),
        include_fuzzy,
        add_comment: !o.no_comment && cfg.add_comment,
        mark_fuzzy: !o.no_fuzzy && cfg.mark_fuzzy,
        batch_size: o.batch_size.unwrap_or(cfg.batch_size),
        limit: o.limit,
        dry_run: o.dry_run,
        glossary_file: o.glossary.as_ref().map(PathBuf::from),
        context_lines: o.context_lines.unwrap_or(2),
        retry_rejected: !o.no_retry,
        debug: o.debug || env("POLIBRE_DEBUG").is_some_and(|v| v != "0"),
        skip: Default::default(),
    };
    Ok(Prepared {
        provider,
        settings,
        targets,
        project: proj,
    })
}

fn cmd_check(o: &Opts) -> Result<i32, String> {
    let root = project_path(o);
    let proj = project::scan(&root, false)?;
    let wanted: Option<Vec<String>> = o
        .langs
        .as_deref()
        .filter(|l| *l != "all")
        .map(|l| l.split(',').map(|s| langs::normalize(s.trim())).collect());
    let (mut checked, mut bad) = (0usize, 0usize);
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for (code, l) in &proj.languages {
        if let Some(w) = &wanted {
            if !w
                .iter()
                .any(|x| x.eq_ignore_ascii_case(&langs::normalize(code)))
            {
                continue;
            }
        }
        for f in &l.files {
            let po = crate::po::PoFile::load(&f.path).map_err(|e| e.to_string())?;
            let (np, _) = po.plural_forms();
            for e in po.entries() {
                if e.obsolete || e.is_header() || e.is_untranslated() {
                    continue;
                }
                let auto = e.comment_lines.iter().any(|c| crate::is_auto_comment(c));
                if o.only_auto && !auto {
                    continue;
                }
                checked += 1;
                let src = crate::validate::Src {
                    msgid: &e.msgid,
                    msgid_plural: e.msgid_plural.as_deref(),
                    flags: &e.flags,
                    lang: code,
                    nplurals: np,
                };
                if let Err(why) = crate::validate::check(&src, &e.msgstr) {
                    bad += 1;
                    let key: String = why
                        .split(['(', '\''])
                        .next()
                        .unwrap_or(&why)
                        .trim()
                        .to_string();
                    *reasons.entry(key).or_default() += 1;
                    if env("GITHUB_ACTIONS").is_some() {
                        println!(
                            "::warning file={}::\"{}\": {}",
                            f.path.display(),
                            trunc(&e.msgid, 80),
                            why
                        );
                    } else {
                        println!(
                            "{}[{}]{} {}  {}\"{}\"{}\n      → \"{}\"\n      {}{}{}",
                            c(YELLOW),
                            code,
                            c(RESET),
                            short_path(&f.path),
                            c(BOLD),
                            trunc(&e.msgid, 90),
                            c(RESET),
                            trunc(&crate::engine::show_forms(&e.msgstr, " · "), 90),
                            c(DIM),
                            why,
                            c(RESET)
                        );
                    }
                }
            }
        }
    }
    eprintln!("\nChecked {} translations: {} with problems.", checked, bad);
    for (r, n) in &reasons {
        eprintln!("  {:>5}  {}", n, r);
    }
    Ok(if bad > 0 && o.fail_on_reject { 3 } else { 0 })
}

fn cmd_prompt(o: &Opts) -> Result<i32, String> {
    let p = prepare(o)?;
    let t = p
        .targets
        .first()
        .ok_or("no language with untranslated entries")?;
    let po = crate::po::PoFile::load(&t.files[0]).map_err(|e| e.to_string())?;
    let (n, expr) = po.plural_forms();
    println!(
        "=== SYSTEM ===\n{}",
        engine::system_prompt(&p.settings, &t.lang, n, &expr)
    );
    println!("=== (user message: JSON with glossary + items incl. source snippets) ===");
    let mut cache = project::SourceCache::default();
    for e in po
        .entries()
        .filter(|e| e.needs_translation(p.settings.include_fuzzy))
        .take(3)
    {
        println!("- {:?}", e.msgid);
        if let Some(root) = &p.settings.source_root {
            for r in e.references.iter().take(2) {
                if let Some(s) = cache.snippet(root, r, p.settings.context_lines) {
                    println!("  {}\n{}", r, s);
                }
            }
        }
    }
    Ok(0)
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Bar,
    Plain,
    Quiet,
}

fn cmd_translate(o: &Opts) -> Result<i32, String> {
    let prep = prepare(o)?;
    let mode = match o.progress.as_deref().unwrap_or("auto") {
        "bar" => Mode::Bar,
        "plain" => Mode::Plain,
        "quiet" | "none" => Mode::Quiet,
        _ => {
            if std::io::stderr().is_terminal() && env("CI").is_none() {
                Mode::Bar
            } else {
                Mode::Plain
            }
        }
    };
    let gha = env("GITHUB_ACTIONS").is_some();

    if prep.targets.is_empty() {
        eprintln!("Nothing to translate: all selected languages are complete.");
        return Ok(0);
    }
    eprintln!(
        "{}PoLibre{}  {} → {}  via {}{}{}{}",
        c(BOLD),
        c(RESET),
        prep.settings.source_lang,
        prep.targets
            .iter()
            .map(|t| t.lang.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        c(CYAN),
        prep.provider.describe(),
        c(RESET),
        if prep.settings.dry_run {
            "  (dry run)"
        } else {
            ""
        }
    );
    eprintln!(
        "{}source root: {} · glossary + code context: {} · project: {}{}",
        c(DIM),
        prep.settings
            .source_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none".into()),
        if prep.provider.kind.is_llm() {
            "yes"
        } else {
            "n/a"
        },
        prep.project.name,
        c(RESET)
    );

    // Ctrl-C: first press stops gracefully, second press exits immediately.
    let cancel = Arc::new(AtomicBool::new(false));
    let presses = Arc::new(AtomicUsize::new(0));
    {
        let cancel = cancel.clone();
        let presses = presses.clone();
        let _ = ctrlc::set_handler(move || {
            let n = presses.fetch_add(1, Ordering::SeqCst);
            cancel.store(true, Ordering::SeqCst);
            if n == 0 {
                eprintln!("\n⏸  Stopping… (files are saved after each batch; press Ctrl-C again to quit now)");
            } else {
                eprintln!("\nAborted.");
                std::process::exit(130);
            }
        });
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let provider = prep.provider.clone();
    let settings = prep.settings.clone();
    let targets = prep.targets.clone();
    let c2 = cancel.clone();
    let worker = std::thread::spawn(move || engine::run(provider, settings, targets, tx, c2));

    let mut ui = Ui::new(mode, gha);
    let mut report_ok = Vec::new();
    let mut report_rej = Vec::new();
    let mut summary = Summary::default();
    for ev in rx {
        match &ev {
            Event::Translated {
                lang,
                path,
                msgid,
                forms,
                ..
            } => {
                // A string, or a list of forms for plural entries.
                let msgstr = match forms.as_slice() {
                    [one] => serde_json::json!(one),
                    _ => serde_json::json!(forms),
                };
                report_ok.push(serde_json::json!({"lang": lang, "file": path, "msgid": msgid, "msgstr": msgstr}))
            }
            Event::Rejected {
                lang,
                path,
                msgid,
                reason,
                ..
            } => report_rej.push(
                serde_json::json!({"lang": lang, "file": path, "msgid": msgid, "reason": reason}),
            ),
            Event::Finished(s) => summary = s.clone(),
            _ => {}
        }
        ui.handle(&ev);
    }
    let _ = worker.join();
    ui.finish();

    if let Some(r) = &o.report {
        let v = serde_json::json!({
            "provider": prep.provider.describe(),
            "translated": report_ok,
            "rejected": report_rej,
            "failed": summary.failed,
            "cancelled": summary.cancelled,
            "dry_run": prep.settings.dry_run,
        });
        std::fs::write(r, serde_json::to_string_pretty(&v).unwrap())
            .map_err(|e| format!("{}: {}", r, e))?;
    }

    let secs = summary.elapsed.as_secs_f64();
    eprintln!(
        "\n{}{}{} translated · {}{}{} rejected · {}{}{} failed · {} file(s) {} · {:.1}s{}",
        c(GREEN),
        summary.translated,
        c(RESET),
        if summary.rejected > 0 { c(YELLOW) } else { "" },
        summary.rejected,
        c(RESET),
        if summary.failed > 0 { c(RED) } else { "" },
        summary.failed,
        c(RESET),
        summary.files_changed,
        if prep.settings.dry_run {
            "would change"
        } else {
            "updated"
        },
        secs,
        if prep.settings.dry_run {
            " (dry run: nothing written)"
        } else {
            ""
        }
    );
    if !prep.settings.dry_run && summary.translated > 0 && prep.settings.mark_fuzzy {
        eprintln!(
            "{}New entries are marked '#, fuzzy' for review.{}",
            c(DIM),
            c(RESET)
        );
    }
    if let Some(f) = &summary.fatal {
        eprintln!("{}error:{} {}", c(RED), c(RESET), f);
        return Ok(1);
    }
    if summary.cancelled {
        return Ok(130);
    }
    if o.fail_on_reject && (summary.rejected > 0 || summary.failed > 0) {
        return Ok(3);
    }
    if summary.translated == 0 && summary.failed > 0 {
        return Ok(1);
    }
    Ok(0)
}

/// Terminal progress display: log lines scroll above a fixed status block.
struct Ui {
    mode: Mode,
    gha: bool,
    total: usize,
    done: usize,
    file: String,
    file_total: usize,
    file_done: usize,
    lang: String,
    current: Vec<String>,
    status_lines: usize,
    started: Instant,
    spinner: usize,
    in_group: bool,
}

impl Ui {
    fn new(mode: Mode, gha: bool) -> Ui {
        Ui {
            mode,
            gha,
            total: 0,
            done: 0,
            file: String::new(),
            file_total: 0,
            file_done: 0,
            lang: String::new(),
            current: vec![],
            status_lines: 0,
            started: Instant::now(),
            spinner: 0,
            in_group: false,
        }
    }

    fn clear_status(&mut self) {
        if self.mode == Mode::Bar && self.status_lines > 0 {
            eprint!("\x1b[{}F\x1b[J", self.status_lines);
            self.status_lines = 0;
        }
    }

    fn log(&mut self, line: String) {
        if self.mode == Mode::Quiet {
            return;
        }
        self.clear_status();
        eprintln!("{}", line);
        self.draw();
    }

    fn draw(&mut self) {
        if self.mode != Mode::Bar || self.total == 0 {
            return;
        }
        let w = term_width();
        let bw = 30.min(w.saturating_sub(40)).max(10);
        let pct = if self.total == 0 {
            100.0
        } else {
            self.done as f64 * 100.0 / self.total as f64
        };
        let el = self.started.elapsed().as_secs_f64();
        let eta = if self.done > 0 && self.done < self.total {
            let s = el / self.done as f64 * (self.total - self.done) as f64;
            format!(" · ETA {}", fmt_dur(s))
        } else {
            String::new()
        };
        let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][self.spinner % 10];
        let mut lines = vec![
            format!(
                "{}Total{} {}{}{} {:>5.1}%  {}/{}  {}{}",
                c(BOLD),
                c(RESET),
                c(GREEN),
                bar(self.done, self.total, bw),
                c(RESET),
                pct,
                self.done,
                self.total,
                fmt_dur(el),
                eta
            ),
            format!(
                "{}File{}  {}{}{} {}/{}  {}[{}] {}{}",
                c(BOLD),
                c(RESET),
                c(CYAN),
                bar(self.file_done, self.file_total, bw),
                c(RESET),
                self.file_done,
                self.file_total,
                c(DIM),
                self.lang,
                trunc(&self.file, w.saturating_sub(bw + 25)),
                c(RESET)
            ),
        ];
        if !self.current.is_empty() {
            let first = trunc(&self.current[0], w.saturating_sub(30));
            let more = if self.current.len() > 1 {
                format!(" (+{} more)", self.current.len() - 1)
            } else {
                String::new()
            };
            lines.push(format!(
                "{} {}Translating{} \"{}\"{}",
                spin,
                c(YELLOW),
                c(RESET),
                first,
                more
            ));
        }
        for l in &lines {
            eprintln!("{}", l);
        }
        self.status_lines = lines.len();
        let _ = std::io::stderr().flush();
    }

    fn handle(&mut self, ev: &Event) {
        self.spinner += 1;
        match ev {
            Event::Info(m) => self.log(format!("{}ℹ {}{}", c(DIM), m, c(RESET))),
            Event::Debug { title, body } => {
                let mut text = format!("{}── {} ──{}", c(DIM), title, c(RESET));
                if !body.is_empty() {
                    text.push('\n');
                    text.push_str(body);
                }
                self.log(text);
            }
            Event::Warn(m) => {
                if self.gha {
                    self.log(format!("::warning::{}", m));
                } else {
                    self.log(format!("{}⚠ {}{}", c(YELLOW), m, c(RESET)));
                }
            }
            Event::Error(m) => {
                if self.gha {
                    self.log(format!("::error::{}", m));
                } else {
                    self.log(format!("{}✗ {}{}", c(RED), m, c(RESET)));
                }
            }
            Event::Plan { total, files } => {
                self.total = *total;
                self.log(format!(
                    "{} entries to translate in {} file(s)",
                    total, files
                ));
            }
            Event::FileStart { path, lang, count } => {
                self.file = short_path(path);
                self.lang = lang.clone();
                self.file_total = *count;
                self.file_done = 0;
                if self.gha && self.mode == Mode::Plain {
                    eprintln!("::group::{} [{}] - {} entries", self.file, lang, count);
                    self.in_group = true;
                } else {
                    self.log(format!(
                        "{}▶ [{}] {} — {} entries{}",
                        c(BOLD),
                        lang,
                        self.file,
                        count,
                        c(RESET)
                    ));
                }
            }
            Event::Working { texts, .. } => {
                self.current = texts.clone();
                if self.mode == Mode::Plain {
                    eprintln!(
                        "  … sending {} strings (\"{}\"{})",
                        texts.len(),
                        trunc(&texts[0], 50),
                        if texts.len() > 1 { ", …" } else { "" }
                    );
                } else {
                    self.clear_status();
                    self.draw();
                }
            }
            Event::Translated { msgid, forms, .. } => {
                let msgstr = &crate::engine::show_forms(forms, " · ");
                self.done += 1;
                self.file_done += 1;
                let w = term_width().saturating_sub(12) / 2;
                let line = format!(
                    "  {}✓{} [{:>4}/{}] {} {}→{} {}",
                    c(GREEN),
                    c(RESET),
                    self.done,
                    self.total,
                    trunc(msgid, w),
                    c(DIM),
                    c(RESET),
                    trunc(msgstr, w)
                );
                self.log(line);
            }
            Event::Rejected {
                msgid,
                reason,
                path,
                ..
            } => {
                self.done += 1;
                self.file_done += 1;
                if self.gha {
                    self.log(format!(
                        "::warning file={}::Rejected translation for \"{}\": {}",
                        path.display(),
                        trunc(msgid, 80),
                        reason
                    ));
                } else {
                    self.log(format!(
                        "  {}✗ rejected{} \"{}\": {}",
                        c(YELLOW),
                        c(RESET),
                        trunc(msgid, 60),
                        reason
                    ));
                }
            }
            Event::Failed { count } => {
                self.done += count;
                self.file_done += count;
            }
            Event::FileDone { translated, .. } => {
                self.current.clear();
                if self.in_group {
                    eprintln!("  {} entries translated", translated);
                    eprintln!("::endgroup::");
                    self.in_group = false;
                }
            }
            Event::Finished(_) => {
                self.current.clear();
                self.done = self.done.min(self.total);
            }
        }
    }

    fn finish(&mut self) {
        if self.mode == Mode::Bar {
            self.clear_status();
            self.draw();
        }
    }
}

fn short_path(p: &Path) -> String {
    let cwd = std::env::current_dir().ok();
    if let Some(c) = cwd {
        if let Ok(r) = p.strip_prefix(&c) {
            return r.display().to_string();
        }
    }
    p.display().to_string()
}

fn fmt_dur(s: f64) -> String {
    let s = s as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
