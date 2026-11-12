// SPDX-License-Identifier: AGPL-3.0-or-later
//! Graphical interface (egui). Mirrors the CLI: pick a project, a translation
//! service and languages, then follow the progress live.
//!
//! Layout targets: works at 800x600 effective pixels (minus the taskbar),
//! optimized for 1024x768, and resizes to fit smaller screens.

use crate::config::{Config, ProviderConfig, ProviderKind, ProviderSettings};
use crate::engine::{self, Event, JobSettings, Summary, Target};
use crate::langs;
use crate::project::{self, Project};
use crate::providers::{self, Detected, LtLanguages};
use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Key, KeyboardShortcut, Layout, Modifiers, RichText,
    TextStyle,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

#[path = "editor/mod.rs"]
mod editor;

const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

const DEFAULT_SIZE: [f32; 2] = [1000.0, 700.0];
/// Fits an 800x600 screen after the taskbar (48) and the title bar.
const MIN_SIZE: [f32; 2] = [760.0, 520.0];

const ACCENT: Color32 = Color32::from_rgb(0, 95, 184);
const GREEN: Color32 = Color32::from_rgb(60, 160, 90);
const AMBER: Color32 = Color32::from_rgb(214, 150, 30);
const RED: Color32 = Color32::from_rgb(210, 70, 70);
/// A button turned into "stop": a soft red tint with a stop icon. Used in
/// place of the button that started the work (Translate, Suggest, Refresh).
fn stop_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    let (fill, ink) = stop_colors(ui);
    egui::Button::new(RichText::new(format!("■  {}", text)).color(ink)).fill(fill)
}

/// A soft red (fill, ink) for buttons that stop or discard something.
fn stop_colors(ui: &egui::Ui) -> (Color32, Color32) {
    if ui.visuals().dark_mode {
        (Color32::from_rgb(84, 44, 42), Color32::from_rgb(255, 190, 182))
    } else {
        (Color32::from_rgb(251, 228, 225), Color32::from_rgb(170, 52, 40))
    }
}

const TRANSLATE_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);

pub fn run(path: Option<String>) -> Result<(), String> {
    // Developer aid for layout checks: POLIBRE_SIZE=800x552.
    let size = std::env::var("POLIBRE_SIZE")
        .ok()
        .and_then(|s| {
            s.split_once('x')
                .and_then(|(w, h)| Some([w.parse().ok()?, h.parse().ok()?]))
        })
        .unwrap_or(DEFAULT_SIZE);
    // Developer aid: POLIBRE_ICON_OUT=icon.ppm writes the icon (on black) and exits.
    if let Ok(out) = std::env::var("POLIBRE_ICON_OUT") {
        let px = crate::icon::rgba(256);
        let mut data = b"P6\n256 256\n255\n".to_vec();
        for p in px.chunks(4) {
            data.extend(p[..3].iter().map(|&c| (c as u32 * p[3] as u32 / 255) as u8));
        }
        return std::fs::write(out, data).map_err(|e| e.to_string());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PoLibre | by LibreTranslate")
            .with_inner_size(size)
            .with_min_inner_size(MIN_SIZE)
            .with_drag_and_drop(true)
            .with_icon(egui::IconData {
                rgba: crate::icon::rgba(256),
                width: 256,
                height: 256,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "PoLibre",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, path)))),
    )
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Fonts and style

/// System fonts covering CJK (and Hangul), tried in order. The first readable
/// file from each group is appended as a fallback after Inter, so only glyphs
/// Inter lacks come from these. `POLIBRE_FONT` (a path, optionally
/// `path#index` for collections) is tried first.
const FALLBACK_FONTS: &[&[&str]] = &[
    // Chinese/Japanese ideographs and kana
    &[
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\YuGothM.ttc",
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ],
    // Korean Hangul
    &[
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        "C:\\Windows\\Fonts\\malgun.ttf",
    ],
];

fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("Inter".into(), egui::FontData::from_static(INTER_REGULAR));
    fonts.font_data.insert(
        "Inter-SemiBold".into(),
        egui::FontData::from_static(INTER_SEMIBOLD),
    );
    // egui's bundled fonts stay as fallbacks for symbols such as ✔ ⚠ ✖.
    let defaults = fonts.families[&FontFamily::Proportional].clone();
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .insert(0, "Inter".into());
    let mut bold = vec!["Inter-SemiBold".to_string()];
    bold.extend(defaults);
    fonts.families.insert(semibold(), bold);

    let mut groups: Vec<Vec<(String, u32)>> = Vec::new();
    if let Ok(p) = std::env::var("POLIBRE_FONT") {
        groups.push(vec![match p.rsplit_once('#') {
            Some((f, i)) if i.parse::<u32>().is_ok() => (f.to_string(), i.parse().unwrap()),
            _ => (p, 0),
        }]);
    }
    groups.extend(
        FALLBACK_FONTS
            .iter()
            .map(|g| g.iter().map(|p| (p.to_string(), 0)).collect()),
    );
    let mut loaded = BTreeSet::new();
    for group in groups {
        for (path, index) in group {
            if loaded.contains(&path) {
                break; // this file already covers the group
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let name = format!("fallback:{path}");
            let mut data = egui::FontData::from_owned(bytes);
            data.index = index;
            // CJK fonts sit higher than Inter; nudge them onto the same baseline
            // so mixed strings like "有効なCRS" line up.
            data.tweak.y_offset_factor = 0.12;
            fonts.font_data.insert(name.clone(), data);
            for family in [FontFamily::Proportional, FontFamily::Monospace, semibold()] {
                fonts.families.entry(family).or_default().push(name.clone());
            }
            loaded.insert(path);
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(19.0, semibold())),
            (
                TextStyle::Name("group".into()),
                FontId::new(14.5, semibold()),
            ),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(14.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(12.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.combo_height = 320.0;
        let r = egui::Rounding::same(5.0);
        let w = &mut style.visuals.widgets;
        for v in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            v.rounding = r;
        }
        style.visuals.window_rounding = egui::Rounding::same(8.0);
        style.visuals.menu_rounding = egui::Rounding::same(6.0);
        // Selection highlight doubles as the progress bar fill.
        style.visuals.selection.bg_fill = ACCENT;
        // Text on selected tabs and items, readable on the accent in both themes.
        style.visuals.selection.stroke.color = Color32::WHITE;
    });
}

// ---------------------------------------------------------------------------
// State

#[derive(Clone, Copy, PartialEq)]
enum LogKind {
    Ok,
    Warn,
    Err,
    Info,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Languages,
    Results,
    Console,
    Editor,
}

/// An action waiting for an answer to "Save changes?".
#[derive(Clone)]
enum Pending {
    CloseEditor,
    OpenEditor(String),
    /// Open the editor on one string from the results.
    EditEntry(ResultRef),
    SwitchFile(usize),
    Quit,
}

/// One entry in the Console tab.
struct ConsoleEntry {
    id: u64,
    kind: Option<LogKind>,
    /// Seconds since the run started.
    at: f32,
    title: String,
    body: String,
    /// Lowercase title + body, for searching.
    search: String,
}

const CONSOLE_MAX: usize = 3000;

/// Where a translated (or rejected) string lives, to approve or edit it.
#[derive(Clone)]
struct ResultRef {
    code: String,
    path: PathBuf,
    msgctxt: Option<String>,
    msgid: String,
}

/// One row of the Results tab.
struct LogLine {
    kind: LogKind,
    /// Language name; empty for general messages.
    lang: String,
    a: String,
    b: String,
    /// The string in its file; None for general messages.
    entry: Option<ResultRef>,
    /// The proposed translation (plural forms separately); empty if rejected.
    forms: Vec<String>,
    /// "Translated by <model>", written with it when marked for review.
    note: Option<String>,
    /// Why writing it to the file didn't work.
    problem: Option<String>,
}

impl LogLine {
    /// A translation waiting for Accept / Edit (not yet in the file).
    fn is_proposal(&self) -> bool {
        self.kind == LogKind::Ok && self.entry.is_some() && !self.forms.is_empty()
    }
}

struct Job {
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    total: usize,
    done: usize,
    translated: usize,
    rejected: usize,
    file: String,
    lang: String,
    current: Vec<String>,
    started: Instant,
    eta: Option<f32>,
    /// The note for this run's translations when they're marked for review.
    note: Option<String>,
    finished: Option<Summary>,
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Name,
    MostComplete,
    LeastComplete,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Name => "Name",
            Sort::MostComplete => "Most complete",
            Sort::LeastComplete => "Least complete",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Project,
    SourceRoot,
    Glossary,
}

enum Bg {
    /// An entry for the Console from work outside a translation run.
    Console(Option<LogKind>, String, String),
    Suggested(editor::Suggestion),
    /// Model list for a service; the number identifies the request (see `models_req`).
    Models(ProviderKind, u64, Result<Vec<String>, String>),
    Lt(String, Result<LtLanguages, String>),
    Picked(Pick, Result<Option<String>, String>),
}

struct App {
    cfg: Config,
    path: String,
    scanned_path: String,
    project: Option<Project>,
    scan_error: Option<String>,
    detected: Vec<Detected>,
    provider: ProviderKind,
    settings: BTreeMap<ProviderKind, ProviderSettings>,
    custom_model: BTreeSet<ProviderKind>,
    remember_key: bool,
    models: BTreeMap<ProviderKind, Vec<String>>,
    model_note: Option<(LogKind, String)>,
    lt_langs: Option<(String, LtLanguages)>,
    lt_loading: bool,
    models_loading: bool,
    /// Identifies the latest model-list request; answers to older (cancelled) ones are ignored.
    models_req: u64,
    picking: bool,
    pick_note: Option<String>,
    bg_tx: Sender<Bg>,
    bg_rx: Receiver<Bg>,
    selected: BTreeSet<String>,
    filter: String,
    /// Language list order.
    sort: Sort,
    tab: Tab,
    console: VecDeque<ConsoleEntry>,
    console_next: u64,
    console_t0: Instant,
    console_filter: String,
    console_follow: bool,
    editor: Option<editor::Editor>,
    /// Suggestions being fetched (file, entry, cancel flag).
    suggest_jobs: Vec<(PathBuf, usize, Arc<AtomicBool>)>,
    editor_problem: Option<String>,
    pending: Option<Pending>,
    /// "Reload the project?" is showing.
    reload_prompt: bool,
    quitting: bool,
    summary: String,
    source_root: String,
    glossary: String,
    source_lang: String,
    include_fuzzy: bool,
    batch_size: usize,
    limit: usize,
    more_options: bool,
    dark: bool,
    job: Option<Job>,
    log: VecDeque<LogLine>,
    fitted: bool,
    /// Developer aid: POLIBRE_SCREENSHOT=out.ppm saves a frame and exits.
    screenshot: Option<(PathBuf, Instant, bool)>,
}

