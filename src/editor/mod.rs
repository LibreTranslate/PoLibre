// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browse and edit the translations of one language (GUI only).
//!
//! Entries that need review (fuzzy) are highlighted; the ones PoLibre
//! filled also show the model that translated them.

mod diff;
mod memory;
mod reference;
mod source;

use super::{icon, note, prompt, thousands, LogKind, ACCENT, AMBER, GREEN, RED};
use crate::po::{Entry, Item, PoFile};
use crate::project::{Glossary, SourceCache};
use crate::validate::{self, Src};
use eframe::egui::{
    self, Align, Color32, Key, KeyboardShortcut, Layout, Modifiers, RichText, TextStyle,
};
use memory::{Match, Memory};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::SystemTime;

const PAGE: usize = 50;
pub const SAVE_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const APPROVE_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
const NEXT_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::ArrowDown);
const PREV_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::ArrowUp);
const REVIEW_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::U);
const SUGGEST_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::J);

/// Something done to one entry, from its buttons or a keyboard shortcut.
#[derive(Debug, Clone, PartialEq)]
enum Action {
    Approve,
    /// Empty the translation to type a new one.
    Discard,
    Suggest,
    /// Stop waiting for a suggestion (the window then cancels the request).
    CancelSuggest,
    ToggleReview,
    /// Take a translation from the translation memory.
    Use(String),
    /// Open or close the translator note editor.
    EditNote,
    CloseNote,
    /// Insert a placeholder at the cursor.
    Insert(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Show {
    All,
    Review,
    Untranslated,
    Translated,
    /// Translations that fail the automatic checks.
    Problems,
}

/// What the editor asks the window to do.
pub enum Request {
    None,
    SwitchFile(usize),
    Saved,
    /// Ask the translation service for one string; answer with [`Editor::suggested`].
    Suggest(Box<SuggestJob>),
}

/// Everything needed to translate one entry in the background.
pub struct SuggestJob {
    pub lang: String,
    /// All of the language's files (for the glossary).
    pub files: Vec<PathBuf>,
    pub path: PathBuf,
    pub idx: usize,
    pub entry: Entry,
    pub nplurals: usize,
    pub plural_expr: String,
}

/// A finished suggestion.
pub struct Suggestion {
    pub path: PathBuf,
    pub idx: usize,
    pub msgid: String,
    pub result: Result<Vec<String>, String>,
}

pub struct Editor {
    pub lang: String,
    pub name: String,
    pub files: Vec<PathBuf>,
    pub file: usize,
    po: PoFile,
    loaded_mtime: Option<SystemTime>,
    nplurals: usize,
    plural_expr: String,
    /// Item indices of the entries that can be edited.
    rows: Vec<usize>,
    changed: BTreeSet<usize>,
    show: Show,
    search: String,
    page: usize,
    problem: Option<String>,
    /// The file changed on disk after it was opened.
    conflict: bool,
    /// Project source folder that `#:` references point into, if known.
    source_root: Option<PathBuf>,
    sources: SourceCache,
    /// Entries whose source code context is shown.
    open_context: BTreeSet<usize>,
    /// Entries edited or approved in the current view: they stay listed even
    /// if they no longer match the filter, so they don't vanish while typing.
    keep: BTreeSet<usize>,
    /// Entry to show and focus once the list is laid out (see [`Editor::focus_entry`]).
    pending_go: Option<usize>,
    /// Entry whose first translation box gets the keyboard focus, and for how
    /// many more frames to ask (see [`Editor::focus_on`]).
    focus: Option<(usize, u8)>,
    /// Name of the translation service, for the Suggest button.
    pub service: String,
    /// Entries waiting for a suggestion, and suggestions that failed.
    suggesting: BTreeSet<usize>,
    suggest_errors: BTreeMap<usize, String>,
    /// Suggest button pressed this frame.
    want_suggest: Option<usize>,
    /// Reviewed translations of the language, and the matches found per entry.
    memory: Memory,
    matches: HashMap<usize, Vec<Match>>,
    /// Project terminology plus the user's glossary file, and the terms
    /// found in each entry's source (singular, plural).
    glossary: Glossary,
    glossary_file: Option<PathBuf>,
    glossary_error: Option<String>,
    marks: HashMap<usize, Marks>,
    /// The project's other languages (code, name, files), and the one shown
    /// for comparison.
    pub others: Vec<(String, String, Vec<PathBuf>)>,
    reference: Option<reference::Reference>,
    /// Entries whose translator note is open for editing, with the text typed.
    note_drafts: HashMap<usize, String>,
    /// Note box that gets the cursor next frame.
    note_focus: Option<usize>,
    /// Where the cursor was in each entry's translation (form, character).
    cursors: HashMap<usize, (usize, usize)>,
    /// Cursor to put into a translation box after inserting text (entry, form, character).
    place_cursor: Option<(usize, usize, usize)>,
    /// Why a translation fails the automatic checks, per entry.
    problems: HashMap<usize, String>,
    /// The entry being worked on (its translation box had the focus last);
    /// keyboard shortcuts act on it.
    current: Option<usize>,
}

/// Glossary terms and placeholders found in an entry's source (singular, plural).
struct Marks {
    terms: [Vec<source::Term>; 2],
    placeholders: [Vec<std::ops::Range<usize>>; 2],
}

/// Lines of code shown above and below a referenced line.
const CONTEXT_LINES: usize = 6;

fn mtime(p: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Editor {
    pub fn open(
        lang: &str,
        name: &str,
        files: Vec<PathBuf>,
        file: usize,
        source_root: Option<PathBuf>,
    ) -> Result<Editor, String> {
        let mut e = Editor {
            lang: lang.to_string(),
            name: name.to_string(),
            files,
            file,
            po: PoFile::parse(""),
            loaded_mtime: None,
            nplurals: 2,
            plural_expr: String::new(),
            rows: vec![],
            changed: BTreeSet::new(),
            show: Show::All,
            search: String::new(),
            page: 0,
            problem: None,
            conflict: false,
            source_root,
            sources: SourceCache::default(),
            open_context: BTreeSet::new(),
            keep: BTreeSet::new(),
            focus: None,
            pending_go: None,
            service: String::new(),
            suggesting: BTreeSet::new(),
            suggest_errors: BTreeMap::new(),
            want_suggest: None,
            memory: Memory::default(),
            matches: HashMap::new(),
            current: None,
            problems: HashMap::new(),
            note_drafts: HashMap::new(),
            note_focus: None,
            others: Vec::new(),
            reference: None,
            glossary: Glossary::default(),
            glossary_file: None,
            glossary_error: None,
            marks: HashMap::new(),
            cursors: HashMap::new(),
            place_cursor: None,
        };
        e.load()?;
        // Start with what most likely needs attention.
        if e.count(Show::Review) > 0 {
            e.show = Show::Review;
        }
        // Developer aid for screenshots: POLIBRE_EDIT_SEARCH=text fills the search box.
        if let Ok(q) = std::env::var("POLIBRE_EDIT_SEARCH") {
            e.search = q;
        }
        Ok(e)
    }

    pub fn path(&self) -> &PathBuf {
        &self.files[self.file]
    }

    pub fn dirty(&self) -> bool {
        !self.changed.is_empty()
    }

    /// (Re)loads the current file, dropping unsaved changes.
    pub fn load(&mut self) -> Result<(), String> {
        let path = self.path().clone();
        let po =
            PoFile::load(&path).map_err(|e| format!("Unable to open {}: {}", path.display(), e))?;
        let (n, expr) = po.plural_forms();
        self.rows = po
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| match it {
                Item::Entry(e) if !e.obsolete && !e.is_header() && !e.msgid.is_empty() => Some(i),
                _ => None,
            })
            .collect();
        self.po = po;
        self.nplurals = n;
        self.plural_expr = expr;
        self.loaded_mtime = mtime(&path);
        self.changed.clear();
        self.problem = None;
        self.conflict = false;
        self.page = 0;
        self.open_context.clear();
        self.keep.clear();
        self.suggesting.clear();
        self.suggest_errors.clear();
        self.current = None;
        self.cursors.clear();
        self.place_cursor = None;
        self.note_drafts.clear();
        self.problems.clear();
        for i in self.rows.clone() {
            self.recheck(i);
        }
        self.refresh_memory();
        Ok(())
    }

    /// Runs the same checks as automatic translations on one entry.
    fn recheck(&mut self, i: usize) {
        let e = self.entry(i);
        let flags: Vec<String> = e.flags.iter().filter(|f| *f != "fuzzy").cloned().collect();
        let src = Src {
            msgid: &e.msgid,
            msgid_plural: e.msgid_plural.as_deref(),
            flags: &flags,
            lang: &self.lang,
            nplurals: self.nplurals,
        };
        let result = if e.is_untranslated() {
            Ok(vec![])
        } else {
            validate::check(&src, &e.msgstr)
        };
        match result {
            Err(why) => self.problems.insert(i, why),
            Ok(_) => self.problems.remove(&i),
        };
    }

    /// Records an edit: unsaved, kept in view, checked again.
    fn mark_changed(&mut self, i: usize) {
        self.changed.insert(i);
        self.keep.insert(i);
        self.recheck(i);
    }

    /// Strings that still need work: not translated, or waiting for review.
    fn needs_attention(&self, i: usize) -> bool {
        let e = self.entry(i);
        e.is_fuzzy() || e.is_untranslated()
    }

    /// The next (`forward`) or previous string needing attention in `shown`,
    /// starting after `from` (or at the start/end of the list).
    fn next_target(&self, shown: &[usize], from: Option<usize>, forward: bool) -> Option<usize> {
        let pos = from.and_then(|f| shown.iter().position(|&x| x == f));
        let found = if forward {
            let start = pos.map_or(0, |p| p + 1);
            shown
                .get(start..)?
                .iter()
                .find(|&&i| self.needs_attention(i))
        } else {
            let end = pos.unwrap_or(shown.len());
            shown[..end]
                .iter()
                .rev()
                .find(|&&i| self.needs_attention(i))
        };
        found.copied()
    }

    /// Shows one string (from the Results tab): switches to its file, keeps it
    /// listed whatever the filter, and puts the cursor in it.
    pub fn focus_entry(&mut self, path: &std::path::Path, msgctxt: Option<&str>, msgid: &str) {
        if let Some(f) = self.files.iter().position(|p| p == path) {
            if f != self.file && self.switch_file(f).is_err() {
                return;
            }
        }
        let found = self.rows.iter().copied().find(|&i| {
            let e = self.entry(i);
            e.msgid == msgid && e.msgctxt.as_deref() == msgctxt
        });
        if let Some(i) = found {
            self.search.clear();
            self.keep.insert(i);
            self.pending_go = Some(i);
        }
    }

    /// Makes `i` the current entry: shows its page and puts the cursor in it.
    fn go_to(&mut self, shown: &[usize], i: usize) {
        if let Some(p) = shown.iter().position(|&x| x == i) {
            self.page = p / PAGE;
        }
        self.current = Some(i);
        self.focus_on(i);
    }

    /// Puts the cursor in the entry's translation box. Asked for two frames:
    /// egui moves the focus itself when an arrow key arrives while the focused
    /// widget doesn't use arrows, which would undo a move made with ⌘↓/⌘↑.
    fn focus_on(&mut self, i: usize) {
        self.focus = Some((i, 2));
    }

    fn apply(&mut self, i: usize, action: Action) {
        self.current = Some(i);
        match action {
            Action::Suggest => {
                if !self.suggesting.contains(&i) {
                    self.want_suggest = Some(i);
                }
                return;
            }
            Action::CancelSuggest => {
                self.suggesting.remove(&i);
                return;
            }
            Action::EditNote => {
                let text = self.entry(i).translator_comments();
                self.note_drafts.insert(i, text);
                self.note_focus = Some(i);
                return;
            }
            Action::CloseNote => {
                self.note_drafts.remove(&i);
                return;
            }
            _ => {}
        }
        let n = if self.entry(i).msgid_plural.is_some() {
            self.nplurals
        } else {
            1
        };
        let Item::Entry(e) = &mut self.po.items[i] else {
            return;
        };
        let has_text = !e.is_untranslated();
        match action {
            Action::Approve if e.is_fuzzy() && has_text => e.set_needs_review(false),
            Action::Discard => {
                // Empty, and no longer a machine translation: what the user
                // types next is their own.
                e.set_manual(vec![String::new(); n]);
                e.set_needs_review(false);
                self.focus_on(i);
            }
            Action::ToggleReview if has_text => {
                let on = !e.is_fuzzy();
                e.set_needs_review(on);
            }
            Action::Use(t) => e.set_manual(vec![t]),
            Action::Insert(t) => {
                let mut forms = e.msgstr.clone();
                forms.resize(n, String::new());
                // At the cursor of the box used last, else at the end of the first.
                let (k, at) = self
                    .cursors
                    .get(&i)
                    .copied()
                    .unwrap_or((0, forms[0].chars().count()));
                let k = k.min(n - 1);
                let at = at.min(forms[k].chars().count());
                let byte = forms[k]
                    .char_indices()
                    .nth(at)
                    .map_or(forms[k].len(), |(b, _)| b);
                forms[k].insert_str(byte, &t);
                e.set_manual(forms);
                self.place_cursor = Some((i, k, at + t.chars().count()));
            }
            _ => return,
        }
        self.mark_changed(i);
    }

    /// Keyboard shortcuts for reviewing without the mouse.
    fn shortcuts(&mut self, ui: &egui::Ui, shown: &[usize]) {
        let pressed = |k: &KeyboardShortcut| ui.input_mut(|inp| inp.consume_shortcut(k));
        let cur = self.current.filter(|c| shown.contains(c));
        if pressed(&APPROVE_KEY) {
            if let Some(c) = cur {
                self.apply(c, Action::Approve);
                if let Some(n) = self.next_target(shown, Some(c), true) {
                    self.go_to(shown, n);
                }
            }
        } else if pressed(&NEXT_KEY) {
            if let Some(n) = self.next_target(shown, cur, true) {
                self.go_to(shown, n);
            }
        } else if pressed(&PREV_KEY) {
            if let Some(n) = self.next_target(shown, cur, false) {
                self.go_to(shown, n);
            }
        } else if pressed(&REVIEW_KEY) {
            if let Some(c) = cur {
                self.apply(c, Action::ToggleReview);
            }
        } else if pressed(&SUGGEST_KEY) {
            if let Some(c) = cur {
                self.apply(c, Action::Suggest);
            }
        } else if let Some(c) = cur.filter(|c| self.suggesting.contains(c)) {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.apply(c, Action::CancelSuggest);
            }
        }
    }

    /// Rebuilds the translation memory and glossary from the files on disk.
    fn refresh_memory(&mut self) {
        self.memory = Memory::build(&self.files);
        self.matches.clear();
        self.glossary = Glossary::from_po_files(&self.files);
        self.glossary_error = None;
        if let Some(f) = &self.glossary_file {
            match Glossary::load_user(f, &self.lang) {
                Ok(user) => self.glossary.merge(user),
                Err(e) => {
                    self.glossary_error = Some(format!("Unable to read the glossary file: {}", e))
                }
            }
        }
        self.marks.clear();
    }

    /// Uses this glossary file too (the "Extra glossary" option).
    pub fn set_glossary_file(&mut self, f: Option<PathBuf>) {
        if f != self.glossary_file {
            self.glossary_file = f;
            self.refresh_memory();
        }
    }

    /// A suggestion is being fetched.
    pub fn busy(&self) -> bool {
        !self.suggesting.is_empty()
    }

    /// Whether the editor still waits for this suggestion; when it doesn't
    /// (cancelled, or the file was switched or reloaded), the request is stopped.
    pub fn wants_suggestion(&self, path: &std::path::Path, idx: usize) -> bool {
        path == self.path() && self.suggesting.contains(&idx)
    }

    fn suggest_job(&mut self, i: usize) -> SuggestJob {
        self.suggesting.insert(i);
        self.suggest_errors.remove(&i);
        SuggestJob {
            lang: self.lang.clone(),
            files: self.files.clone(),
            path: self.path().clone(),
            idx: i,
            entry: self.entry(i).clone(),
            nplurals: self.nplurals,
            plural_expr: self.plural_expr.clone(),
        }
    }

    /// Puts a suggestion into its entry, marked as needing review. Ignored if
    /// the file was switched or reloaded in the meantime.
    pub fn suggested(&mut self, s: Suggestion) {
        let idx = s.idx;
        if s.path != *self.path() || !self.suggesting.remove(&idx) {
            return;
        }
        let Some(Item::Entry(e)) = self.po.items.get_mut(idx) else {
            return;
        };
        if e.msgid != s.msgid {
            return;
        }
        match s.result {
            Ok(forms) => {
                e.set_translation(forms, true);
                self.mark_changed(idx);
            }
            Err(why) => {
                self.suggest_errors.insert(idx, why);
            }
        }
    }

    pub fn switch_file(&mut self, i: usize) -> Result<(), String> {
        let old = self.file;
        self.file = i;
        let r = self.load();
        if r.is_err() {
            self.file = old;
        }
        // Prefer the reference language's file of the same name.
        let code = self.reference_code().map(String::from);
        self.set_reference(code.as_deref());
        r
    }

    /// The "Compare with" menu.
    fn reference_picker(&mut self, ui: &mut egui::Ui) {
        if self.others.is_empty() {
            return;
        }
        let current = match &self.reference {
            Some(r) => format!("Compare with: {}", r.name),
            None => "Compare with…".into(),
        };
        let mut pick: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt("editor-reference")
            .selected_text(current)
            .height(320.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(self.reference.is_none(), "No other language")
                    .clicked()
                {
                    pick = Some(None);
                }
                for (code, name, _) in &self.others {
                    if ui
                        .selectable_label(self.reference_code() == Some(code), name)
                        .clicked()
                    {
                        pick = Some(Some(code.clone()));
                    }
                }
            })
            .response
            .on_hover_text("Show each string's translation in another language");
        if let Some(c) = pick {
            self.set_reference(c.as_deref());
        }
    }

