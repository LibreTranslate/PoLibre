// SPDX-License-Identifier: AGPL-3.0-or-later
//! The source text of an entry, with glossary terms marked and placeholders
//! that can be clicked to insert them into the translation.

use eframe::egui::{self, text::LayoutJob, CursorIcon, FontId, Sense, Stroke, TextFormat};
use std::ops::Range;

/// A glossary term found in the source text.
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    /// Byte range in the source text.
    pub range: Range<usize>,
    pub source: String,
    pub translation: String,
}

/// Where the glossary terms occur in `text` (whole words, any case), longest
/// terms first when they overlap.
pub fn find_terms(text: &str, glossary: &[(String, String)]) -> Vec<Term> {
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return vec![]; // case folding changed byte offsets; don't guess
    }
    let mut sorted: Vec<&(String, String)> = glossary.iter().collect();
    sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    let mut out: Vec<Term> = Vec::new();
    for (src, tr) in sorted {
        let needle = src.to_lowercase();
        if needle.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(p) = lower[from..].find(&needle) {
            let (a, b) = (from + p, from + p + needle.len());
            let not_word = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            let whole_word =
                not_word(lower[..a].chars().next_back()) && not_word(lower[b..].chars().next());
            let free = !out.iter().any(|t| t.range.start < b && a < t.range.end);
            if whole_word && free {
                out.push(Term {
                    range: a..b,
                    source: text[a..b].to_string(),
                    translation: tr.clone(),
                });
            }
            from = b;
        }
    }
    out.sort_by_key(|t| t.range.start);
    out
}