impl App {
    fn new(cc: &eframe::CreationContext, path: Option<String>) -> App {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);
        if let Some(z) = std::env::var("POLIBRE_ZOOM")
            .ok()
            .and_then(|z| z.parse::<f32>().ok())
        {
            cc.egui_ctx.set_zoom_factor(z); // developer aid: simulate 120/144 dpi
        }
        let cfg = Config::load();
        // POLIBRE_THEME=light|dark overrides the saved choice (developer aid).
        let dark = match std::env::var("POLIBRE_THEME").as_deref() {
            Ok("dark") => true,
            Ok("light") => false,
            _ => cfg.theme == "dark",
        };
        cc.egui_ctx.set_theme(if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        let detected = providers::detect();
        // First run: LibreTranslate, which works without installing anything.
        let provider = cfg.provider.unwrap_or(ProviderKind::Libretranslate);
        let mut settings = BTreeMap::new();
        for k in ProviderKind::ALL {
            let mut s = cfg.provider_settings(k);
            let list = k.suggested_models();
            if s.model.is_empty() && !list.is_empty() && !list.contains(&"") {
                s.model = list[0].to_string();
            }
            settings.insert(k, s);
        }
        let remember_key = cfg.providers.values().any(|p| !p.api_key.is_empty());
        let path = path.unwrap_or_else(|| {
            if !cfg.last_project.is_empty() {
                cfg.last_project.clone()
            } else {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            }
        });
        let (bg_tx, bg_rx) = channel();
        let mut app = App {
            path,
            scanned_path: String::new(),
            project: None,
            scan_error: None,
            detected,
            provider,
            settings,
            custom_model: BTreeSet::new(),
            remember_key,
            models: BTreeMap::new(),
            model_note: None,
            lt_langs: None,
            lt_loading: false,
            models_loading: false,
            models_req: 0,
            picking: false,
            pick_note: None,
            bg_tx,
            bg_rx,
            selected: BTreeSet::new(),
            filter: String::new(),
            sort: Sort::Name,
            // Developer aid for screenshots: POLIBRE_TAB=console.
            tab: if std::env::var("POLIBRE_TAB").as_deref() == Ok("console") {
                Tab::Console
            } else {
                Tab::Languages
            },
            console: VecDeque::new(),
            console_next: 0,
            console_t0: Instant::now(),
            console_filter: String::new(),
            console_follow: true,
            editor: None,
            suggest_jobs: Vec::new(),
            editor_problem: None,
            pending: None,
            reload_prompt: false,
            quitting: false,
            summary: String::new(),
            source_root: String::new(),
            glossary: String::new(),
            source_lang: cfg.source_lang.clone(),
            include_fuzzy: cfg.include_fuzzy,
            batch_size: cfg.batch_size,
            limit: 0,
            more_options: std::env::var("POLIBRE_MORE_OPTIONS").is_ok(),
            dark,
            job: None,
            log: VecDeque::new(),
            fitted: false,
            screenshot: std::env::var("POLIBRE_SCREENSHOT")
                .ok()
                .map(|p| (PathBuf::from(p), Instant::now(), false)),
            cfg,
        };
        for k in ProviderKind::ALL {
            let m = &app.settings[&k].model;
            if !m.is_empty() && !k.suggested_models().contains(&m.as_str()) {
                app.custom_model.insert(k);
            }
        }
        app.scan();
        app.ensure_lt_langs();
        // Developer aid: POLIBRE_EDIT=it opens the translation editor.
        if let Ok(l) = std::env::var("POLIBRE_EDIT") {
            app.open_editor(&l);
        }
        // Developer aid: POLIBRE_AUTOSTART=it,de selects languages and starts a preview run.
        if let Ok(l) = std::env::var("POLIBRE_AUTOSTART") {
            app.selected = l.split(',').map(|s| s.trim().to_string()).collect();
            app.limit = 40;
            app.start(false);
        }
        app
    }

    fn running(&self) -> bool {
        self.job
            .as_ref()
            .map(|j| j.finished.is_none())
            .unwrap_or(false)
    }

    fn provider_config(&self) -> ProviderConfig {
        ProviderConfig::resolve(self.provider, &self.settings[&self.provider])
    }

    fn scan(&mut self) {
        let path = self.path.trim().to_string();
        self.scanned_path = path.clone();
        if path.is_empty() {
            self.project = None;
            self.scan_error = None;
            return;
        }
        match project::scan(&PathBuf::from(&path), self.include_fuzzy) {
            Ok(proj) => {
                self.summary = proj.summary.clone();
                self.source_root = proj
                    .source_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.selected.retain(|l| proj.languages.contains_key(l));
                if proj.languages.is_empty() {
                    self.scan_error = Some(
                        "No translation files (.po) were found in this folder. Choose the folder that contains the project's locale files.".into(),
                    );
                    self.project = None;
                } else {
                    self.scan_error = None;
                    self.project = Some(proj);
                    self.cfg.last_project = path;
                    let _ = self.cfg.save();
                }
            }
            Err(_) => {
                self.project = None;
                self.scan_error =
                    Some("This folder can't be opened. Check that the path is correct.".into());
            }
        }
    }

    fn ensure_lt_langs(&mut self) {
        if self.provider != ProviderKind::Libretranslate || self.lt_loading {
            return;
        }
        let pc = self.provider_config();
        if self
            .lt_langs
            .as_ref()
            .map(|(u, _)| *u == pc.url)
            .unwrap_or(false)
        {
            return;
        }
        self.lt_loading = true;
        let tx = self.bg_tx.clone();
        let src = self.source_lang.clone();
        std::thread::spawn(move || {
            let r = LtLanguages::fetch(&pc.url, &src);
            let _ = tx.send(Bg::Lt(pc.url.clone(), r));
        });
    }

    fn refresh_models(&mut self) {
        let pc = self.provider_config();
        self.models_loading = true;
        self.model_note = None;
        self.models_req += 1;
        let (tx, req) = (self.bg_tx.clone(), self.models_req);
        std::thread::spawn(move || {
            let r = providers::list_models(&pc);
            let _ = tx.send(Bg::Models(pc.kind, req, r));
        });
    }