    /// The language shown for comparison.
    pub fn reference_code(&self) -> Option<&str> {
        self.reference.as_ref().map(|r| r.code.as_str())
    }

    pub fn set_reference(&mut self, code: Option<&str>) {
        let prefer = self.path().file_name().map(|f| f.to_os_string());
        self.reference = code.and_then(|c| {
            let (code, name, files) = self.others.iter().find(|o| o.0 == c)?;
            Some(reference::Reference::load(
                code,
                name,
                files,
                prefer.as_deref(),
            ))
        });
    }

    /// Saves the file. Refuses (and flags a conflict) if the file changed on
    /// disk since it was opened, unless `force` is set.
    pub fn save(&mut self, force: bool) -> Result<(), String> {
        let path = self.path().clone();
        if !force && mtime(&path) != self.loaded_mtime {
            self.conflict = true;
            return Err("conflict".into());
        }
        self.po
            .save(&path)
            .map_err(|e| format!("Unable to save {}: {}", path.display(), e))?;
        self.loaded_mtime = mtime(&path);
        self.changed.clear();
        self.conflict = false;
        self.problem = None;
        self.refresh_memory();
        Ok(())
    }

    fn entry(&self, i: usize) -> &crate::po::Entry {
        match &self.po.items[i] {
            Item::Entry(e) => e,
            Item::Raw(_) => unreachable!(),
        }
    }