/// Where the placeholders and markup that must be kept (`%(name)s`, `{n}`,
/// `<b>`, URLs, …) are in `text`.
pub fn placeholder_spans(text: &str, flags: &[String]) -> Vec<Range<usize>> {
    let mut tokens: Vec<String> = crate::validate::placeholders(text, flags)
        .into_iter()
        .filter_map(|t| t.split_once(':').map(|(_, v)| v.to_string()))
        .filter(|t| !t.is_empty())
        .collect();
    tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));
    tokens.dedup();
    let mut out: Vec<Range<usize>> = Vec::new();
    for t in tokens {
        for (a, _) in text.match_indices(t.as_str()) {
            let r = a..a + t.len();
            if !out.iter().any(|o| o.start < r.end && r.start < o.end) {
                out.push(r);
            }
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

/// The placeholder under a point (relative to the laid-out text), if any.
fn placeholder_at(
    galley: &egui::Galley,
    text: &str,
    placeholders: &[Range<usize>],
    pos: egui::Vec2,
) -> Option<Range<usize>> {
    if pos.x < 0.0 || pos.y < 0.0 || pos.x > galley.size().x || pos.y > galley.size().y {
        return None;
    }
    // The nearest cursor sits between two characters: the pointer is over
    // the one on whichever side of the cursor it lies.
    let cursor = galley.cursor_from_pos(pos);
    let c = cursor.ccursor.index;
    let x = galley.pos_from_cursor(&cursor).min.x;
    let ci = if pos.x < x { c.checked_sub(1)? } else { c };
    let byte = text.char_indices().nth(ci)?.0;
    placeholders
        .iter()
        .find(|r| r.start <= byte && byte < r.end)
        .cloned()
}

/// Whether `translation` seems to use the glossary's `term` (allowing a
/// different ending, e.g. plural or gender: "progetto" / "progetti").
pub fn uses_term(translation: &str, term: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(|w| w.to_lowercase())
            .collect()
    };
    let (t, term) = (words(translation), words(term));
    if term.is_empty() {
        return true;
    }
    // Each word of the term in order, allowing the last two letters of longer
    // words to differ.
    let same = |w: &str, x: &str| {
        let n = x.chars().count();
        w == x || (n >= 6 && w.starts_with(&x.chars().take(n - 2).collect::<String>()))
    };
    t.windows(term.len())
        .any(|win| win.iter().zip(&term).all(|(w, x)| same(w, x)))
}

/// Draws the source text: glossary terms underlined (hover lists them) and
/// placeholders as clickable chips. Returns the placeholder clicked, if any.
pub fn show(
    ui: &mut egui::Ui,
    text: &str,
    terms: &[Term],
    placeholders: &[Range<usize>],
    size: f32,
) -> Option<String> {
    let font = FontId::proportional(size);
    let color = ui.visuals().text_color();
    let plain = TextFormat::simple(font.clone(), color);
    // The accent is too dark to read on a dark background.
    let chip = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(120, 175, 240)
    } else {
        super::ACCENT
    };
    // Cut points: every term and placeholder boundary.
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for r in terms.iter().map(|t| &t.range).chain(placeholders) {
        cuts.extend([r.start, r.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut job = LayoutJob::default();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut f = plain.clone();
        if placeholders.iter().any(|r| r.start <= a && b <= r.end) {
            f.color = chip;
            f.background = super::ACCENT.gamma_multiply(0.12);
        } else if terms.iter().any(|t| t.range.start <= a && b <= t.range.end) {
            f.underline = Stroke::new(1.5, super::ACCENT);
        }
        job.append(&text[a..b], 0.0, f);
    }
    job.wrap.max_width = ui.available_width();
    let galley = ui.fonts(|f| f.layout_job(job));
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
    ui.painter().galley(rect.min, galley.clone(), color);

    let hovered = resp
        .hover_pos()
        .and_then(|p| placeholder_at(&galley, text, placeholders, p - rect.min));
    let mut clicked = None;
    if let Some(r) = &hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        if resp.clicked() {
            clicked = Some(text[r.clone()].to_string());
        }
    }
    let resp = resp.on_hover_ui(|ui| {
        if let Some(r) = &hovered {
            ui.label(format!(
                "Click to insert {} into the translation",
                &text[r.clone()]
            ));
        } else if !terms.is_empty() {
            ui.label(egui::RichText::new("Glossary").strong());
            for t in terms {
                ui.label(format!("{} → {}", t.source, t.translation));
            }
        } else {
            ui.label("Right-click to copy");
        }
    });
    resp.context_menu(|ui| {
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(text.to_string());
            ui.close_menu();
        }
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn finds_whole_word_terms() {
        let gl = g(&[
            ("project", "progetto"),
            ("Project settings", "Impostazioni del progetto"),
            ("task", "attività"),
        ]);
        let t = find_terms("Open Project settings for this project's tasks", &gl);
        let found: Vec<&str> = t.iter().map(|t| t.source.as_str()).collect();
        // The longer term wins where they overlap; "tasks" isn't the word "task".
        assert_eq!(found, vec!["Project settings", "project"]);
        assert_eq!(
            &"Open Project settings for this project's tasks"[t[1].range.clone()],
            "project"
        );
    }

    #[test]
    fn placeholder_positions() {
        let text = "Hello %(name)s, open <b>%(name)s</b> at {url}";
        let spans = placeholder_spans(text, &["python-format".to_string()]);
        let found: Vec<&str> = spans.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(found, vec!["%(name)s", "<b>", "%(name)s", "</b>", "{url}"]);
        assert!(placeholder_spans("Nothing here", &[]).is_empty());
    }

    #[test]
    fn pointer_over_a_placeholder() {
        let text = "Hi %(name)s, bye";
        let ph = placeholder_spans(text, &["python-format".to_string()]);
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            let galley = ctx.fonts(|f| {
                f.layout_no_wrap(
                    text.into(),
                    FontId::proportional(15.0),
                    egui::Color32::WHITE,
                )
            });
            // The middle of each character.
            let mid = |ci: usize| {
                let a = galley
                    .pos_from_cursor(&galley.from_ccursor(egui::text::CCursor::new(ci)))
                    .min;
                let b = galley
                    .pos_from_cursor(&galley.from_ccursor(egui::text::CCursor::new(ci + 1)))
                    .min;
                egui::vec2((a.x + b.x) / 2.0, galley.size().y / 2.0)
            };
            let hit = |ci: usize| placeholder_at(&galley, text, &ph, mid(ci)).map(|r| &text[r]);
            assert_eq!(hit(1), None, "the 'i' of Hi");
            assert_eq!(hit(3), Some("%(name)s"), "first character");
            assert_eq!(hit(10), Some("%(name)s"), "last character");
            assert_eq!(hit(11), None, "the comma after it");
            assert_eq!(
                placeholder_at(&galley, text, &ph, egui::vec2(-5.0, 2.0)),
                None
            );
        });
    }

    #[test]
    fn term_use_allows_other_endings() {
        assert!(uses_term("Elimina i progetti", "progetto"));
        assert!(uses_term("Apri il Progetto", "progetto"));
        assert!(!uses_term("Elimina le attività", "progetto"));
        assert!(!uses_term("Sì", "No"));
        assert!(!uses_term("Header non valido", "No"), "whole words only");
        assert!(uses_term(
            "Apri le impostazioni del progetto",
            "Impostazioni del progetto"
        ));
        assert!(!uses_term(
            "Impostazioni generali del progetto",
            "Impostazioni del progetto"
        ));
    }
}