    fn browse(&mut self, what: Pick) {
        if self.picking {
            return;
        }
        self.picking = true;
        self.pick_note = None;
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let r = match what {
                Pick::Project => pick_path(true, "Choose the project folder"),
                Pick::SourceRoot => pick_path(true, "Choose the source code folder"),
                Pick::Glossary => pick_path(false, "Choose a glossary file"),
            };
            let _ = tx.send(Bg::Picked(what, r));
        });
    }

    fn lt_supported(&self, code: &str) -> Option<bool> {
        if self.provider != ProviderKind::Libretranslate {
            return None;
        }
        let langs = self
            .lt_langs
            .as_ref()
            .map(|(_, l)| l.clone())
            .unwrap_or_else(LtLanguages::fallback);
        Some(langs.map(code).is_some())
    }

    /// Remembers the current choices for next time. The API key is stored only
    /// when "Remember API key" is on.
    fn save_settings(&mut self) {
        self.cfg.provider = Some(self.provider);
        let mut s = self.settings[&self.provider].clone();
        if !self.remember_key {
            s.api_key.clear();
        }
        self.cfg.providers.insert(self.provider, s);
        self.cfg.include_fuzzy = self.include_fuzzy;
        self.cfg.batch_size = self.batch_size;
        self.cfg.source_lang = self.source_lang.clone();
        let _ = self.cfg.save();
    }

    /// Translations waiting in Results, per language code.
    fn proposals_per_language(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for l in self.log.iter().filter(|l| l.is_proposal()) {
            *m.entry(l.entry.as_ref().unwrap().code.clone()).or_insert(0) += 1;
        }
        m
    }

    /// Strings a run would translate in a language: the missing ones, minus
    /// those already waiting in Results.
    fn to_translate(&self, l: &project::LangInfo, waiting: &BTreeMap<String, usize>) -> usize {
        l.untranslated().saturating_sub(waiting.get(&l.code).copied().unwrap_or(0))
    }

    fn pending_count(&self) -> usize {
        let Some(p) = &self.project else { return 0 };
        let waiting = self.proposals_per_language();
        let n: usize = self
            .selected
            .iter()
            .filter(|l| self.lt_supported(l) != Some(false))
            .filter_map(|l| p.languages.get(l))
            .map(|l| self.to_translate(l, &waiting))
            .sum();
        if self.limit > 0 {
            n.min(self.limit)
        } else {
            n
        }
    }

    fn start(&mut self, save: bool) {
        if save {
            self.save_settings();
        }
        let Some(proj) = &self.project else { return };
        let waiting = self.proposals_per_language();
        let targets: Vec<Target> = self
            .selected
            .iter()
            .filter(|l| self.lt_supported(l) != Some(false))
            .filter_map(|l| proj.languages.get(l))
            .filter(|l| self.to_translate(l, &waiting) > 0)
            .map(|l| Target {
                lang: l.code.clone(),
                files: l.files.iter().map(|f| f.path.clone()).collect(),
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let settings = self.job_settings(proj);
        let provider = self.provider_config();
        let note = engine::auto_note(&provider, &settings);
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        // Keep the translations still waiting for a decision (the run skips
        // them); drop the rest of the previous run's results.
        self.log.retain(|l| l.is_proposal());
        self.tab = Tab::Results;
        self.console_t0 = Instant::now();
        std::thread::spawn(move || engine::run(provider, settings, targets, tx, c2));
        self.job = Some(Job {
            rx,
            cancel,
            total: 0,
            done: 0,
            translated: 0,
            rejected: 0,
            file: String::new(),
            lang: String::new(),
            current: vec![],
            started: Instant::now(),
            eta: None,
            note,
            finished: None,
        });
    }

    /// Settings for a translation run (or a single suggestion) from the current choices.
    fn job_settings(&self, proj: &Project) -> JobSettings {
        let is_llm = self.provider.is_llm();
        JobSettings {
            source_root: if !is_llm || self.source_root.trim().is_empty() {
                None
            } else {
                Some(PathBuf::from(self.source_root.trim()))
            },
            project_name: proj.name.clone(),
            summary: self.summary.clone(),
            source_lang: self.source_lang.clone(),
            include_fuzzy: self.include_fuzzy,
            // The app shows translations first; nothing is written by a run.
            add_comment: true,
            mark_fuzzy: true,
            batch_size: self.batch_size,
            limit: if self.limit > 0 {
                Some(self.limit)
            } else {
                None
            },
            dry_run: true,
            glossary_file: if self.glossary.trim().is_empty() {
                None
            } else {
                Some(PathBuf::from(self.glossary.trim()))
            },
            context_lines: 2,
            retry_rejected: true,
            debug: true,
            // Translations waiting in Results aren't asked for again.
            skip: self
                .log
                .iter()
                .filter(|l| l.is_proposal())
                .filter_map(|l| l.entry.as_ref())
                .map(|r| (r.path.clone(), r.msgctxt.clone(), r.msgid.clone()))
                .collect(),
        }
    }

    /// Translates one editor entry in the background (the Suggest button).
    fn suggest(&mut self, job: Box<editor::SuggestJob>) {
        let job = *job;
        let Some(proj) = &self.project else { return };
        let settings = self.job_settings(proj);
        let provider = self.provider_config();
        let note = engine::auto_note(&provider, &settings);
        let bg = self.bg_tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.suggest_jobs.push((job.path.clone(), job.idx, cancel.clone()));
        std::thread::spawn(move || {
            let (tx, rx) = channel();
            let e = job.entry.clone();
            let worker = std::thread::spawn(move || {
                engine::suggest(
                    &provider,
                    &settings,
                    &job.lang,
                    &job.files,
                    &e,
                    job.nplurals,
                    &job.plural_expr,
                    &tx,
                    &cancel,
                )
            });
            // Forward the request log to the Console while it runs.
            for ev in rx {
                let entry = match ev {
                    Event::Debug { title, body } => Bg::Console(None, title, body),
                    Event::Info(m) => Bg::Console(Some(LogKind::Info), sentence(&m), String::new()),
                    Event::Warn(m) => Bg::Console(Some(LogKind::Warn), sentence(&m), String::new()),
                    _ => continue,
                };
                let _ = bg.send(entry);
            }
            let result = worker
                .join()
                .unwrap_or_else(|_| Err("the request failed unexpectedly".into()));
            let _ = bg.send(Bg::Suggested(editor::Suggestion {
                path: job.path,
                idx: job.idx,
                msgid: job.entry.msgid,
                result,
                note,
            }));
        });
    }

    fn push_console(&mut self, kind: Option<LogKind>, title: String, body: String) {
        let search = format!("{}\n{}", title, body).to_lowercase();
        self.console.push_back(ConsoleEntry {
            id: self.console_next,
            kind,
            at: self.console_t0.elapsed().as_secs_f32(),
            title,
            body,
            search,
        });
        self.console_next += 1;
        while self.console.len() > CONSOLE_MAX {
            self.console.pop_front();
        }
    }

    fn console_text(entries: &[&ConsoleEntry]) -> String {
        entries
            .iter()
            .map(|e| {
                let mut t = format!("[{:.1}s] {}", e.at, e.title);
                if !e.body.is_empty() {
                    t.push('\n');
                    t.push_str(&e.body);
                }
                t
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn console_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        let f = self.console_filter.trim().to_lowercase();
        let shown: Vec<&ConsoleEntry> = self
            .console
            .iter()
            .filter(|e| f.is_empty() || e.search.contains(&f))
            .collect();
        let mut clear = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!shown.is_empty(), egui::Button::new("Copy all"))
                .on_hover_text("Copy the entries shown below, with their details")
                .clicked()
            {
                ui.ctx().copy_text(Self::console_text(&shown));
            }
            if ui
                .add_enabled(!self.console.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                clear = true;
            }
            ui.checkbox(&mut self.console_follow, "Auto scroll");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.console_filter)
                        .hint_text(prompt("Find in console"))
                        .desired_width(180.0),
                );
            });
        });
        ui.add_space(4.0);
        let follow = self.console_follow;
        egui::ScrollArea::vertical()
            .id_salt("console")
            .stick_to_bottom(follow)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Room for the text box borders, which the scroll area would clip.
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 1.0,
                        right: 6.0,
                        top: 1.0,
                        bottom: 1.0,
                    })
                    .show(ui, |ui| {
                        if shown.is_empty() && !self.console.is_empty() {
                            ui.label(RichText::new("No entries match the search.").weak());
                        }
                        let mono = TextStyle::Monospace;
                        // Developer aid for screenshots: POLIBRE_CONSOLE_OPEN=1 expands entries.
                        let open_all = std::env::var("POLIBRE_CONSOLE_OPEN").is_ok();
                        for e in &shown {
                            let mut head = RichText::new(format!("{:>6.1}s   {}", e.at, e.title));
                            if e.kind.is_some() {
                                head = head.strong();
                            }
                            if e.body.is_empty() {
                                ui.horizontal(|ui| {
                                    ui.add_space(ui.spacing().indent);
                                    if let Some(k) = e.kind {
                                        ui.label(icon(k));
                                    }
                                    ui.add(egui::Label::new(head).wrap());
                                });
                                continue;
                            }
                            egui::CollapsingHeader::new(head)
                                .id_salt(("console", e.id))
                                .default_open(open_all)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui.small_button("Copy").clicked() {
                                            ui.ctx().copy_text(e.body.clone());
                                        }
                                        ui.label(
                                            RichText::new(format!(
                                                "{} characters",
                                                thousands(e.body.chars().count())
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                    });
                                    let mut text = e.body.as_str();
                                    ui.add(
                                        egui::TextEdit::multiline(&mut text)
                                            .font(mono.clone())
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(1),
                                    );
                                });
                        }
                    });
            });
        if clear {
            self.console.clear();
        }
    }

    fn push_log(&mut self, l: LogLine) {
        self.log.push_back(l);
        while self.log.len() > 1500 {
            self.log.pop_front();
        }
    }

    /// Stops the suggestion requests the editor no longer waits for: cancelled
    /// by the user, or the editor was closed, reloaded or switched files.
    fn cancel_unwanted_suggestions(&mut self) {
        let ed = self.editor.as_ref();
        self.suggest_jobs.retain(|(path, idx, cancel)| {
            let wanted = ed.is_some_and(|e| e.wants_suggestion(path, *idx));
            if !wanted {
                cancel.store(true, Ordering::Relaxed);
            }
            wanted
        });
    }

    fn poll(&mut self) {
        self.cancel_unwanted_suggestions();
        while let Ok(b) = self.bg_rx.try_recv() {
            match b {
                Bg::Console(kind, title, body) => self.push_console(kind, title, body),
                Bg::Suggested(s) => {
                    self.suggest_jobs.retain(|(p, i, _)| !(*p == s.path && *i == s.idx));
                    if let Some(ed) = &mut self.editor {
                        ed.suggested(s);
                    }
                }
                Bg::Models(_, req, _) if req != self.models_req => {} // cancelled
                Bg::Models(k, _, r) => {
                    self.models_loading = false;
                    match r {
                        Ok(m) if m.is_empty() => {
                            self.model_note = Some((
                                LogKind::Warn,
                                "The service didn't list any models. Type a model name instead."
                                    .into(),
                            ))
                        }
                        Ok(m) => {
                            self.model_note =
                                Some((LogKind::Info, format!("{} models available.", m.len())));
                            self.models.insert(k, m);
                        }
                        Err(e) => {
                            self.model_note = Some((
                                LogKind::Warn,
                                format!("Unable to get the model list: {}", e),
                            ))
                        }
                    }
                }
                Bg::Lt(url, r) => {
                    self.lt_loading = false;
                    let l = r.unwrap_or_else(|_| LtLanguages::fallback());
                    self.lt_langs = Some((url, l));
                }
                Bg::Picked(what, r) => {
                    self.picking = false;
                    match r {
                        Ok(Some(p)) => match what {
                            Pick::Project => {
                                self.path = p;
                                self.scan();
                            }
                            Pick::SourceRoot => self.source_root = p,
                            Pick::Glossary => self.glossary = p,
                        },
                        Ok(None) => {}
                        Err(e) => self.pick_note = Some(e),
                    }
                }
            }
        }
        let mut new_logs = Vec::new();
        let mut new_console: Vec<(Option<LogKind>, String, String)> = Vec::new();
        if let Some(job) = &mut self.job {
            while let Ok(ev) = job.rx.try_recv() {
                let msg = |kind, m: String| LogLine {
                    kind,
                    lang: String::new(),
                    a: m,
                    b: String::new(),
                    entry: None,
                    forms: vec![],
                    note: None,
                    problem: None,
                };
                let entry = |code: &str, path: PathBuf, msgctxt: Option<String>, msgid: &str| {
                    Some(ResultRef {
                        code: code.to_string(),
                        path,
                        msgctxt,
                        msgid: msgid.to_string(),
                    })
                };
                match ev {
                    // Shown in the Console only.
                    Event::Info(m) => {
                        new_console.push((Some(LogKind::Info), sentence(&m), String::new()))
                    }
                    Event::Warn(m) => {
                        new_console.push((Some(LogKind::Warn), sentence(&m), String::new()));
                        new_logs.push(msg(LogKind::Warn, sentence(&m)))
                    }
                    Event::Error(m) => {
                        new_console.push((Some(LogKind::Err), sentence(&m), String::new()));
                        new_logs.push(msg(LogKind::Err, sentence(&m)))
                    }
                    Event::Debug { title, body } => new_console.push((None, title, body)),
                    Event::Plan { total, .. } => job.total = total,
                    Event::FileStart { path, lang, .. } => {
                        job.file = path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        job.lang = langs::name(&lang);
                    }
                    Event::Working { texts, .. } => job.current = texts,
                    Event::Translated {
                        lang,
                        path,
                        msgctxt,
                        msgid,
                        forms,
                    } => {
                        job.done += 1;
                        job.translated += 1;
                        let msgstr = engine::show_forms(&forms, "\n");
                        new_logs.push(LogLine {
                            kind: LogKind::Ok,
                            lang: langs::name(&lang),
                            entry: entry(&lang, path, msgctxt, &msgid),
                            a: msgid,
                            b: msgstr,
                            forms,
                            note: job.note.clone(),
                            problem: None,
                        });
                    }
                    Event::Rejected {
                        lang,
                        path,
                        msgctxt,
                        msgid,
                        reason,
                    } => {
                        job.done += 1;
                        job.rejected += 1;
                        new_logs.push(LogLine {
                            kind: LogKind::Warn,
                            lang: langs::name(&lang),
                            entry: entry(&lang, path, msgctxt, &msgid),
                            a: msgid,
                            b: format!("Not used: {}", reason),
                            forms: vec![],
                            note: None,
                            problem: None,
                        });
                    }
                    Event::Failed { count } => job.done += count,
                    Event::FileDone { .. } => job.current.clear(),
                    Event::Finished(s) => {
                        job.current.clear();
                        job.finished = Some(s);
                    }
                }
            }
        }
        for l in new_logs {
            self.push_log(l);
        }
        for (kind, title, body) in new_console {
            self.push_console(kind, title, body);
        }
    }

    // -----------------------------------------------------------------------
    // Translation editor

    fn editor_dirty(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.dirty())
    }

    /// Runs an action, first asking to save if the editor has unsaved changes.
    fn request(&mut self, p: Pending) {
        if self.editor_dirty() {
            self.pending = Some(p);
        } else {
            self.perform(p);
        }
    }

    fn perform(&mut self, p: Pending) {
        match p {
            Pending::CloseEditor => {
                self.editor = None;
                if self.tab == Tab::Editor {
                    self.tab = Tab::Languages;
                }
            }
            Pending::OpenEditor(code) => self.open_editor(&code),
            Pending::EditEntry(r) => {
                if self.editor.as_ref().is_none_or(|e| e.lang != r.code) {
                    self.open_editor(&r.code);
                }
                if let Some(ed) = &mut self.editor {
                    ed.focus_entry(&r.path, r.msgctxt.as_deref(), &r.msgid);
                    self.tab = Tab::Editor;
                }
            }
            Pending::SwitchFile(i) => {
                if let Some(ed) = &mut self.editor {
                    if let Err(e) = ed.switch_file(i) {
                        self.editor_problem = Some(e);
                    }
                }
            }
            Pending::Quit => {
                self.quitting = true;
            }
        }
    }

    fn open_editor(&mut self, code: &str) {
        let Some(p) = &self.project else { return };
        let Some(l) = p.languages.get(code) else {
            return;
        };
        let files: Vec<PathBuf> = l.files.iter().map(|f| f.path.clone()).collect();
        // Start with the file that has the most strings to review.
        let first = l
            .files
            .iter()
            .enumerate()
            .max_by_key(|(_, f)| (f.fuzzy, f.untranslated))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let root = if self.source_root.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(self.source_root.trim()))
        };
        let mut others: Vec<(String, String, Vec<PathBuf>)> = p
            .languages
            .values()
            .filter(|o| o.code != code)
            .map(|o| {
                (
                    o.code.clone(),
                    o.name.clone(),
                    o.files.iter().map(|f| f.path.clone()).collect(),
                )
            })
            .collect();
        others.sort_by(|a, b| a.1.cmp(&b.1));
        match editor::Editor::open(code, &l.name, files, first, root) {
            Ok(mut ed) => {
                ed.others = others;
                let r = self.cfg.reference_lang.clone();
                ed.set_reference((!r.is_empty()).then_some(r.as_str()));
                self.editor = Some(ed);
                self.editor_problem = None;
                self.tab = Tab::Editor;
            }
            Err(e) => self.editor_problem = Some(e),
        }
    }

    fn editor_panel(&mut self, ui: &mut egui::Ui) {
        if let Some(p) = self.editor_problem.clone() {
            ui.add_space(8.0);
            note(ui, LogKind::Err, &p);
        }
        let Some(ed) = &mut self.editor else { return };
        ed.service = service_label(self.provider).to_string();
        // Remember the comparison language for next time.
        let r = ed.reference_code().unwrap_or("").to_string();
        if r != self.cfg.reference_lang {
            self.cfg.reference_lang = r;
            let _ = self.cfg.save();
        }
        let g = self.glossary.trim();
        ed.set_glossary_file((!g.is_empty()).then(|| PathBuf::from(g)));
        match ed.ui(ui) {
            editor::Request::Suggest(job) => self.suggest(job),
            editor::Request::None => {}
            editor::Request::Close => self.request(Pending::CloseEditor),
            editor::Request::SwitchFile(i) => self.request(Pending::SwitchFile(i)),
            editor::Request::Saved => {
                let sel = self.selected.clone();
                self.scan();
                self.selected = sel;
            }
        }
    }

    /// "Save changes?" dialog for a pending action.
    fn save_prompt(&mut self, ctx: &egui::Context) {
        let Some(p) = self.pending.clone() else {
            return;
        };
        let name = self
            .editor
            .as_ref()
            .map(|e| e.name.clone())
            .unwrap_or_default();
        modal_backdrop(ctx);
        let mut answer: Option<u8> = None; // 0 save, 1 don't save, 2 cancel
        egui::Window::new("PoLibre")
            .id(egui::Id::new("save-prompt"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::popup(&ctx.style()).inner_margin(20.0))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(
                    RichText::new(format!("Save changes to the {} translations?", name))
                        .text_style(TextStyle::Name("group".into())),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("If you don't save, your changes will be lost.").weak());
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let save = egui::Button::new(RichText::new("Save").color(Color32::WHITE))
                        .fill(ACCENT)
                        .min_size(egui::vec2(90.0, 28.0));
                    if ui.add(save).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                        answer = Some(0);
                    }
                    if ui
                        .add(egui::Button::new("Don't save").min_size(egui::vec2(90.0, 28.0)))
                        .clicked()
                    {
                        answer = Some(1);
                    }
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::vec2(90.0, 28.0)))
                        .clicked()
                        || ui.input(|i| i.key_pressed(Key::Escape))
                    {
                        answer = Some(2);
                    }
                });
            });
        match answer {
            Some(0) => {
                let ok = match self.editor.as_mut().map(|e| e.save(false)) {
                    Some(Err(e)) if e == "conflict" => {
                        // The editor now shows the conflict and its choices.
                        self.tab = Tab::Editor;
                        false
                    }
                    Some(Err(e)) => {
                        self.editor_problem = Some(e);
                        false
                    }
                    _ => true,
                };
                self.pending = None;
                if ok {
                    let sel = self.selected.clone();
                    self.scan();
                    self.selected = sel;
                    self.perform(p);
                }
            }
            Some(1) => {
                if let Some(ed) = &mut self.editor {
                    let _ = ed.load();
                }
                self.pending = None;
                self.perform(p);
            }
            Some(2) => self.pending = None,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Panels

    fn project_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.theme_button(ui);
                self.reload_button(ui, running);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    self.settings_toggle(ui);
                    self.project_field(ui, running)
                });
            });
        });
        self.project_summary(ui);
    }

    /// Collapses or restores the translation settings panel, and remembers it.
    fn settings_toggle(&mut self, ui: &mut egui::Ui) {
        let hidden = self.cfg.hide_settings;
        let tip = if hidden {
            "Show the translation settings"
        } else {
            "Hide the translation settings"
        };
        let b = egui::Button::new(RichText::new("⚙").size(17.0));
        if ui.add(b).on_hover_text(tip).clicked() {
            self.cfg.hide_settings = !hidden;
            let _ = self.cfg.save();
        }
    }

    /// Reads the project's files again; asks first if that would lose
    /// unsaved edits or translations waiting in Results.
    fn reload_button(&mut self, ui: &mut egui::Ui, running: bool) {
        let enabled = !running && !self.picking && !self.path.trim().is_empty();
        let b = egui::Button::new(RichText::new("🔄").size(17.0));
        if ui
            .add_enabled(enabled, b)
            .on_hover_text("Reload the project's translation files")
            .on_disabled_hover_text("Can't reload while translating")
            .clicked()
        {
            if self.editor_dirty() || self.log.iter().any(|l| l.is_proposal()) {
                self.reload_prompt = true;
            } else {
                self.reload();
            }
        }
    }

    /// Reads the project again from disk, dropping unsaved editor changes and
    /// the Results list.
    fn reload(&mut self) {
        self.reload_prompt = false;
        self.log.clear();
        // The last run's results and notes no longer apply.
        if !self.running() {
            self.job = None;
            if self.tab == Tab::Results {
                self.tab = Tab::Languages;
            }
        }
        let sel = self.selected.clone();
        self.scan();
        self.selected = sel;
        self.selected
            .retain(|l| self.project.as_ref().is_some_and(|p| p.languages.contains_key(l)));
        let lang = self.editor.as_ref().map(|e| e.lang.clone());
        if let Some(code) = lang {
            let still_there = self.project.as_ref().is_some_and(|p| p.languages.contains_key(&code));
            if !still_there {
                self.perform(Pending::CloseEditor);
            } else if let Some(ed) = &mut self.editor {
                if let Err(e) = ed.load() {
                    self.editor_problem = Some(e);
                }
            }
        }
    }

    fn reload_prompt(&mut self, ctx: &egui::Context) {
        if !self.reload_prompt {
            return;
        }
        let mut lost = Vec::new();
        if let Some(ed) = self.editor.as_ref().filter(|e| e.dirty()) {
            lost.push(format!("Your unsaved changes to the {} translations will be lost.", ed.name));
        }
        match self.log.iter().filter(|l| l.is_proposal()).count() {
            0 => {}
            1 => lost.push("1 translation in Results hasn't been accepted or marked for review yet, and will be discarded.".into()),
            n => lost.push(format!(
                "{} translations in Results haven't been accepted or marked for review yet, and will be discarded.",
                thousands(n)
            )),
        }
        modal_backdrop(ctx);
        let mut answer: Option<bool> = None;
        egui::Window::new("PoLibre")
            .id(egui::Id::new("reload-prompt"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::popup(&ctx.style()).inner_margin(20.0))
            .show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(RichText::new("Reload the project?").text_style(TextStyle::Name("group".into())));
                ui.add_space(4.0);
                for l in &lost {
                    ui.add(egui::Label::new(RichText::new(l).weak()).wrap());
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let (fill, ink) = stop_colors(ui);
                    let reload = egui::Button::new(RichText::new("Reload").color(ink)).fill(fill).min_size(egui::vec2(90.0, 28.0));
                    if ui.add(reload).clicked() {
                        answer = Some(true);
                    }
                    if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(90.0, 28.0))).clicked()
                        || ui.input(|i| i.key_pressed(Key::Escape))
                    {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => self.reload(),
            Some(false) => self.reload_prompt = false,
            None => {}
        }
    }

    /// Switches between the light and dark theme, and remembers the choice.
    fn theme_button(&mut self, ui: &mut egui::Ui) {
        let (glyph, tip) = if self.dark {
            ("☀", "Switch to the light theme")
        } else {
            ("🌙", "Switch to the dark theme")
        };
        if ui
            .button(RichText::new(glyph).size(17.0))
            .on_hover_text(tip)
            .clicked()
        {
            self.dark = !self.dark;
            ui.ctx().set_theme(if self.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            self.cfg.theme = if self.dark { "dark" } else { "light" }.into();
            let _ = self.cfg.save();
        }
    }

    fn project_field(&mut self, ui: &mut egui::Ui, running: bool) {
        {
            ui.label(group_text("Project"));
            let mut browse = false;
            let resp = field_with_button(
                ui,
                "Browse…",
                !running && !self.picking,
                &mut browse,
                |ui, w| {
                    ui.add_enabled(
                        !running,
                        egui::TextEdit::singleline(&mut self.path)
                            .hint_text(prompt("Type a folder path or drag a folder here"))
                            .desired_width(w),
                    )
                },
            );
            if resp.lost_focus() && self.path.trim() != self.scanned_path {
                self.scan();
            }
            if browse {
                self.browse(Pick::Project);
            }
        }
    }

    fn project_summary(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        if let Some(e) = self.scan_error.clone() {
            note(ui, LogKind::Warn, &e);
        } else if let Some(e) = self.pick_note.clone() {
            note(ui, LogKind::Warn, &e);
        } else if let Some(p) = &self.project {
            let missing: usize = p.languages.values().map(|l| l.untranslated()).sum();
            let files: usize = p.languages.values().map(|l| l.files.len()).sum();
            ui.label(
                RichText::new(format!(
                    "{} languages · {} translation files · {} strings to translate",
                    p.languages.len(),
                    files,
                    thousands(missing)
                ))
                .weak(),
            );
        } else {
            ui.label(
                RichText::new("Choose the folder of a project that has .po translation files.")
                    .weak(),
            );
        }
        ui.add_space(8.0);
    }

    fn service_panel(&mut self, ui: &mut egui::Ui) {
        group(ui, "Translation service");
        let old = self.provider;
        let installed = |k: ProviderKind, det: &[Detected]| det.iter().any(|d| d.kind == k);
        egui::ComboBox::from_id_salt("provider")
            .width(ui.available_width())
            .selected_text(service_label(self.provider))
            .show_ui(ui, |ui| {
                for k in ProviderKind::ALL {
                    let mut text = service_label(k).to_string();
                    if installed(k, &self.detected) {
                        text.push_str("  ✔");
                    }
                    ui.selectable_value(&mut self.provider, k, text);
                }
            });
        if old != self.provider {
            self.model_note = None;
            self.ensure_lt_langs();
        }
        let kind = self.provider;
        let det = self
            .detected
            .iter()
            .find(|d| d.kind == kind)
            .map(|d| d.how.clone());
        match (kind.cli_binary(), &det) {
            (Some(bin), Some(_)) => help(ui, &format!("Uses the {} command installed on this computer. No API key is needed.", bin)),
            (Some(bin), None) => note(
                ui,
                LogKind::Warn,
                &format!("The {} command isn't installed on this computer. Install it, or choose another service.", bin),
            ),
            _ => {}
        }

        if kind.is_llm() {
            ui.add_space(4.0);
            ui.label("Model");
            let list: Vec<String> = self.models.get(&kind).cloned().unwrap_or_else(|| {
                kind.suggested_models()
                    .iter()
                    .map(|m| m.to_string())
                    .collect()
            });
            let custom = self.custom_model.contains(&kind);
            let s = self.settings.get_mut(&kind).unwrap();
            let shown = if custom {
                "Other model…".to_string()
            } else {
                model_label(&s.model)
            };
            let can_refresh = !kind.is_cli();
            let mut set_custom = None;
            let mut want_refresh = false;
            ui.horizontal(|ui| {
                let w = if can_refresh {
                    ui.available_width()
                        - button_width(ui, "■  Cancel").max(button_width(ui, "Refresh"))
                        - ui.spacing().item_spacing.x
                        - 1.0
                } else {
                    ui.available_width()
                };
                egui::ComboBox::from_id_salt(("model", kind.id()))
                    .width(w)
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for m in &list {
                            if ui
                                .selectable_label(!custom && s.model == *m, model_label(m))
                                .clicked()
                            {
                                s.model = m.clone();
                                set_custom = Some(false);
                            }
                        }
                        ui.separator();
                        if ui.selectable_label(custom, "Other model…").clicked() {
                            set_custom = Some(true);
                        }
                    });
                if can_refresh && self.models_loading {
                    // The same button stops the request; its answer is then ignored.
                    if ui.add(stop_button(ui, "Cancel")).on_hover_text("Stop getting the model list").clicked() {
                        self.models_req += 1;
                        self.models_loading = false;
                    }
                } else if can_refresh {
                    let b = ui.add(egui::Button::new("Refresh"));
                    if b.on_hover_text("Get the current list of models from the service")
                        .clicked()
                    {
                        want_refresh = true;
                    }
                }
            });
            match set_custom {
                Some(true) => {
                    self.custom_model.insert(kind);
                }
                Some(false) => {
                    self.custom_model.remove(&kind);
                }
                None => {}
            }
            if self.custom_model.contains(&kind) {
                let s = self.settings.get_mut(&kind).unwrap();
                ui.add(
                    egui::TextEdit::singleline(&mut s.model)
                        .hint_text(prompt("Model name"))
                        .desired_width(f32::INFINITY),
                );
            }
            if want_refresh {
                self.refresh_models();
            }
            if self.models_loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Getting models…").weak());
                });
            } else if let Some((k, n)) = self.model_note.clone() {
                note(ui, k, &n);
            }
        }

        if kind.needs_key() || kind.uses_url() {
            ui.add_space(4.0);
            ui.label("API key");
            let s = self.settings.get_mut(&kind).unwrap();
            let env = kind
                .key_env()
                .iter()
                .find(|e| std::env::var(e).map(|v| !v.is_empty()).unwrap_or(false));
            // The public libretranslate.com server needs a key; offer to get one.
            let url = s.url.trim().trim_end_matches('/');
            let public_lt = kind == ProviderKind::Libretranslate
                && (url.is_empty() || url.eq_ignore_ascii_case(LT_PUBLIC))
                && s.api_key.is_empty()
                && env.is_none();
            if public_lt {
                ui.horizontal(|ui| {
                    let mut get = false;
                    field_with_button(ui, "Get API key", true, &mut get, |ui, w| {
                        ui.add(
                            egui::TextEdit::singleline(&mut s.api_key)
                                .password(true)
                                .desired_width(w),
                        )
                    });
                    if get {
                        open_url(LT_PORTAL);
                    }
                });
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut s.api_key)
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
            }
            if s.api_key.is_empty() {
                if let Some(e) = env {
                    help(
                        ui,
                        &format!(
                            "Leave empty to use the key in the {} environment variable.",
                            e
                        ),
                    );
                }
            }
            ui.checkbox(&mut self.remember_key, "Remember API key");
        }
        if kind.uses_url() {
            ui.add_space(4.0);
            ui.label("Server address");
            let s = self.settings.get_mut(&kind).unwrap();
            let r = ui.add(
                egui::TextEdit::singleline(&mut s.url)
                    .hint_text(prompt(kind.default_url()))
                    .desired_width(f32::INFINITY),
            );
            if r.lost_focus() {
                self.ensure_lt_langs();
            }
            if kind == ProviderKind::Libretranslate && self.lt_loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Checking which languages the server supports…").weak());
                });
            }
        }
    }

    fn options_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        // "Options" opens and closes the section.
        let arrow = if self.more_options { "⏷" } else { "⏵" };
        let header = egui::Button::new(group_text(&format!("{}  Options", arrow))).frame(false);
        if ui.add(header).on_hover_text(if self.more_options { "Hide the options" } else { "Show the options" }).clicked() {
            self.more_options = !self.more_options;
        }
        if !self.more_options {
            return;
        }
        ui.add_space(2.0);
        if ui
            .checkbox(&mut self.include_fuzzy, "Redo fuzzy translations")
            .on_hover_text("Translate again the strings marked fuzzy (waiting for review), replacing their current translation")
            .changed()
        {
            self.scan();
        }
        ui.add_space(4.0);
        ui.label("Language of the original text");
        egui::ComboBox::from_id_salt("srclang")
            .width(ui.available_width())
            .selected_text(format!(
                "{} ({})",
                langs::name(&self.source_lang),
                self.source_lang
            ))
            .show_ui(ui, |ui| {
                let mut all: Vec<_> = langs::NAMES
                    .iter()
                    .filter(|(c, _)| !c.contains('_'))
                    .collect();
                all.sort_by_key(|(_, n)| *n);
                for (code, name) in all {
                    ui.selectable_value(
                        &mut self.source_lang,
                        code.to_string(),
                        format!("{} ({})", name, code),
                    );
                }
            });
        ui.add_space(2.0);
        egui::Grid::new("numbers")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Strings per request");
                ui.add(
                    egui::DragValue::new(&mut self.batch_size)
                        .range(0..=200)
                        .custom_formatter(|v, _| {
                            if v == 0.0 {
                                "Automatic".into()
                            } else {
                                format!("{}", v)
                            }
                        }),
                );
                ui.end_row();
                ui.label("Translate at most");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.limit)
                            .range(0..=100000)
                            .custom_formatter(|v, _| {
                                if v == 0.0 {
                                    "All".into()
                                } else {
                                    format!("{}", v)
                                }
                            })
                            .custom_parser(|s| {
                                if s.trim().eq_ignore_ascii_case("all") {
                                    Some(0.0)
                                } else {
                                    s.trim().parse().ok()
                                }
                            }),
                    );
                    ui.label("strings");
                });
                ui.end_row();
            });
        let is_llm = self.provider.is_llm();
        if is_llm {
            ui.add_space(8.0);
            ui.label("Source code folder");
            let mut browse = false;
            ui.horizontal(|ui| {
                field_with_button(ui, "Browse…", !self.picking, &mut browse, |ui, w| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.source_root)
                            .hint_text(prompt("Not found"))
                            .desired_width(w),
                    )
                })
            });
            if browse {
                self.browse(Pick::SourceRoot);
            }
            help(
                ui,
                "The code around each string is sent as part of the context (where applicable).",
            );
            ui.add_space(4.0);
            ui.label("About this software");
            ui.add(
                egui::TextEdit::multiline(&mut self.summary)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
            help(
                ui,
                "A short description helps the translation service pick the right words. Filled in from the README.",
            );
            ui.add_space(4.0);
            ui.label("Extra glossary");
            let mut browse = false;
            ui.horizontal(|ui| {
                field_with_button(ui, "Browse…", !self.picking, &mut browse, |ui, w| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.glossary)
                            .hint_text(prompt("None"))
                            .desired_width(w),
                    )
                })
            });
            if browse {
                self.browse(Pick::Glossary);
            }
            help(ui, "A CSV, TSV or .po file of preferred terms. Terms already translated in the project are always used.");
        }
    }

    fn languages_panel(&mut self, ui: &mut egui::Ui) {
        let Some(proj) = self.project.clone() else {
            return;
        };
        let running = self.running();
        ui.add_space(10.0);
        // Selection buttons and search on the left, sorting on the right. On
        // narrow windows the row splits in two, keeping the sort control right-aligned.
        let sp = ui.spacing().item_spacing.x;
        let combo_w = button_width(ui, "Least complete") + 16.0;
        let sort_w =
            button_width(ui, "Sort by") - ui.spacing().button_padding.x * 2.0 + sp + combo_w;
        let buttons_w =
            button_width(ui, "Select incomplete") + button_width(ui, "Clear selection") + sp;
        let find_w = 140.0 + 8.0;
        let avail = ui.available_width();
        let one_row = buttons_w + sp + find_w + 12.0 + sort_w <= avail;
        let buttons = |ui: &mut egui::Ui, this: &mut Self| {
            ui.add_enabled_ui(!running, |ui| {
                if ui.button("Select incomplete").clicked() {
                    for (k, l) in &proj.languages {
                        if l.untranslated() > 0 && this.lt_supported(k) != Some(false) {
                            this.selected.insert(k.clone());
                        }
                    }
                }
                if ui
                    .add_enabled(
                        !this.selected.is_empty(),
                        egui::Button::new("Clear selection"),
                    )
                    .clicked()
                {
                    this.selected.clear();
                }
            });
        };
        let find = |ui: &mut egui::Ui, this: &mut Self| {
            ui.add(
                egui::TextEdit::singleline(&mut this.filter)
                    .hint_text(prompt("Find a language"))
                    .desired_width(140.0),
            );
        };
        let sort = |ui: &mut egui::Ui, this: &mut Self| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("sort")
                    .width(combo_w)
                    .selected_text(this.sort.label())
                    .show_ui(ui, |ui| {
                        for s in [Sort::Name, Sort::MostComplete, Sort::LeastComplete] {
                            ui.selectable_value(&mut this.sort, s, s.label());
                        }
                    });
                ui.label("Sort by");
            });
        };
        if one_row {
            ui.horizontal(|ui| {
                buttons(ui, self);
                find(ui, self);
                sort(ui, self);
            });
        } else {
            ui.horizontal(|ui| buttons(ui, self));
            if find_w + 16.0 + sort_w <= avail {
                ui.horizontal(|ui| {
                    find(ui, self);
                    sort(ui, self);
                });
            } else {
                ui.horizontal(|ui| find(ui, self));
                ui.horizontal(|ui| sort(ui, self));
            }
        }
        ui.add_space(4.0);

        let f = self.filter.trim().to_lowercase();
        let mut shown: Vec<_> = proj
            .languages
            .iter()
            .filter(|(c, l)| {
                f.is_empty() || c.to_lowercase().contains(&f) || l.name.to_lowercase().contains(&f)
            })
            .collect();
        // Completeness is measured by the strings still to translate.
        match self.sort {
            Sort::Name => shown.sort_by(|a, b| a.1.name.cmp(&b.1.name)),
            Sort::MostComplete => shown.sort_by(|a, b| {
                a.1.untranslated()
                    .cmp(&b.1.untranslated())
                    .then_with(|| a.1.name.cmp(&b.1.name))
            }),
            Sort::LeastComplete => shown.sort_by(|a, b| {
                b.1.untranslated()
                    .cmp(&a.1.untranslated())
                    .then_with(|| a.1.name.cmp(&b.1.name))
            }),
        }
        // Size cells to the longest entry so nothing is cut off.
        let body = TextStyle::Body.resolve(ui.style());
        let small = TextStyle::Small.resolve(ui.style());
        let width_of = |t: &str, font: &FontId| {
            ui.fonts(|fo| {
                fo.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
                    .size()
                    .x
            })
        };
        let name_w = shown
            .iter()
            .map(|(_, l)| width_of(&l.name, &body))
            .fold(80.0, f32::max);
        let lt = self.provider == ProviderKind::Libretranslate;
        let count_w = width_of(if lt { "Not available" } else { "Complete" }, &small);
        let fuzzy_w = width_of("⚠ 9,999", &small) + 6.0;
        let bar_w = 70.0 + 8.0 + width_of("100%", &small);
        let min_cell = 30.0 + name_w + 12.0 + count_w;
        let gap = 20.0;
        let avail = ui.available_width() - ui.spacing().scroll.bar_width - 8.0;
        // Always a single column; the row spans the full width so the numbers line up on the right.
        let cols = 1;
        let cell_w = avail.max(min_cell.min(avail));

        let mut open_editor: Option<String> = None;
        egui::ScrollArea::vertical()
            .id_salt("langs")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if shown.is_empty() {
                    ui.label(
                        RichText::new(format!("No languages match \"{}\".", self.filter.trim()))
                            .weak(),
                    );
                }
                egui::Grid::new("langgrid")
                    .striped(true)
                    .num_columns(cols)
                    .spacing([gap, 2.0])
                    .min_col_width(cell_w)
                    .max_col_width(cell_w)
                    .show(ui, |ui| {
                        for (i, (code, l)) in shown.iter().enumerate() {
                            let supported = self.lt_supported(code);
                            let u = l.untranslated();
                            let mut on = self.selected.contains(*code);
                            let enabled = supported != Some(false) && !running;
                            ui.allocate_ui_with_layout(
                                egui::vec2(cell_w, 24.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.set_width(cell_w);
                                    let resp = ui.add_enabled(
                                        enabled,
                                        egui::Checkbox::new(&mut on, l.name.as_str()),
                                    );
                                    let files = l
                                        .files
                                        .iter()
                                        .map(|f| {
                                            f.path.file_name().unwrap_or_default().to_string_lossy()
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    if supported == Some(false) {
                                        resp.on_disabled_hover_text(format!(
                                            "This LibreTranslate server can't translate {} ({}).",
                                            l.name, code
                                        ));
                                    } else {
                                        resp.on_hover_text(format!(
                                            "{} ({}): {}",
                                            l.name, code, files
                                        ));
                                    }
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui
                                            .add(egui::Button::new("Edit").small())
                                            .on_hover_text(format!(
                                                "Browse and edit the {} translations",
                                                l.name
                                            ))
                                            .clicked()
                                        {
                                            open_editor = Some(code.to_string());
                                        }
                                        ui.add_space(8.0);
                                        // Fixed-width cells so the bars line up.
                                        let cell = |ui: &mut egui::Ui, w: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
                                            ui.allocate_ui_with_layout(egui::vec2(w, 20.0), Layout::right_to_left(Align::Center), |ui| {
                                                ui.set_width(w);
                                                add(ui)
                                            });
                                        };
                                        let fz = l.fuzzy();
                                        // Icon first, left-aligned, so the icons form a column.
                                        cell(ui, fuzzy_w, &mut |ui| {
                                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                                if fz > 0 {
                                                    let tip = if fz == 1 {
                                                        "1 translation needs review".to_string()
                                                    } else {
                                                        format!("{} translations need review", thousands(fz))
                                                    };
                                                    ui.label(icon(LogKind::Warn).small()).on_hover_text(&tip);
                                                    ui.label(RichText::new(thousands(fz)).small().weak()).on_hover_text(&tip);
                                                }
                                            });
                                        });
                                        ui.add_space(6.0);
                                        let t = if supported == Some(false) {
                                            "Not available".to_string()
                                        } else if u == 0 {
                                            "Complete".to_string()
                                        } else {
                                            thousands(u)
                                        };
                                        cell(ui, count_w, &mut |ui| {
                                            ui.label(RichText::new(t.clone()).small().weak())
                                                .on_hover_text("Strings to translate");
                                        });
                                        ui.add_space(14.0);
                                        cell(ui, bar_w, &mut |ui| {
                                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                                completion_bar(ui, l.reviewed(), fz, l.total(), 70.0);
                                            });
                                        });
                                    });
                                },
                            );
                            if on {
                                self.selected.insert(code.to_string());
                            } else {
                                self.selected.remove(*code);
                            }
                            if (i + 1) % cols == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });
        if let Some(code) = open_editor {
            let same = self.editor.as_ref().is_some_and(|e| e.lang == code);
            if same {
                self.tab = Tab::Editor;
            } else {
                self.request(Pending::OpenEditor(code));
            }
        }
    }

    fn action_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        let n = self.pending_count();
        let mut status = String::new();
        ui.add_space(10.0);
        // Button on the right, status text to its left.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if running {
                    let stopping = self
                        .job
                        .as_ref()
                        .map(|j| j.cancel.load(Ordering::Relaxed))
                        .unwrap_or(false);
                    let b = stop_button(ui, if stopping { "Stopping…" } else { "Stop" })
                        .min_size(egui::vec2(160.0, 32.0));
                    if ui.add_enabled(!stopping, b).on_hover_text("Stop translating; what's done so far is kept").clicked() {
                        if let Some(j) = &self.job {
                            j.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                } else {
                    let label = match n {
                        0 => "Translate".to_string(),
                        1 => "Translate 1 string".to_string(),
                        n => format!("Translate {} strings", thousands(n)),
                    };
                    let b = egui::Button::new(
                        RichText::new(label)
                            .color(Color32::WHITE)
                            .family(semibold()),
                    )
                    .fill(if n > 0 {
                        ACCENT
                    } else if ui.visuals().dark_mode {
                        ACCENT.gamma_multiply(0.45)
                    } else {
                        // Disabled widgets are drawn faded, so start from a solid mid blue.
                        Color32::from_rgb(70, 130, 200)
                    })
                    .min_size(egui::vec2(160.0, 32.0));
                    let shortcut = ui.ctx().format_shortcut(&TRANSLATE_KEY);
                    let resp = ui
                        .add_enabled(n > 0, b)
                        .on_hover_text(format!("Shortcut: {}", shortcut));
                    if resp.clicked()
                        || (n > 0 && ui.input_mut(|i| i.consume_shortcut(&TRANSLATE_KEY)))
                    {
                        self.start(true);
                    }
                }
                let sel = self
                    .selected
                    .iter()
                    .filter(|l| self.lt_supported(l) != Some(false))
                    .count();
                let text = if running {
                    self.job
                        .as_ref()
                        .map(|j| progress_detail(j))
                        .unwrap_or_default()
                } else if sel == 0 {
                    "Select one or more languages to translate.".to_string()
                } else if n == 0 {
                    "The selected languages are already complete.".to_string()
                } else {
                    format!(
                        "{} {} selected.",
                        sel,
                        if sel == 1 { "language" } else { "languages" }
                    )
                };
                status = text;
                if ui.available_width() >= 220.0 {
                    ui.add(
                        egui::Label::new(RichText::new(std::mem::take(&mut status)).weak()).wrap(),
                    );
                }
            })
        });
        if !status.is_empty() {
            ui.add(egui::Label::new(RichText::new(status).weak()).wrap());
        }
        if let Some(job) = &mut self.job {
            ui.add_space(6.0);
            if job.finished.is_none() {
                let frac = if job.total == 0 {
                    0.0
                } else {
                    job.done as f32 / job.total as f32
                };
                ui.add(egui::ProgressBar::new(frac).desired_height(10.0));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} of {} strings",
                            thousands(job.done),
                            thousands(job.total)
                        ))
                        .weak(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(t) = eta(job) {
                            ui.label(RichText::new(t).weak());
                        }
                    });
                });
                if let Some(t) = job.current.first() {
                    let n = job.current.len();
                    let first = flatten(t);
                    let first = if first.chars().count() > 70 {
                        format!("{}…", first.chars().take(70).collect::<String>())
                    } else {
                        first
                    };
                    let text = if n == 1 {
                        format!("Now translating “{}”", first)
                    } else {
                        format!("Now translating {} strings, starting with “{}”", n, first)
                    };
                    ui.add(egui::Label::new(RichText::new(text).weak()).wrap());
                }
            } else if let Some((kind, text)) = job.finished.as_ref().and_then(run_problems) {
                // Only worth a message when something went wrong.
                note(ui, kind, &text);
            }
        }
        ui.add_space(6.0);
    }

    /// The Results tab: the translations of the last runs, waiting for the
    /// user to accept them or mark them for review; and what was rejected.
    fn results(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        if self.log.is_empty() {
            let text = if self.running() { "Translations will appear here." } else { "Nothing is waiting for review." };
            ui.label(RichText::new(text).weak());
            return;
        }
        // Toolbar
        let proposals = self.log.iter().filter(|l| l.is_proposal()).count();
        let mut bulk: Option<u8> = None; // 0 accept all, 1 mark all for review, 2 clear
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 0.0), Layout::right_to_left(Align::Center), |ui| {
            // Right to left: Clear, then Mark all for review, then Accept all.
            if ui
                .add(egui::Button::new("×  Clear"))
                .on_hover_text("Remove everything from this list without saving it")
                .clicked()
            {
                bulk = Some(2);
            }
            let review = egui::Button::new(RichText::new("⚠  Mark all for review").color(review_ink(ui))).fill(review_fill(ui));
            if ui
                .add_enabled(proposals > 0, review)
                .on_hover_text("Save all these translations to the files, marked as needing review")
                .clicked()
            {
                bulk = Some(1);
            }
            let accept = egui::Button::new(RichText::new("✔  Accept all").color(Color32::WHITE)).fill(ACCENT);
            if ui
                .add_enabled(proposals > 0, accept)
                .on_hover_text("Save all these translations to the files as reviewed")
                .clicked()
            {
                bulk = Some(0);
            }
        });
        ui.add_space(6.0);
        let gap = 10.0;
        let icon_w = 16.0;
        let body = TextStyle::Body.resolve(ui.style());
        let width = |t: &str, f: &egui::FontId| {
            ui.fonts(|fo| fo.layout_no_wrap(t.into(), f.clone(), Color32::WHITE).size().x)
        };
        let lang_w = self
            .log
            .iter()
            .map(|l| width(&l.lang, &body))
            .fold(width("Language", &TextStyle::Small.resolve(ui.style())), f32::max)
            .min(160.0)
            + 2.0;
        let small = egui::TextStyle::Button.resolve(ui.style());
        let actions_w = width("✔", &small) + width("Edit", &small) + 2.0 * ui.spacing().button_padding.x * 2.0 + gap;
        let text_w = ((ui.available_width() - icon_w - lang_w - actions_w - gap * 4.0 - 14.0) / 2.0).max(120.0);
        let header = |t: &str| RichText::new(t).small().weak();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            cell(ui, icon_w, RichText::new(""));
            cell(ui, lang_w, header("Language"));
            cell(ui, text_w, header("Original text"));
            cell(ui, text_w, header("Translation"));
        });
        ui.separator();
        let stripe = ui.visuals().faint_bg_color;
        let (mut accept, mut edit) = (None, None);
        egui::ScrollArea::vertical()
            .id_salt("results")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, l) in self.log.iter().enumerate() {
                    let fill = if i % 2 == 1 { stripe } else { Color32::TRANSPARENT };
                    egui::Frame::none().fill(fill).inner_margin(egui::Margin::symmetric(0.0, 3.0)).show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            cell(ui, icon_w, icon(l.kind));
                            if l.lang.is_empty() {
                                cell(ui, lang_w + text_w * 2.0 + gap * 2.0, RichText::new(&l.a));
                                return;
                            }
                            cell(ui, lang_w, RichText::new(&l.lang).weak());
                            cell(ui, text_w, RichText::new(&l.a));
                            let t = RichText::new(&l.b);
                            ui.allocate_ui_with_layout(egui::vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                                ui.set_width(text_w);
                                ui.add(egui::Label::new(if l.kind == LogKind::Ok { t } else { t.weak() }).wrap());
                                if let Some(p) = &l.problem {
                                    ui.add(egui::Label::new(RichText::new(p).small().color(RED)).wrap());
                                }
                            });
                            if l.entry.is_none() {
                                return;
                            }
                            ui.allocate_ui_with_layout(egui::vec2(actions_w, 0.0), Layout::right_to_left(Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let tip = if l.is_proposal() {
                                    format!("Save it marked as needing review, and open it in the {} translations", l.lang)
                                } else {
                                    format!("Open this string in the {} translations", l.lang)
                                };
                                if ui.button("Edit").on_hover_text(tip).clicked() {
                                    edit = Some(i);
                                }
                                if l.is_proposal() {
                                    let b = egui::Button::new(RichText::new("✔").color(GREEN));
                                    if ui.add(b).on_hover_text("Accept: save this translation to the file as reviewed").clicked() {
                                        accept = Some(i);
                                    }
                                }
                            });
                        });
                    });
                }
            });
        match bulk {
            Some(2) => self.log.clear(),
            Some(b) => {
                let all: Vec<usize> = (0..self.log.len()).filter(|&i| self.log[i].is_proposal()).collect();
                self.save_proposals(&all, b == 1);
            }
            None => {}
        }
        if let Some(i) = accept {
            self.save_proposals(&[i], false);
        }
        if let Some(i) = edit {
            let r = self.log[i].entry.clone().unwrap();
            if self.log[i].is_proposal() {
                // Saved for review first, so the editor shows it.
                self.save_proposals(&[i], true);
            }
            // No "Save changes?" question when the string is in the file already open.
            let same_file = self.editor.as_ref().is_some_and(|e| e.lang == r.code && *e.path() == r.path);
            if same_file {
                self.perform(Pending::EditEntry(r));
            } else {
                self.request(Pending::EditEntry(r));
            }
        }
    }

    /// Writes the proposals at `rows` to their files: as reviewed, or (if
    /// `review`) marked as needing review with the model note. Saved rows leave
    /// the list; the others say why they weren't saved.
    fn save_proposals(&mut self, rows: &[usize], review: bool) {
        let mut by_file: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for &i in rows {
            if let Some(r) = self.log.get(i).filter(|l| l.is_proposal()).and_then(|l| l.entry.as_ref()) {
                by_file.entry(r.path.clone()).or_default().push(i);
            }
        }
        let mut done: BTreeSet<usize> = BTreeSet::new();
        for (path, idx) in &by_file {
            let items: Vec<(Option<String>, String, Vec<String>)> = idx
                .iter()
                .map(|&i| {
                    let l = &self.log[i];
                    let r = l.entry.as_ref().unwrap();
                    (r.msgctxt.clone(), r.msgid.clone(), l.forms.clone())
                })
                .collect();
            // Every row of a run has the same note.
            let note = self.log[idx[0]].note.clone();
            match crate::po::PoFile::write_proposals(path, &items, review, note.as_deref()) {
                Ok(outcome) => {
                    for (&i, w) in idx.iter().zip(outcome) {
                        match w {
                            crate::po::Written::Ok => {
                                done.insert(i);
                            }
                            crate::po::Written::Missing => self.log[i].problem = Some("This string is no longer in the file.".into()),
                            crate::po::Written::Taken => {
                                self.log[i].problem = Some("It was translated and reviewed in the meantime, so it was left as it is.".into())
                            }
                        }
                    }
                }
                Err(e) => {
                    for &i in idx {
                        self.log[i].problem = Some(format!("Unable to save {}: {}", path.display(), e));
                    }
                }
            }
        }
        let mut i = 0;
        self.log.retain(|_| {
            let keep = !done.contains(&i);
            i += 1;
            keep
        });
        // Refresh the counts, and an open editor that has nothing unsaved.
        let sel = self.selected.clone();
        self.scan();
        self.selected = sel;
        if let Some(ed) = &mut self.editor {
            if !ed.dirty() && by_file.contains_key(ed.path()) {
                let _ = ed.load();
            }
        }
    }
}