    fn matches(&self, i: usize, show: Show, search: &str) -> bool {
        let e = self.entry(i);
        let ok = match show {
            Show::All => true,
            Show::Review => e.is_fuzzy(),
            Show::Untranslated => e.is_untranslated(),
            Show::Translated => !e.is_untranslated() && !e.is_fuzzy(),
            Show::Problems => self.problems.contains_key(&i),
        };
        if !ok {
            return false;
        }
        if search.is_empty() {
            return true;
        }
        let has = |s: &str| s.to_lowercase().contains(search);
        has(&e.msgid)
            || e.msgid_plural.as_deref().is_some_and(has)
            || e.msgctxt.as_deref().is_some_and(has)
            || e.msgstr.iter().any(|s| has(s))
    }

    fn count(&self, show: Show) -> usize {
        self.rows
            .iter()
            .filter(|&&i| self.matches(i, show, ""))
            .count()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Request {
        let mut req = Request::None;
        ui.add_space(10.0);

        // Filters
        let counts = [
            (Show::All, "All strings", self.count(Show::All)),
            (Show::Review, "Needs review", self.count(Show::Review)),
            (
                Show::Untranslated,
                "Not translated",
                self.count(Show::Untranslated),
            ),
            (Show::Translated, "Reviewed", self.count(Show::Translated)),
            (
                Show::Problems,
                "Possible problems",
                self.count(Show::Problems),
            ),
        ];
        ui.horizontal_wrapped(|ui| {
            if self.files.len() > 1 {
                ui.label("File");
                let current = self
                    .path()
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                egui::ComboBox::from_id_salt("editor-file")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (i, f) in self.files.iter().enumerate() {
                            let n = f
                                .file_name()
                                .map(|f| f.to_string_lossy().to_string())
                                .unwrap_or_default();
                            if ui.selectable_label(i == self.file, n).clicked() && i != self.file {
                                req = Request::SwitchFile(i);
                            }
                        }
                    });
                ui.add_space(8.0);
            }
            ui.label("Show");
            let cur = counts.iter().find(|c| c.0 == self.show).unwrap();
            egui::ComboBox::from_id_salt("editor-show")
                .selected_text(format!("{} ({})", cur.1, thousands(cur.2)))
                .show_ui(ui, |ui| {
                    for (s, label, n) in counts {
                        if ui
                            .selectable_label(
                                self.show == s,
                                format!("{} ({})", label, thousands(n)),
                            )
                            .clicked()
                        {
                            self.show = s;
                            self.page = 0;
                            self.keep.clear();
                        }
                    }
                });
            ui.add_space(8.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(prompt("Find text"))
                    .desired_width(180.0),
            );
            if r.changed() {
                self.page = 0;
                self.keep.clear();
            }
        });