/// Colors of "Mark all for review": a soft amber, like strings needing review.
fn review_fill(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(78, 60, 28)
    } else {
        Color32::from_rgb(252, 238, 210)
    }
}

fn review_ink(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(244, 205, 140)
    } else {
        Color32::from_rgb(130, 82, 10)
    }
}

// ---------------------------------------------------------------------------
// Helpers

fn service_label(k: ProviderKind) -> &'static str {
    match k {
        ProviderKind::ClaudeCli => "Claude Code on this computer",
        ProviderKind::GeminiCli => "Gemini CLI on this computer",
        ProviderKind::CodexCli => "Codex CLI on this computer",
        ProviderKind::Anthropic => "Claude (Anthropic API key)",
        ProviderKind::Google => "Gemini (Google API key)",
        ProviderKind::Openai => "OpenAI (API key)",
        ProviderKind::OpenaiCompatible => "OpenAI compatible (llama-server, OpenRouter, ...)",
        ProviderKind::Libretranslate => "LibreTranslate",
    }
}

fn model_label(m: &str) -> String {
    match m {
        "" => "Service default".into(),
        "opus" => "Opus (newest version)".into(),
        "sonnet" => "Sonnet (newest version)".into(),
        "haiku" => "Haiku (newest version)".into(),
        m => m.into(),
    }
}