        // Save bar
        ui.add_space(4.0);
        if self.conflict {
            note(
                ui,
                LogKind::Warn,
                "This file was changed by another program, or by a translation run, after it was opened. Reload it to see those changes (your edits here will be lost), or save to replace them.",
            );
            ui.horizontal(|ui| {
                if ui.button("Reload").clicked() {
                    if let Err(e) = self.load() {
                        self.problem = Some(e);
                    }
                }
                if ui.button("Save anyway").clicked() {
                    match self.save(true) {
                        Ok(()) => req = Request::Saved,
                        Err(e) => self.problem = Some(e),
                    }
                }
            });
        } else {
            ui.horizontal(|ui| {
                let dirty = self.dirty();
                let label = RichText::new("Commit changes").color(if dirty {
                    Color32::WHITE
                } else {
                    ui.visuals().weak_text_color()
                });
                let b = egui::Button::new(label).fill(if dirty {
                    ACCENT
                } else {
                    ui.visuals().widgets.inactive.weak_bg_fill
                });
                let key = ui.ctx().format_shortcut(&SAVE_KEY);
                let save = ui
                    .add_enabled(dirty, b)
                    .on_hover_text(format!("Shortcut: {}", key))
                    .clicked()
                    || (dirty && ui.input_mut(|i| i.consume_shortcut(&SAVE_KEY)));
                if save {
                    match self.save(false) {
                        Ok(()) => req = Request::Saved,
                        Err(e) if e == "conflict" => {}
                        Err(e) => self.problem = Some(e),
                    }
                }
                if ui
                    .add_enabled(dirty, egui::Button::new("Discard changes"))
                    .clicked()
                {
                    if let Err(e) = self.load() {
                        self.problem = Some(e);
                    }
                }
                ui.add_space(8.0);
                super::completion_bar(
                    ui,
                    self.count(Show::Translated),
                    self.count(Show::Review),
                    self.rows.len(),
                    70.0,
                );
                ui.add_space(8.0);
                match self.changed.len() {
                    0 => {}
                    1 => {
                        ui.label(RichText::new("1 string changed.").weak());
                    }
                    n => {
                        ui.label(
                            RichText::new(format!("{} strings changed.", thousands(n))).weak(),
                        );
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.reference_picker(ui)
                });
            });
        }
        if let Some(p) = self.problem.clone() {
            note(ui, LogKind::Err, &p);
        }
        if let Some(p) = &self.glossary_error {
            note(ui, LogKind::Warn, p);
        }
        ui.add_space(4.0);
        ui.separator();

        // List
        let search = self.search.trim().to_lowercase();
        let shown: Vec<usize> = self
            .rows
            .iter()
            .copied()
            .filter(|&i| self.keep.contains(&i) || self.matches(i, self.show, &search))
            .collect();
        if let Some(i) = self.pending_go.take() {
            self.go_to(&shown, i);
        }
        self.shortcuts(ui, &shown);
        let pages = shown.len().div_ceil(PAGE).max(1);
        self.page = self.page.min(pages - 1);
        let start = self.page * PAGE;
        let end = (start + PAGE).min(shown.len());