fn prompt(t: &str) -> RichText {
    RichText::new(t).italics()
}

fn group_text(t: &str) -> RichText {
    RichText::new(t).text_style(TextStyle::Name("group".into()))
}

fn group(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(group_text(title));
    ui.add_space(2.0);
}

fn help(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).small().weak()).wrap());
}

fn icon(kind: LogKind) -> RichText {
    match kind {
        LogKind::Ok => RichText::new("✔").color(GREEN),
        LogKind::Warn => RichText::new("⚠").color(AMBER),
        LogKind::Err => RichText::new("✖").color(RED),
        LogKind::Info => RichText::new("ℹ").weak(),
    }
}

/// A message with its standard icon; the text itself stays in the normal color.
fn note(ui: &mut egui::Ui, kind: LogKind, text: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(icon(kind));
        ui.add(egui::Label::new(text).wrap());
    });
}

/// How complete a language or file is: reviewed (accent), waiting for review
/// (amber), not translated (track), with the reviewed percentage after it.
fn completion_bar(ui: &mut egui::Ui, reviewed: usize, fuzzy: usize, total: usize, width: f32) {
    let pct = |n: usize| {
        if total == 0 {
            0.0
        } else {
            n as f32 * 100.0 / total as f32
        }
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, ui.visuals().widgets.inactive.weak_bg_fill);
    let w = |n: usize| rect.width() * pct(n) / 100.0;
    let done = egui::Rect::from_min_size(rect.min, egui::vec2(w(reviewed), rect.height()));
    let review = egui::Rect::from_min_size(
        egui::pos2(done.max.x, rect.min.y),
        egui::vec2(w(fuzzy), rect.height()),
    );
    p.rect_filled(review.union(done), 3.0, AMBER);
    p.rect_filled(done, 3.0, ACCENT);
    let untranslated = total.saturating_sub(reviewed + fuzzy);
    resp.on_hover_text(format!(
        "{:.0}% reviewed ({} of {})\n{:.0}% waiting for review\n{:.0}% not translated",
        pct(reviewed).floor(),
        thousands(reviewed),
        thousands(total),
        pct(fuzzy),
        pct(untranslated)
    ));
    ui.label(
        RichText::new(format!("{:.0}%", pct(reviewed).floor()))
            .small()
            .weak(),
    );
}

fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = TextStyle::Button.resolve(ui.style());
    ui.fonts(|f| {
        f.layout_no_wrap(text.to_string(), font, Color32::WHITE)
            .size()
            .x
    }) + ui.spacing().button_padding.x * 2.0
}

/// A text field that fills the row, followed by a button anchored on the right.
/// Dims and blocks the window behind a dialog.
fn modal_backdrop(ctx: &egui::Context) {
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("modal-backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, egui::Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(110));
        });
}

fn field_with_button<R>(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    clicked: &mut bool,
    field: impl FnOnce(&mut egui::Ui, f32) -> R,
) -> R {
    let bw = button_width(ui, label);
    // TextEdit's desired width excludes its inner margin (4 px each side).
    let w = (ui.available_width() - bw - ui.spacing().item_spacing.x - 8.0).max(60.0);
    let r = field(ui, w);
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        *clicked = true;
    }
    r
}

fn cell(ui: &mut egui::Ui, w: f32, text: RichText) {
    ui.allocate_ui_with_layout(egui::vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(w);
        ui.add(egui::Label::new(text).wrap());
    });
}

fn flatten(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "fetching x" -> "Fetching x."
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    let mut out = match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    };
    if !out.ends_with(['.', '!', '?', ')']) {
        out.push('.');
    }
    out
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn progress_detail(j: &Job) -> String {
    if j.lang.is_empty() {
        "Getting ready…".into()
    } else {
        format!("Translating {} · {}", j.lang, j.file)
    }
}