        let pager_h = 40.0;
        let list_h = (ui.available_height() - pager_h).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt(("editor", self.file, self.page, self.show as u8))
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Room for the card borders, which the scroll area would clip.
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 1.0,
                        right: 6.0,
                        top: 1.0,
                        bottom: 1.0,
                    })
                    .show(ui, |ui| {
                        if shown.is_empty() {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(if search.is_empty() {
                                    "There are no strings in this view."
                                } else {
                                    "No strings match the search."
                                })
                                .weak(),
                            );
                        }
                        for &i in &shown[start..end] {
                            if let Some(a) = self.entry_ui(ui, i) {
                                self.apply(i, a);
                            }
                            ui.add_space(6.0);
                        }
                    });
            });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.page > 0, egui::Button::new("⏴"))
                .on_hover_text("Previous page")
                .clicked()
            {
                self.page -= 1;
            }
            let text = if shown.is_empty() {
                "0 strings".to_string()
            } else {
                format!("{}–{} of {}", start + 1, end, thousands(shown.len()))
            };
            ui.label(RichText::new(text).weak());
            if ui
                .add_enabled(self.page + 1 < pages, egui::Button::new("⏵"))
                .on_hover_text("Next page")
                .clicked()
            {
                self.page += 1;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let k = |s: &KeyboardShortcut| ui.ctx().format_shortcut(s);
                let help = format!(
                    "{}  Next string to review or translate\n{}  Previous one\n{}  Approve and go to the next\n{}  Mark or unmark as needing review\n{}  Suggest a translation\n{}  Commit changes",
                    k(&NEXT_KEY), k(&PREV_KEY), k(&APPROVE_KEY), k(&REVIEW_KEY), k(&SUGGEST_KEY), k(&SAVE_KEY)
                );
                ui.label(RichText::new("Keyboard shortcuts").weak()).on_hover_text(help);
            });
        });
        // Developer aid: POLIBRE_EDIT_SUGGEST=1 presses Suggest on the first string once.
        static AUTO_SUGGEST: std::sync::Once = std::sync::Once::new();
        if std::env::var("POLIBRE_EDIT_SUGGEST").is_ok() {
            if let Some(&i) = shown.first() {
                AUTO_SUGGEST.call_once(|| self.want_suggest = Some(i));
            }
        }
        if let Some(i) = self.want_suggest.take() {
            req = Request::Suggest(Box::new(self.suggest_job(i)));
        }
        req
    }

    /// Draws one entry. Typing changes it directly; buttons return an action.
    fn entry_ui(&mut self, ui: &mut egui::Ui, i: usize) -> Option<Action> {
        let nplurals = self.nplurals;
        let plural_expr = self.plural_expr.clone();
        let has_root = self.source_root.is_some();
        let mut action = None;
        let Item::Entry(e) = &mut self.po.items[i] else {
            return None;
        };
        let fuzzy = e.is_fuzzy();
        let stroke = if fuzzy {
            egui::Stroke::new(1.0, AMBER.gamma_multiply(0.7))
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        let mut changed = false;
        egui::Frame::none()
            .stroke(stroke)
            .rounding(6.0)
            .inner_margin(egui::Margin::symmetric(12.0, 10.0))
            .fill(if fuzzy {
                AMBER.gamma_multiply(0.06)
            } else {
                Color32::TRANSPARENT
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // Status line
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if fuzzy {
                        ui.label(icon(LogKind::Warn));
                        ui.label(RichText::new("Needs review").strong());
                    } else if e.is_untranslated() {
                        ui.label(RichText::new("○").weak());
                        ui.label(RichText::new("Not translated").weak());
                    } else {
                        ui.label(icon(LogKind::Ok));
                        ui.label(RichText::new("Reviewed").weak());
                    }
                    if let Some(n) = e.auto_note() {
                        ui.label(RichText::new("·").weak());
                        ui.label(RichText::new(note_label(n)).weak())
                            .on_hover_text("Added by PoLibre");
                    }
                    if let Some(c) = &e.msgctxt {
                        ui.label(RichText::new("·").weak());
                        ui.label(RichText::new(format!("Context: {}", c)).weak());
                    }
                    if let Some(r) = e.references.first() {
                        ui.label(RichText::new("·").weak());
                        let more = if e.references.len() > 1 {
                            format!(" and {} more", e.references.len() - 1)
                        } else {
                            String::new()
                        };
                        ui.label(RichText::new(format!("{}{}", r, more)).small().weak())
                            .on_hover_text(e.references.join("\n"));
                        if has_root {
                            let open = self.open_context.contains(&i);
                            let label = if open { "Hide context" } else { "See context" };
                            if ui
                                .small_button(label)
                                .on_hover_text("Show the source code where this string is used")
                                .clicked()
                            {
                                if open {
                                    self.open_context.remove(&i);
                                } else {
                                    self.open_context.insert(i);
                                }
                            }
                        }
                    }
                });
                // Developer aid for screenshots: POLIBRE_EDIT_CONTEXT=1 shows all contexts.
                if self.open_context.contains(&i)
                    || (has_root && std::env::var("POLIBRE_EDIT_CONTEXT").is_ok())
                {
                    let root = self.source_root.clone().unwrap_or_default();
                    let mut found = 0;
                    for r in &e.references {
                        let Some(snip) = self.sources.snippet(&root, r, CONTEXT_LINES) else {
                            continue;
                        };
                        found += 1;
                        ui.add_space(4.0);
                        ui.label(RichText::new(r).small().weak());
                        ui.push_id(("context", i, r.as_str()), |ui| code_block(ui, &snip));
                    }
                    if found == 0 {
                        ui.add_space(2.0);
                        note(
                            ui,
                            LogKind::Info,
                            &format!(
                                "The source code for this string wasn't found in {}.",
                                root.display()
                            ),
                        );
                    }
                }
                ui.add_space(4.0);
                // Source, with glossary terms marked
                let glossary = &self.glossary;
                let m = self.marks.entry(i).or_insert_with(|| {
                    // Terms shorter than 3 letters ("No", "OK") are mostly noise here.
                    let terms = |t: &str| -> Vec<source::Term> {
                        source::find_terms(t, &glossary.relevant(&[t], 20))
                            .into_iter()
                            .filter(|x| x.source.chars().count() >= 3)
                            .collect()
                    };
                    let spans = |t: &str| source::placeholder_spans(t, &e.flags);
                    let plural = e.msgid_plural.as_deref().unwrap_or("");
                    Marks {
                        terms: [terms(&e.msgid), terms(plural)],
                        placeholders: [spans(&e.msgid), spans(plural)],
                    }
                });
                let texts = [Some(e.msgid.as_str()), e.msgid_plural.as_deref()];
                for (k, text) in texts.iter().enumerate() {
                    let Some(text) = text else { continue };
                    if e.msgid_plural.is_some() {
                        ui.label(RichText::new(if k == 0 { "Singular" } else { "Plural" }).small().weak());
                    }
                    if let Some(p) = source::show(ui, text, &m.terms[k], &m.placeholders[k], 15.0) {
                        action = Some(Action::Insert(p));
                    }
                }
                let terms: Vec<source::Term> = m.terms.concat();
                if let Some(rf) = &self.reference {
                    if let Some(t) = rf.get(e.msgctxt.as_deref(), &e.msgid) {
                        let review = if t.needs_review { " (needs review)" } else { "" };
                        ui.add(
                            egui::Label::new(RichText::new(format!("{}: {}{}", rf.name, t.text, review)).weak())
                                .wrap(),
                        );
                    }
                }
                // What changed in the source since the translation was made.
                if fuzzy {
                    if let Some(old) = e.previous_msgid().filter(|o| *o != e.msgid) {
                        ui.add_space(2.0);
                        ui.label(RichText::new("Source changed since it was translated:").small().weak());
                        source_diff(ui, &old, &e.msgid);
                    }
                }
                for c in &e.extracted_comments {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Developer note: {}", c))
                                .small()
                                .weak(),
                        )
                        .wrap(),
                    );
                }
                // Translator note (`# …` comments), kept in the file.
                if let Some(draft) = self.note_drafts.get_mut(&i) {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let w = ui.available_width() - super::button_width(ui, "Done") - ui.spacing().item_spacing.x;
                        let r = ui.add(
                            egui::TextEdit::multiline(draft)
                                .id_salt(("note", i))
                                .desired_rows(1)
                                .desired_width(w)
                                .hint_text(prompt("Note for translators, saved in the file")),
                        );
                        if self.note_focus == Some(i) {
                            r.request_focus();
                            self.note_focus = None;
                        }
                        if r.changed() {
                            e.set_translator_comments(draft);
                            changed = true;
                        }
                        if ui.button("Done").clicked() {
                            action = Some(Action::CloseNote);
                        }
                    });
                } else {
                    let note_text = e.translator_comments();
                    if !note_text.trim().is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Label::new(RichText::new(format!("Translator note: {}", note_text)).small().weak()).wrap());
                            if ui.small_button("Edit").clicked() {
                                action = Some(Action::EditNote);
                            }
                        });
                    }
                }
                ui.add_space(4.0);
                // Translation
                let n = if e.msgid_plural.is_some() {
                    nplurals
                } else {
                    1
                };
                let mut forms = e.msgstr.clone();
                forms.resize(n, String::new());
                for (k, f) in forms.iter_mut().enumerate() {
                    if n > 1 {
                        ui.label(RichText::new(crate::engine::form_label(k, n)).small().weak())
                            .on_hover_text(format!(
                                "Used when the plural rule “{}” gives {}",
                                if plural_expr.is_empty() {
                                    "n != 1"
                                } else {
                                    &plural_expr
                                },
                                k
                            ));
                    }
                    let out = egui::TextEdit::multiline(f)
                        .id_salt(("msgstr", i, k))
                        .desired_rows(1)
                        .desired_width(f32::INFINITY)
                        .hint_text(prompt("Type the translation"))
                        .show(ui);
                    let r = out.response;
                    if r.has_focus() {
                        if let Some(c) = out.cursor_range {
                            self.cursors.insert(i, (k, c.primary.ccursor.index));
                        }
                    }
                    // After an insertion: cursor right after the inserted text.
                    if let Some((_, _, at)) = self.place_cursor.filter(|p| p.0 == i && p.1 == k) {
                        let mut state = out.state;
                        let c = egui::text::CCursor::new(at);
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(c)));
                        state.store(ui.ctx(), r.id);
                        r.request_focus();
                        self.cursors.insert(i, (k, at));
                        self.place_cursor = None;
                    }
                    if r.changed() {
                        changed = true;
                    }
                    if r.gained_focus() {
                        self.current = Some(i);
                    }
                    if let Some((f, n)) = self.focus.filter(|f| k == 0 && f.0 == i) {
                        r.request_focus();
                        r.scroll_to_me(Some(Align::Center));
                        self.focus = (n > 1).then_some((f, n - 1));
                    }
                }
                if changed {
                    // Stays "needs review" until approved.
                    e.set_manual(forms.clone());
                }
                // Reviewed translations of the same or similar text.
                if e.msgid_plural.is_none() && (fuzzy || e.is_untranslated()) {
                    let path = &self.files[self.file];
                    let found = self.matches.entry(i).or_insert_with(|| {
                        self.memory.lookup(&e.msgid, e.msgctxt.as_deref(), path, 3)
                    });
                    if let Some(t) = memory_ui(ui, found, &e.msgid, &forms[0]) {
                        action = Some(Action::Use(t));
                    }
                }
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    let has_text = forms.iter().any(|f| !f.is_empty());
                    let ctx = ui.ctx().clone();
                    let key = |k: &KeyboardShortcut| ctx.format_shortcut(k);
                    if e.is_fuzzy() {
                        let b = egui::Button::new(RichText::new("✔ Approve").color(Color32::WHITE))
                            .fill(ACCENT);
                        if ui
                            .add_enabled(has_text, b)
                            .on_hover_text(format!(
                                "Mark this translation as reviewed\n{}: approve and go to the next string",
                                key(&APPROVE_KEY)
                            ))
                            .clicked()
                        {
                            action = Some(Action::Approve);
                        }
                        if ui
                            .add_enabled(has_text, egui::Button::new("Discard"))
                            .on_hover_text("Clear this translation and type your own")
                            .clicked()
                        {
                            action = Some(Action::Discard);
                        }
                    }
                    if self.suggesting.contains(&i) {
                        // The Suggest button turns into a stop button while waiting.
                        if ui
                            .add(super::stop_button(ui, "Cancel"))
                            .on_hover_text("Stop getting a suggestion (Esc)")
                            .clicked()
                        {
                            action = Some(Action::CancelSuggest);
                        }
                        ui.spinner();
                    } else if ui
                        .button("Suggest")
                        .on_hover_text(format!(
                            "Ask {} for a translation ({})",
                            self.service,
                            key(&SUGGEST_KEY)
                        ))
                        .clicked()
                    {
                        action = Some(Action::Suggest);
                    }
                    if !self.note_drafts.contains_key(&i)
                        && e.translator_comments().trim().is_empty()
                        && ui.button("Add note").on_hover_text("Add a note for translators to this string").clicked()
                    {
                        action = Some(Action::EditNote);
                    }
                    // Same checks as automatic translations, as a hint.
                    if let Some(why) = self.problems.get(&i) {
                        ui.add_space(12.0);
                        ui.label(icon(LogKind::Warn));
                        ui.add(
                            egui::Label::new(RichText::new(format!("Possible problem: {}", why)).weak())
                                .wrap(),
                        );
                    }
                });
                if let Some(why) = self.suggest_errors.get(&i) {
                    note(ui, LogKind::Warn, &format!("No suggestion: {}", why));
                }
                // Glossary terms the translation doesn't seem to use.
                if !e.is_untranslated() {
                    let all = e.msgstr.join(" ");
                    let mut seen = BTreeSet::new();
                    for t in terms.iter().filter(|t| !source::uses_term(&all, &t.translation)) {
                        if seen.insert(t.translation.to_lowercase()) {
                            ui.label(
                                RichText::new(format!("Glossary: “{}” is usually translated as “{}”.", t.source, t.translation))
                                    .small()
                                    .weak(),
                            );
                        }
                    }
                }
            });
        if changed {
            self.mark_changed(i);
        }
        if action.is_some() {
            ui.ctx().request_repaint();
        }
        action
    }
}