/// A time-remaining estimate, shown only once it can be reasonably accurate
/// (after a few seconds and 10% of the work) and smoothed so it doesn't jump.
fn eta(job: &mut Job) -> Option<String> {
    let el = job.started.elapsed().as_secs_f32();
    if job.total == 0
        || job.done == 0
        || job.done >= job.total
        || el < 8.0
        || (job.done as f32) < job.total as f32 * 0.1
    {
        return None;
    }
    let raw = el / job.done as f32 * (job.total - job.done) as f32;
    let e = match job.eta {
        Some(prev) => prev * 0.9 + raw * 0.1,
        None => raw,
    };
    job.eta = Some(e);
    Some(if e < 50.0 {
        "Less than a minute left".into()
    } else if e < 90.0 {
        "About 1 minute left".into()
    } else if e < 3600.0 {
        format!("About {} minutes left", (e / 60.0).round())
    } else {
        format!("About {:.1} hours left", e / 3600.0)
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// What went wrong in a finished run, if anything.
fn run_problems(s: &Summary) -> Option<(LogKind, String)> {
    if let Some(f) = &s.fatal {
        return Some((
            LogKind::Err,
            format!("Translation stopped because of a problem. {}", sentence(f)),
        ));
    }
    let mut text = Vec::new();
    if s.rejected > 0 {
        text.push(format!(
            "{} didn't pass the quality checks and {} left untranslated; use Edit to translate {} yourself.",
            plural(s.rejected, "translation", "translations"),
            if s.rejected == 1 { "was" } else { "were" },
            if s.rejected == 1 { "it" } else { "them" },
        ));
    }
    if s.failed > 0 {
        text.push(format!(
            "{} couldn't be sent to the service.",
            plural(s.failed, "string", "strings")
        ));
    }
    (!text.is_empty()).then(|| (LogKind::Warn, text.join(" ")))
}

/// Opens the system's folder/file chooser without extra dependencies.
/// Ok(None) means the person closed it without choosing.
const LT_PUBLIC: &str = "https://libretranslate.com";
const LT_PORTAL: &str = "https://portal.libretranslate.com";

/// Opens a web page in the default browser.
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    let _ = r;
}

fn pick_path(folder: bool, title: &str) -> Result<Option<String>, String> {
    use std::process::Command;
    let finish = |out: std::process::Output| {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(if out.status.success() && !p.is_empty() {
            Some(p)
        } else {
            None
        })
    };
    #[cfg(target_os = "macos")]
    {
        let what = if folder { "folder" } else { "file" };
        let out = Command::new("osascript")
            .arg("-e")
            .arg(format!("POSIX path of (choose {} with prompt \"{}\")", what, title))
            .output()
            .map_err(|_| "Unable to open the file browser. Type the path instead, or drag it onto this window.".to_string())?;
        finish(out).map(|p: Option<String>| {
            p.map(|p| {
                if folder && p.len() > 1 {
                    p.trim_end_matches('/').to_string()
                } else {
                    p
                }
            })
        })
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let script = if folder {
            format!("Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = '{}'; if ($d.ShowDialog() -eq 'OK') {{ $d.SelectedPath }}", title)
        } else {
            format!("Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Title = '{}'; if ($d.ShowDialog() -eq 'OK') {{ $d.FileName }}", title)
        };
        let out = Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", &script])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .map_err(|_| "Unable to open the file browser. Type the path instead, or drag it onto this window.".to_string())?;
        finish(out)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut z = Command::new("zenity");
        z.args(["--file-selection", &format!("--title={}", title)]);
        if folder {
            z.arg("--directory");
        }
        if let Ok(out) = z.output() {
            return finish(out);
        }
        let mut k = Command::new("kdialog");
        k.args([
            if folder {
                "--getexistingdirectory"
            } else {
                "--getopenfilename"
            },
            ".",
            "--title",
            title,
        ]);
        if let Ok(out) = k.output() {
            return finish(out);
        }
        Err("Unable to open a file browser because neither zenity nor kdialog is installed. Type the path instead, or drag it onto this window.".into())
    }
}

// ---------------------------------------------------------------------------

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        // Ask before quitting with unsaved edits.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.editor_dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.tab = Tab::Editor;
            self.pending = Some(Pending::Quit);
        }
        if self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.save_prompt(ctx);
        self.reload_prompt(ctx);
        self.dev_screenshot(ctx);
        // Shrink the default window to fit small screens.
        if !self.fitted {
            if let Some(mon) = ctx.input(|i| i.viewport().monitor_size) {
                self.fitted = true;
                let inner = ctx
                    .input(|i| i.viewport().inner_rect)
                    .map(|r| r.size())
                    .unwrap_or(egui::vec2(DEFAULT_SIZE[0], DEFAULT_SIZE[1]));
                let max = egui::vec2(mon.x - 32.0, mon.y - 48.0 - 40.0);
                if inner.x > max.x || inner.y > max.y {
                    let fit = egui::vec2(
                        inner.x.min(max.x).max(MIN_SIZE[0]),
                        inner.y.min(max.y).max(MIN_SIZE[1]),
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(fit));
                }
            }
        }
        let suggesting = self.editor.as_ref().is_some_and(|e| e.busy());
        if self.running() || self.models_loading || self.lt_loading || self.picking || suggesting {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(p) = dropped.first() {
            if !self.running() {
                self.path = p.display().to_string();
                self.scan();
            }
        }

        let margin = egui::Margin::symmetric(16.0, 0.0);
        let panel =
            |ctx: &egui::Context| egui::Frame::side_top_panel(&ctx.style()).inner_margin(margin);
        egui::TopBottomPanel::top("project")
            .frame(panel(ctx))
            .show(ctx, |ui| self.project_bar(ui));

        egui::SidePanel::left("settings")
            .frame(panel(ctx))
            .resizable(true)
            .default_width(300.0)
            .width_range(260.0..=420.0)
            .show_animated(ctx, !self.cfg.hide_settings, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_enabled_ui(!self.running(), |ui| {
                            self.service_panel(ui);
                            self.options_panel(ui);
                        });
                        ui.add_space(10.0);
                    });
            });

        let central = egui::Frame::central_panel(&ctx.style()).inner_margin(margin);
        egui::CentralPanel::default().frame(central).show(ctx, |ui| {
            if self.project.is_none() {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(RichText::new("Choose a project folder").text_style(TextStyle::Heading));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Pick the folder that contains your .po translation files, or drag it onto this window.").weak());
                    ui.add_space(10.0);
                    if ui.add_enabled(!self.picking, egui::Button::new("Browse…").min_size(egui::vec2(120.0, 30.0))).clicked() {
                        self.browse(Pick::Project);
                    }
                });
                return;
            }
            egui::TopBottomPanel::top("tabs").frame(egui::Frame::none()).show_separator_line(true).show_inside(ui, |ui| {
                unclip(ui);
                ui.add_space(8.0);
                let mut close = false;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Languages, "Languages");
                    if let Some(ed) = &self.editor {
                        let label = format!("{}{}", ed.name, if ed.dirty() { " •" } else { "" });
                        ui.selectable_value(&mut self.tab, Tab::Editor, label);
                        if ui.small_button("×").on_hover_text(format!("Close the {} translations", ed.name)).clicked() {
                            close = true;
                        }
                        ui.add_space(4.0);
                    }
                    if self.job.is_some() {
                        ui.selectable_value(&mut self.tab, Tab::Results, "Results");
                    }
                    ui.selectable_value(&mut self.tab, Tab::Console, "Console");
                });
                if close {
                    self.request(Pending::CloseEditor);
                }
                ui.add_space(4.0);
            });
            if self.tab == Tab::Editor && self.editor.is_some() {
                egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| {
                    unclip(ui);
                    self.editor_panel(ui)
                });
                return;
            }
            egui::TopBottomPanel::bottom("run")
                .frame(egui::Frame::none())
                .show_separator_line(true)
                .resizable(false)
                .show_inside(ui, |ui| {
                    unclip(ui);
                    self.action_bar(ui);
                });
            egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| {
                unclip(ui);
                match self.tab {
                Tab::Languages => self.languages_panel(ui),
                Tab::Results => self.results(ui),
                Tab::Console => self.console_panel(ui),
                Tab::Editor => self.languages_panel(ui),
                }
            });
        });
    }
}

/// Lets widgets at the edge of a borderless inner panel draw their hover
/// outline, which egui paints slightly outside the widget and the panel clips.
fn unclip(ui: &mut egui::Ui) {
    ui.set_clip_rect(ui.clip_rect().expand2(egui::vec2(4.0, 2.0)));
}

impl App {
    fn dev_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, t0, requested)) = &mut self.screenshot else {
            return;
        };
        let delay: f32 = std::env::var("POLIBRE_SCREENSHOT_DELAY")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(2.0);
        if !*requested && t0.elapsed().as_secs_f32() > delay {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            *requested = true;
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let mut data = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
            for p in &img.pixels {
                data.extend_from_slice(&[p.r(), p.g(), p.b()]);
            }
            let _ = std::fs::write(&*path, data);
            self.quitting = true; // no "Save changes?" prompt
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