/// The new source text with removed words struck out and added ones highlighted.
fn source_diff(ui: &mut egui::Ui, old: &str, new: &str) {
    use egui::text::{LayoutJob, TextFormat};
    let font = egui::FontId::proportional(15.0);
    let text = ui.visuals().text_color();
    let mut job = LayoutJob::default();
    for (op, t) in diff::word_diff(old, new) {
        let mut f = TextFormat::simple(font.clone(), text);
        match op {
            diff::Op::Same => {}
            diff::Op::Removed => {
                f.color = RED;
                f.strikethrough = egui::Stroke::new(1.0, RED);
            }
            diff::Op::Added => {
                f.background = GREEN.gamma_multiply(0.25);
            }
        }
        job.append(&t, 0.0, f);
    }
    job.wrap.max_width = ui.available_width();
    ui.label(job).on_hover_text(format!("Before: {}", old));
}

/// Lists translation-memory matches; returns the translation to use, if picked.
fn memory_ui(ui: &mut egui::Ui, found: &[Match], msgid: &str, current: &str) -> Option<String> {
    let mut picked = None;
    if found.is_empty() {
        return None;
    }
    ui.add_space(2.0);
    ui.label(RichText::new("Existing translations").small().weak());
    for (k, m) in found.iter().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let used = m.translation == current;
            if ui
                .add_enabled(!used, egui::Button::new("Use").small())
                .on_hover_text(format!("From {}", m.file))
                .clicked()
            {
                picked = Some(m.translation.clone());
            }
            ui.label(
                RichText::new(format!("{:.0}%", m.score * 100.0))
                    .small()
                    .weak(),
            )
            .on_hover_text(if m.score >= 1.0 {
                "Same text"
            } else {
                "Share of words in common"
            });
            ui.push_id(k, |ui| {
                ui.add(egui::Label::new(&m.translation).wrap());
            });
            if m.source != msgid {
                ui.add(
                    egui::Label::new(RichText::new(format!("for “{}”", m.source)).small().weak())
                        .wrap(),
                );
            }
        });
    }
    picked
}

/// The PoLibre note without its date and review suffix:
/// "Translated by opus on 2026-11-11, needs review" -> "Translated by opus".
fn note_label(n: &str) -> &str {
    // Older notes also carry a date and ", needs review".
    let n = n.trim_end_matches(", needs review");
    match n.rsplit_once(" on ") {
        Some((head, date)) if date.len() == 10 && date.as_bytes()[4] == b'-' => head,
        _ => n,
    }
}

/// Source lines with line numbers; the referenced line (marked ">") is highlighted.
fn code_block(ui: &mut egui::Ui, snippet: &str) {
    let mono = TextStyle::Monospace.resolve(ui.style());
    let hl = ACCENT.gamma_multiply(0.25);
    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::horizontal()
                .id_salt("code")
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for line in snippet.lines() {
                        let target = line.starts_with('>');
                        let text = RichText::new(line.get(1..).unwrap_or("")).font(mono.clone());
                        let text = if target { text.strong() } else { text.weak() };
                        let fill = if target { hl } else { Color32::TRANSPARENT };
                        egui::Frame::none().fill(fill).show(ui, |ui| {
                            ui.add(egui::Label::new(text).extend());
                        });
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary .po file with the given entries (msgid, msgstr, fuzzy).
    fn temp_po(name: &str, entries: &[(&str, &str, bool)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("polibre-ed-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut text = String::from(
            "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
        );
        for (id, tr, fuzzy) in entries {
            text.push_str(&format!(
                "\n{}msgid \"{}\"\nmsgstr \"{}\"\n",
                if *fuzzy { "#, fuzzy\n" } else { "" },
                id,
                tr
            ));
        }
        let path = dir.join("django.po");
        std::fs::write(&path, text).unwrap();
        path
    }

    /// Runs one frame of the editor without a window, with the given key presses.
    fn frame(ctx: &egui::Context, ed: &mut Editor, keys: &[KeyboardShortcut]) {
        let events = keys
            .iter()
            .map(|k| egui::Event::Key {
                key: k.logical_key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: k.modifiers,
            })
            .collect();
        let input = egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ed.ui(ui);
            });
        });
    }

    #[test]
    fn keyboard_review_moves_approves_and_toggles() {
        let path = temp_po(
            "keys",
            &[
                ("One", "Uno", true),
                ("Two", "Due", false),
                ("Three", "", false),
                ("Four", "Quattro", true),
            ],
        );
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        ed.show = Show::All;
        let r = ed.rows.clone();
        let ctx = egui::Context::default();
        frame(&ctx, &mut ed, &[]);
        // Next skips the reviewed "Two"; previous goes back.
        frame(&ctx, &mut ed, &[NEXT_KEY]);
        assert_eq!(ed.current, Some(r[0]));
        frame(&ctx, &mut ed, &[NEXT_KEY]);
        assert_eq!(ed.current, Some(r[2]), "the reviewed string is skipped");
        frame(&ctx, &mut ed, &[PREV_KEY]);
        assert_eq!(ed.current, Some(r[0]));
        // The cursor really is in that string's translation box.
        frame(&ctx, &mut ed, &[]);
        frame(&ctx, &mut ed, &[]);
        assert!(ed.focus.is_none());
        let focused = ctx.memory(|m| m.focused());
        assert!(focused.is_some());
        frame(&ctx, &mut ed, &[]);
        assert_eq!(
            ed.current,
            Some(r[0]),
            "the focused box is the current string"
        );
        assert_eq!(ctx.memory(|m| m.focused()), focused, "focus stays");
        // Approve "One" and land on the next string needing work.
        frame(&ctx, &mut ed, &[APPROVE_KEY]);
        assert!(!ed.entry(r[0]).is_fuzzy());
        assert_eq!(ed.current, Some(r[2]));
        // Toggle review on a translated string, and back.
        ed.current = Some(r[1]);
        frame(&ctx, &mut ed, &[REVIEW_KEY]);
        assert!(ed.entry(r[1]).is_fuzzy());
        frame(&ctx, &mut ed, &[REVIEW_KEY]);
        assert!(!ed.entry(r[1]).is_fuzzy());
        // An untranslated string can't be marked for review; Suggest asks the window.
        ed.current = Some(r[2]);
        frame(&ctx, &mut ed, &[REVIEW_KEY]);
        assert!(!ed.entry(r[2]).is_fuzzy());
        assert_eq!(ed.changed.len(), 2, "the approved and the toggled string");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn problems_filter_follows_edits() {
        let path = temp_po(
            "problems",
            &[
                ("Hello %(name)s", "Ciao", false),
                ("Bye", "Ciao ciao", false),
                ("New", "", false),
            ],
        );
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        let a = ed.rows[0];
        assert_eq!(
            ed.count(Show::Problems),
            1,
            "missing placeholder; untranslated isn't a problem"
        );
        assert!(ed.problems[&a].contains("%(name)s"));
        ed.apply(a, Action::Use("Ciao %(name)s".into()));
        assert_eq!(ed.count(Show::Problems), 0);
        ed.load().unwrap();
        assert_eq!(
            ed.count(Show::Problems),
            1,
            "back after discarding the edit"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn glossary_from_project_and_file() {
        let path = temp_po(
            "glossary",
            &[
                ("Project", "Progetto", false),
                ("Delete project", "Elimina attività", true),
            ],
        );
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        let terms = |ed: &Editor, t: &str| source::find_terms(t, &ed.glossary.relevant(&[t], 20));
        let found = terms(&ed, "Delete project");
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].source.as_str(), found[0].translation.as_str()),
            ("project", "Progetto")
        );
        assert!(
            !source::uses_term("Elimina attività", &found[0].translation),
            "flagged as not using the term"
        );
        // A glossary file adds (and overrides) terms.
        let file = path.with_file_name("terms.csv");
        std::fs::write(&file, "project,Lavoro\ndelete,Cancella\n").unwrap();
        ed.set_glossary_file(Some(file));
        let found = terms(&ed, "Delete project");
        assert_eq!(
            found
                .iter()
                .map(|t| t.translation.as_str())
                .collect::<Vec<_>>(),
            vec!["Cancella", "Lavoro"]
        );
        ed.set_glossary_file(Some(path.with_file_name("missing.csv")));
        assert!(ed.glossary_error.is_some());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn translator_note_typed_in_the_editor() {
        let path = temp_po("note", &[("One", "Uno", false)]);
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        ed.show = Show::All;
        let a = ed.rows[0];
        let ctx = egui::Context::default();
        frame(&ctx, &mut ed, &[]);
        ed.apply(a, Action::EditNote);
        frame(&ctx, &mut ed, &[]); // the note box takes the cursor
        let input = egui::RawInput {
            events: vec![egui::Event::Text("Button label".into())],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ed.ui(ui);
            });
        });
        assert_eq!(ed.entry(a).translator_comments(), "Button label");
        assert!(ed.dirty());
        ed.apply(a, Action::CloseNote);
        ed.save(false).unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("# Button label\nmsgid \"One\""));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn compare_with_another_language() {
        let it = temp_po("ref-it", &[("Open", "", false)]);
        let fr = temp_po("ref-fr", &[("Open", "Ouvrir", false)]);
        let mut ed = Editor::open("it", "Italian", vec![it.clone()], 0, None).unwrap();
        ed.others = vec![("fr".into(), "French".into(), vec![fr.clone()])];
        ed.set_reference(Some("fr"));
        assert_eq!(ed.reference_code(), Some("fr"));
        assert_eq!(
            ed.reference
                .as_ref()
                .unwrap()
                .get(None, "Open")
                .unwrap()
                .text,
            "Ouvrir"
        );
        ed.switch_file(0).unwrap();
        assert_eq!(ed.reference_code(), Some("fr"), "kept when switching files");
        ed.set_reference(Some("xx"));
        assert_eq!(ed.reference_code(), None, "unknown languages are ignored");
        for p in [it, fr] {
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
    }

    #[test]
    fn placeholder_is_inserted_at_the_cursor() {
        let path = temp_po(
            "insert",
            &[
                ("Hello %(name)s", "Ciao", false),
                ("Bye %(name)s", "", false),
            ],
        );
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        let (a, b) = (ed.rows[0], ed.rows[1]);
        // Cursor after "Ci": inserted there, cursor moves after it.
        ed.cursors.insert(a, (0, 2));
        ed.apply(a, Action::Insert("%(name)s".into()));
        assert_eq!(ed.entry(a).msgstr, vec!["Ci%(name)sao"]);
        assert_eq!(ed.place_cursor, Some((a, 0, 10)));
        // In the window: the box gets the cursor there, and typing continues after it.
        ed.show = Show::All;
        let ctx = egui::Context::default();
        frame(&ctx, &mut ed, &[]);
        assert!(ed.place_cursor.is_none());
        frame(&ctx, &mut ed, &[]);
        let typed = egui::RawInput {
            events: vec![egui::Event::Text("!".into())],
            ..Default::default()
        };
        let _ = ctx.run(typed, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ed.ui(ui);
            });
        });
        assert_eq!(ed.entry(a).msgstr, vec!["Ci%(name)s!ao"]);
        // No cursor yet: at the end.
        ed.apply(b, Action::Insert("%(name)s".into()));
        assert_eq!(ed.entry(b).msgstr, vec!["%(name)s"]);
        assert!(ed.changed.contains(&a) && ed.changed.contains(&b));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn focus_a_string_from_the_results() {
        let a = temp_po("focus-a", &[("One", "Uno", true)]);
        let b = temp_po("focus-b", &[("Two", "Due", false), ("Three", "Tre", false)]);
        let mut ed = Editor::open("it", "Italian", vec![a.clone(), b.clone()], 0, None).unwrap();
        assert_eq!(ed.show, Show::Review, "opens on strings needing review");
        ed.focus_entry(&b, None, "Three");
        assert_eq!(ed.file, 1, "switched to the string's file");
        let three = ed.rows[1];
        let ctx = egui::Context::default();
        frame(&ctx, &mut ed, &[]);
        assert_eq!(ed.current, Some(three));
        frame(&ctx, &mut ed, &[]);
        assert!(
            ctx.memory(|m| m.focused()).is_some(),
            "the cursor is in its box"
        );
        assert!(
            ed.keep.contains(&three),
            "listed even though the filter is Needs review"
        );
        for p in [a, b] {
            std::fs::remove_dir_all(p.parent().unwrap()).ok();
        }
    }

    #[test]
    fn suggest_shortcut_and_actions() {
        let path = temp_po("actions", &[("One", "Uno", true)]);
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        let a = ed.rows[0];
        ed.current = Some(a);
        let ctx = egui::Context::default();
        let mut req = Request::None;
        let _ = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::J,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::COMMAND,
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| req = ed.ui(ui));
            },
        );
        assert!(matches!(req, Request::Suggest(ref j) if j.idx == a));
        ed.apply(a, Action::Discard);
        assert!(
            ed.entry(a).is_untranslated()
                && !ed.entry(a).is_fuzzy()
                && ed.focus.is_some_and(|f| f.0 == a)
        );
        ed.apply(a, Action::Use("Uno".into()));
        assert_eq!(ed.entry(a).msgstr, vec!["Uno"]);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn cancelling_a_suggestion() {
        let path = temp_po("cancel", &[("One", "", false), ("Two", "", false)]);
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        ed.show = Show::All;
        let (a, b) = (ed.rows[0], ed.rows[1]);
        // The Cancel button: no longer waited for, and a late answer is ignored.
        ed.suggest_job(a);
        assert!(ed.wants_suggestion(&path, a));
        ed.apply(a, Action::CancelSuggest);
        assert!(!ed.wants_suggestion(&path, a) && !ed.busy());
        ed.suggested(Suggestion { path: path.clone(), idx: a, msgid: "One".into(), result: Err("cancelled".into()) });
        assert!(ed.suggest_errors.is_empty(), "no error shown for a cancelled suggestion");
        ed.suggested(Suggestion { path: path.clone(), idx: a, msgid: "One".into(), result: Ok(vec!["Uno".into()]) });
        assert!(ed.entry(a).is_untranslated());
        // Esc on the current string cancels its suggestion.
        ed.suggest_job(b);
        ed.current = Some(b);
        let ctx = egui::Context::default();
        frame(&ctx, &mut ed, &[KeyboardShortcut::new(Modifiers::NONE, Key::Escape)]);
        assert!(!ed.wants_suggestion(&path, b));
        // Reloading the file drops every pending suggestion.
        ed.suggest_job(a);
        ed.load().unwrap();
        assert!(!ed.wants_suggestion(&path, a));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn suggestion_fills_the_entry_for_review() {
        let path = temp_po("suggest", &[("One", "", false), ("Two", "", false)]);
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        let (a, b) = (ed.rows[0], ed.rows[1]);
        let job = ed.suggest_job(a);
        assert!(ed.busy() && job.entry.msgid == "One" && job.idx == a);
        ed.suggested(Suggestion {
            path: path.clone(),
            idx: a,
            msgid: "One".into(),
            result: Ok(vec!["Uno".into()]),
        });
        assert!(!ed.busy());
        assert_eq!(ed.entry(a).msgstr, vec!["Uno"]);
        assert!(ed.entry(a).is_fuzzy(), "a suggestion needs review");
        assert_eq!(ed.entry(a).auto_note(), None, "no machine-translation note");
        assert!(ed.dirty());
        // A failure is reported on the entry; a stale answer is ignored.
        ed.suggest_job(b);
        ed.suggested(Suggestion {
            path: path.clone(),
            idx: b,
            msgid: "Two".into(),
            result: Err("offline".into()),
        });
        assert_eq!(
            ed.suggest_errors.get(&b).map(String::as_str),
            Some("offline")
        );
        ed.suggested(Suggestion {
            path: path.clone(),
            idx: b,
            msgid: "Two".into(),
            result: Ok(vec!["Due".into()]),
        });
        assert!(ed.entry(b).is_untranslated(), "not requested any more");
        ed.suggest_job(b);
        ed.load().unwrap();
        ed.suggested(Suggestion {
            path: path.clone(),
            idx: b,
            msgid: "Two".into(),
            result: Ok(vec!["Due".into()]),
        });
        assert!(ed.entry(b).is_untranslated(), "file reloaded meanwhile");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn commit_writes_approved_and_edited_strings() {
        let dir = std::env::temp_dir().join(format!("at-editor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("django.po");
        std::fs::write(
            &path,
            "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\n#, fuzzy\nmsgid \"One\"\nmsgstr \"Uno\"\n\n#, fuzzy\nmsgid \"Two\"\nmsgstr \"Due?\"\n",
        )
        .unwrap();
        let mut ed = Editor::open("it", "Italian", vec![path.clone()], 0, None).unwrap();
        let (a, b) = (ed.rows[0], ed.rows[1]);
        // Approve the first; edit the second (it stays "needs review").
        if let Item::Entry(e) = &mut ed.po.items[a] {
            e.set_needs_review(false);
        }
        if let Item::Entry(e) = &mut ed.po.items[b] {
            e.set_manual(vec!["Due".into()]);
        }
        ed.changed.extend([a, b]);
        ed.save(false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\n\nmsgid \"One\"\nmsgstr \"Uno\"\n"),
            "{}",
            text
        );
        assert!(
            text.contains("#, fuzzy\nmsgid \"Two\"\nmsgstr \"Due\""),
            "{}",
            text
        );
        assert!(!ed.dirty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
