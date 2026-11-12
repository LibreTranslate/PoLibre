// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validation of machine translations.
//!
//! Every translation produced by a provider passes through [`check`] before
//! it is written to a .po file. Some problems are repaired automatically
//! (surrounding whitespace, wrapping quotes, literal "\n" escapes); anything
//! that can't be repaired safely is rejected and the entry stays untranslated.

use crate::langs::{self, Script};

/// Everything we need to know about the source string to validate a translation.
pub struct Src<'a> {
    pub msgid: &'a str,
    pub msgid_plural: Option<&'a str>,
    pub flags: &'a [String],
    pub lang: &'a str,
    pub nplurals: usize,
}

/// Validate (and lightly repair) the translated plural forms of an entry.
pub fn check(src: &Src, forms: &[String]) -> Result<Vec<String>, String> {
    match src.msgid_plural {
        None => {
            if forms.len() != 1 {
                return Err(format!("expected 1 translation, got {}", forms.len()));
            }
            Ok(vec![check_one(
                src.msgid, &forms[0], src.flags, src.lang, None,
            )?])
        }
        Some(plural) => {
            if forms.len() != src.nplurals {
                return Err(format!(
                    "expected {} plural forms, got {}",
                    src.nplurals,
                    forms.len()
                ));
            }
            let mut allowed = placeholders(src.msgid, src.flags);
            allowed.extend(placeholders(plural, src.flags));
            let mut out = Vec::with_capacity(forms.len());
            for (i, f) in forms.iter().enumerate() {
                // Form 0 is usually the singular, the rest follow msgid_plural.
                let reference = if i == 0 && src.nplurals > 1 {
                    src.msgid
                } else {
                    plural
                };
                let v = check_one(reference, f, src.flags, src.lang, Some((plural, &allowed)))
                    .map_err(|e| format!("plural form {}: {}", i, e))?;
                out.push(v);
            }
            Ok(out)
        }
    }
}

fn split_ws(s: &str) -> (&str, &str, &str) {
    let core = s.trim();
    if core.is_empty() {
        return (s, "", "");
    }
    let start = s.find(core).unwrap_or(0);
    (&s[..start], core, &s[start + core.len()..])
}

/// Validate one translated string against its source. Returns the repaired string.
pub fn check_one(
    source: &str,
    output: &str,
    flags: &[String],
    lang: &str,
    plural: Option<(&str, &Vec<String>)>,
) -> Result<String, String> {
    // ---- normalisation & safe repairs ----
    let mut out = output.replace("\r\n", "\n").replace('\u{feff}', "");
    let (lead, src_core, trail) = split_ws(source);

    // Wrapping quotes the source doesn't have ("Ciao" / «Ciao» / “Ciao”).
    for (a, b) in [("\"", "\""), ("“", "”"), ("«", "»"), ("'", "'"), ("`", "`")] {
        let t = out.trim();
        if t.len() > a.len() + b.len()
            && t.starts_with(a)
            && t.ends_with(b)
            && !(src_core.starts_with(a) || src_core.starts_with('"'))
        {
            let inner = &t[a.len()..t.len() - b.len()];
            if !inner.contains(a) && !inner.contains(b) {
                out = inner.to_string();
            }
        }
    }
    // Literal "\n" escapes where the source has real newlines.
    if source.contains('\n') && !source.contains("\\n") && out.contains("\\n") {
        out = out.replace("\\n", "\n");
    }
    let core = fix_spacing(src_core, out.trim(), lang);

    // ---- hard rejections ----
    if core.is_empty() {
        return Err("empty translation".into());
    }
    if let Some(c) = core
        .chars()
        .find(|c| *c == '\u{fffd}' || (c.is_control() && *c != '\n' && *c != '\t'))
    {
        return Err(format!("contains a control character (U+{:04X})", c as u32));
    }
    garbage(src_core, &core)?;
    stray_symbols(src_core, &core)?;

    // Newline structure must be preserved.
    let n_src = src_core.matches('\n').count();
    let n_out = core.matches('\n').count();
    if n_src != n_out {
        return Err(format!(
            "line break count differs (source {}, translation {})",
            n_src, n_out
        ));
    }

    // Placeholders, tags, URLs, entities.
    let want = placeholders(source, flags);
    let got = placeholders(&core, flags);
    match plural {
        None => compare_tokens(&want, &got)?,
        Some((plural_src, allowed)) => {
            // Each form may omit numeric placeholders, but must not invent new
            // ones and must keep the non-numeric ones of msgid_plural.
            for g in &got {
                if !allowed.contains(g) {
                    return Err(format!("unexpected placeholder/markup {}", g));
                }
            }
            let required: Vec<String> = placeholders(plural_src, flags)
                .into_iter()
                .filter(|t| !is_numeric_placeholder(t))
                .collect();
            let mut pool = got.clone();
            for r in required {
                if let Some(i) = pool.iter().position(|g| *g == r) {
                    pool.remove(i);
                } else {
                    return Err(format!("missing placeholder/markup {}", r));
                }
            }
        }
    }

    length_ratio(src_core, &core, lang)?;
    script_check(src_core, &core, lang)?;
    repetition(src_core, &core)?;

    // Copied source text (only meaningful for longer strings: short ones such
    // as "CSS" or "OK" are legitimately identical in many languages).
    let words = src_core
        .split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_alphabetic()))
        .count();
    if core == src_core && words >= 4 && !langs::base(lang).eq_ignore_ascii_case("en") {
        return Err("translation is identical to the source text".into());
    }

    Ok(format!("{}{}{}", lead, core, trail))
}

/// Remove stray spaces before punctuation (a common MT artifact, e.g.
/// "已禁用 ." or "terminato ."), unless the source itself has that spacing
/// or the language's typography uses it (French).
fn fix_spacing(src: &str, out: &str, lang: &str) -> String {
    let cjk = langs::is_compact(lang);
    let french = langs::base(&langs::normalize(lang)).eq_ignore_ascii_case("fr");
    let cjk_punct = "。，、：；！？）」』";
    let chars: Vec<char> = out.chars().collect();
    let mut res = String::with_capacity(out.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ' ' {
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            let next = chars.get(j).copied();
            let prev = res.chars().next_back();
            let drop = match (prev, next) {
                (Some(p), Some(n)) if p != ' ' => {
                    let ascii_p = ".,;:!?".contains(n) && !src.contains(&format!(" {}", n));
                    let french_keep = french && ";:!?".contains(n);
                    let next_is_end_or_space = chars
                        .get(j + 1)
                        .map(|x| x.is_whitespace() || ".,;:!?".contains(*x))
                        .unwrap_or(true);
                    (ascii_p && !french_keep && next_is_end_or_space)
                        || (cjk && (cjk_punct.contains(n) || "。，、：；！？（「".contains(p)))
                }
                _ => false,
            };
            if drop {
                i = j;
                continue;
            }
        }
        res.push(c);
        i += 1;
    }
    res
}

fn compare_tokens(want: &[String], got: &[String]) -> Result<(), String> {
    let mut pool = got.to_vec();
    let mut missing = Vec::new();
    for w in want {
        if let Some(i) = pool.iter().position(|g| g == w) {
            pool.remove(i);
        } else {
            missing.push(w.clone());
        }
    }
    if !missing.is_empty() {
        return Err(format!("missing placeholder/markup {}", missing.join(" ")));
    }
    if !pool.is_empty() {
        return Err(format!("unexpected placeholder/markup {}", pool.join(" ")));
    }
    Ok(())
}

fn is_numeric_placeholder(t: &str) -> bool {
    let t = t.trim_start_matches("ph:");
    t.ends_with('d') && t.starts_with('%')
        || t.ends_with('i') && t.starts_with('%')
        || t.contains("count")
        || t.contains("num")
        || t == "{0}"
        || t == "{}"
        || t == "%s"
        || t == "%1$s"
        || t == "%1$d"
}

fn has_format_flag(flags: &[String], kinds: &[&str]) -> bool {
    flags.iter().any(|f| {
        !f.starts_with("no-") && f.ends_with("-format") && kinds.iter().any(|k| f.starts_with(k))
    })
}

/// Extract placeholders and markup tokens that must survive translation.
/// Returned as a sorted list (a multiset) of tagged tokens.
pub fn placeholders(s: &str, flags: &[String]) -> Vec<String> {
    let printf = has_format_flag(
        flags,
        &[
            "c",
            "python",
            "javascript",
            "php",
            "perl",
            "objc",
            "sh",
            "gcc",
            "boost",
            "qt",
            "kde",
            "lua",
            "java-printf",
        ],
    );
    let mut out = Vec::new();
    let b = s.as_bytes();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let (pos, c) = chars[i];
        match c {
            '%' => {
                // %(name)s - python named, always a placeholder.
                if pos + 1 < b.len() && b[pos + 1] == b'(' {
                    if let Some(close) = s[pos..].find(')') {
                        let after = pos + close + 1;
                        let conv_len = s[after..]
                            .char_indices()
                            .find(|(_, ch)| ch.is_ascii_alphabetic())
                            .map(|(j, ch)| j + ch.len_utf8());
                        if let Some(cl) = conv_len {
                            let spec = &s[after..after + cl];
                            if spec.len() <= 6
                                && spec[..spec.len() - 1]
                                    .chars()
                                    .all(|ch| "#0- +.123456789".contains(ch))
                            {
                                let tok = &s[pos..after + cl];
                                out.push(format!("ph:{}", tok));
                                let end = after + cl;
                                while i < n && chars[i].0 < end {
                                    i += 1;
                                }
                                continue;
                            }
                        }
                    }
                }
                // printf style
                let rest = &s[pos + 1..];
                if rest.starts_with('%') {
                    if printf {
                        out.push("ph:%%".into());
                    }
                    i += 2;
                    continue;
                }
                if let Some(len) = printf_spec_len(rest) {
                    let tok = &s[pos..pos + 1 + len];
                    let simple =
                        matches!(tok, "%s" | "%d" | "%i" | "%f" | "%@") || tok.contains('$');
                    if printf || simple {
                        out.push(format!("ph:{}", tok));
                        let end = pos + 1 + len;
                        while i < n && chars[i].0 < end {
                            i += 1;
                        }
                        continue;
                    }
                }
            }
            '{' => {
                let rest = &s[pos..];
                if rest.starts_with("{{") {
                    if let Some(end) = rest.find("}}") {
                        let inner: String = rest[2..end]
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !inner.is_empty() && inner.len() < 80 {
                            out.push(format!("ph:{{{{{}}}}}", inner));
                            let stop = pos + end + 2;
                            while i < n && chars[i].0 < stop {
                                i += 1;
                            }
                            continue;
                        }
                    }
                    i += 2;
                    continue;
                }
                if let Some(end) = rest[1..].find(['}', '{']) {
                    if rest.as_bytes()[1 + end] == b'}' {
                        let inner = &rest[1..1 + end];
                        let ok = inner.len() < 64
                            && inner.chars().all(|ch| {
                                ch.is_alphanumeric() || "_.!:<>^#=+-,[]'\"0123456789".contains(ch)
                            })
                            && !inner.contains(',');
                        if ok {
                            out.push(format!("ph:{{{}}}", inner));
                            let stop = pos + end + 2;
                            while i < n && chars[i].0 < stop {
                                i += 1;
                            }
                            continue;
                        }
                    }
                }
            }
            '$' => {
                let rest = &s[pos..];
                if rest.starts_with("${") {
                    if let Some(end) = rest.find('}') {
                        out.push(format!("ph:{}", &rest[..=end]));
                        let stop = pos + end + 1;
                        while i < n && chars[i].0 < stop {
                            i += 1;
                        }
                        continue;
                    }
                }
            }
            '<' => {
                if let Some(t) = html_tag(&s[pos..]) {
                    let (tok, len) = t;
                    out.push(tok);
                    let stop = pos + len;
                    while i < n && chars[i].0 < stop {
                        i += 1;
                    }
                    continue;
                }
            }
            '&' => {
                let rest = &s[pos..];
                if let Some(end) = rest[1..].find(';') {
                    let name = &rest[1..1 + end];
                    if !name.is_empty()
                        && name.len() <= 8
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '#')
                    {
                        out.push(format!("entity:&{};", name));
                        let stop = pos + end + 2;
                        while i < n && chars[i].0 < stop {
                            i += 1;
                        }
                        continue;
                    }
                }
            }
            'h' => {
                let rest = &s[pos..];
                if rest.starts_with("http://") || rest.starts_with("https://") {
                    let end = rest
                        .find(|ch: char| ch.is_whitespace() || "\"'<>)".contains(ch))
                        .unwrap_or(rest.len());
                    let url = rest[..end].trim_end_matches(['.', ',', ';', ':']);
                    out.push(format!("url:{}", url));
                    let stop = pos + url.len();
                    while i < n && chars[i].0 < stop {
                        i += 1;
                    }
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.extend(code_tokens(s));
    out.sort();
    out
}

/// Code identifiers that must never be translated: snake_case names,
/// file names with extensions and command line flags.
fn code_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in s.split_whitespace() {
        let w = raw.trim_matches(|c: char| "\"'`.,;:!?()[]«»“”".contains(c));
        if w.len() < 3
            || w.starts_with("http")
            || w.contains(['%', '{', '}', '<', '>', '$', '@', '&'])
        {
            continue;
        }
        let ident_chars = w
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-/".contains(c));
        if !ident_chars {
            continue;
        }
        let b = w.as_bytes();
        let snake = (1..b.len().saturating_sub(1)).any(|i| {
            b[i] == b'_' && b[i - 1].is_ascii_alphanumeric() && b[i + 1].is_ascii_alphanumeric()
        });
        let file = match w.rsplit_once('.') {
            Some((name, ext)) => {
                !name.is_empty()
                    && (2..=5).contains(&ext.len())
                    && ext
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && ext.chars().any(|c| c.is_ascii_lowercase())
                    && !name.ends_with('.')
            }
            None => false,
        };
        let flag = w.starts_with("--")
            && w.len() > 3
            && w[2..].starts_with(|c: char| c.is_ascii_alphabetic());
        if snake || file || flag {
            out.push(format!("code:{}", w));
        }
    }
    out
}

/// Length of a printf conversion spec following '%' (without the '%').
fn printf_spec_len(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    let mut j = 0;
    // positional: 1$
    let mut k = 0;
    while k < b.len() && b[k].is_ascii_digit() {
        k += 1;
    }
    if k > 0 && k < b.len() && b[k] == b'$' {
        j = k + 1;
    }
    while j < b.len() && b"#0- +'".contains(&b[j]) {
        j += 1;
    }
    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'*') {
        j += 1;
    }
    if j < b.len() && b[j] == b'.' {
        j += 1;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'*') {
            j += 1;
        }
    }
    while j < b.len() && b"hlLqjzt".contains(&b[j]) {
        j += 1;
    }
    if j < b.len() && b"diouxXeEfFgGaAcspn@".contains(&b[j]) {
        // "%s" followed by letters is still a placeholder in printf strings, but
        // "100% sure" (space flag + 's') is not: require no space flag for plain text.
        if rest[..j].contains(' ') {
            return None;
        }
        return Some(j + 1);
    }
    None
}

/// Parse an HTML/XML tag at the start of `s`. Returns (token, byte length).
fn html_tag(s: &str) -> Option<(String, usize)> {
    let end = s.find('>')?;
    let inner = &s[1..end];
    if inner.is_empty() || inner.contains('<') || inner.len() > 300 {
        return None;
    }
    let closing = inner.starts_with('/');
    let body = inner.trim_start_matches('/');
    let name: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
        .collect();
    if name.is_empty() || !name.chars().next().unwrap().is_ascii_alphabetic() {
        return None;
    }
    // Keep URL-bearing attributes: they must never be changed.
    let mut attrs = String::new();
    for key in ["href=", "src="] {
        if let Some(p) = body.find(key) {
            let v = &body[p + key.len()..];
            let v = v.trim_start_matches(['"', '\'']);
            let v: String = v
                .chars()
                .take_while(|c| *c != '"' && *c != '\'' && !c.is_whitespace())
                .collect();
            attrs.push_str(&format!(" {}{}", key, v));
        }
    }
    let self_closing = body.trim_end().ends_with('/');
    Some((
        format!(
            "tag:<{}{}{}{}>",
            if closing { "/" } else { "" },
            name.to_ascii_lowercase(),
            attrs,
            if self_closing { "/" } else { "" }
        ),
        end + 1,
    ))
}

/// Typical LLM chatter / formatting debris that must never end up in a .po file.
/// Symbols that ordinary text rarely needs; when one shows up in a
/// translation but not in the original, the output is usually malformed:
/// alternatives joined with "|" ("voce | voci"), Markdown ("**Salva**"),
/// escapes, tags like "[IT]", decorations, or the model's formatting.
const STRAY_SYMBOLS: &[char] = &[
    '|', '¦', '‖', '\\', '`', '*', '~', '^', '#', '=', '_', '@', '[', ']', '•', '→', '⇒', '►', '§', '¶', '¤',
];

/// Emoji and pictographs.
fn is_pictograph(c: char) -> bool {
    matches!(c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F000..=0x1F2FF)
}

/// The text without its placeholders, tags, URLs and code identifiers (which
/// are checked separately and may legitimately contain any symbol).
fn plain_text(s: &str) -> String {
    let mut t = s.to_string();
    let mut tokens: Vec<String> = placeholders(s, &[])
        .into_iter()
        .filter_map(|p| p.split_once(':').map(|(_, v)| v.to_string()))
        .collect();
    tokens.sort_by_key(|x| std::cmp::Reverse(x.len()));
    for tok in tokens {
        t = t.replace(&tok, " ");
    }
    // printf-style and named placeholders even without a format flag.
    let mut out = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' && chars.peek() == Some(&'(') {
            for d in chars.by_ref() {
                if d == ')' {
                    break;
                }
            }
            chars.next(); // the conversion letter
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

fn stray_symbols(src_raw: &str, out: &str) -> Result<(), String> {
    let (src, out) = (plain_text(src_raw), plain_text(out));
    // Present in the original (anywhere, e.g. in "color_map"), or an arrow
    // standing for the original's ">" ("Restart > From DEM").
    let allowed = |c: char| src_raw.contains(c) || (matches!(c, '→' | '⇒' | '►') && src_raw.contains('>'));
    if let Some(c) = out
        .chars()
        .find(|c| (STRAY_SYMBOLS.contains(c) || is_pictograph(*c)) && !allowed(*c))
    {
        return Err(if matches!(c, '|' | '¦' | '‖') {
            format!(
                "contains “{}”, which isn't in the original text; it looks like several alternatives joined together, but only one translation is wanted",
                c
            )
        } else {
            format!("contains “{}”, which isn't in the original text", c)
        });
    }
    // "voce / voci": words separated by a spaced slash the original doesn't have
    // (a slash between numbers, as in "3 / 10", is fine).
    if !src.contains('/') {
        let chars: Vec<char> = out.chars().collect();
        for w in chars.windows(5) {
            if w[1] == ' ' && w[2] == '/' && w[3] == ' ' && w[0].is_alphabetic() && w[4].is_alphabetic() {
                return Err("contains “ / ” between words, which isn't in the original text; it looks like alternatives, but only one translation is wanted".into());
            }
        }
    }
    Ok(())
}

fn garbage(src: &str, out: &str) -> Result<(), String> {
    let lo = out.to_lowercase();
    let slo = src.to_lowercase();
    let contains_new = |needle: &str| lo.contains(needle) && !slo.contains(needle);
    for needle in [
        "```",
        "<think",
        "</think",
        "msgid",
        "msgstr",
        "<|",
        "|>",
        "as an ai",
        "language model",
        "\"translation\"",
        "\"id\":",
        "[translation",
        "(translation",
        "translator's note",
        "translator note",
        "note: ",
        "nota: ",
        "i cannot translate",
        "i can't translate",
    ] {
        if contains_new(needle) {
            return Err(format!("output contains suspicious text '{}'", needle));
        }
    }
    for prefix in [
        "here is",
        "here's",
        "sure",
        "certainly",
        "translation:",
        "translated:",
        "the translation",
        "okay,",
        "ok, here",
    ] {
        if lo.starts_with(prefix) && !slo.starts_with(prefix) {
            return Err(format!("output starts with chatter '{}'", prefix));
        }
    }
    // A JSON-ish fragment in place of a translation.
    let t = out.trim();
    if (t.starts_with('{') && t.ends_with('}') || t.starts_with('[') && t.ends_with(']'))
        && t.contains(':')
        && !(src.trim().starts_with('{') || src.trim().starts_with('['))
    {
        return Err("output looks like a JSON fragment".into());
    }
    Ok(())
}

fn length_ratio(src: &str, out: &str, lang: &str) -> Result<(), String> {
    let s = src.chars().count() as f64;
    let t = out.chars().count() as f64;
    let compact = langs::is_compact(lang);
    if s < 8.0 {
        let max = s * 5.0 + 15.0;
        if t > max {
            return Err(format!(
                "translation too long ({} chars for a {} char source)",
                t, s
            ));
        }
        return Ok(());
    }
    let (lo, hi) = match (s >= 20.0, compact) {
        (true, false) => (0.3, 3.0),
        (true, true) => (0.1, 2.5),
        (false, false) => (0.2, 4.0),
        (false, true) => (0.08, 3.0),
    };
    let r = t / s;
    if r < lo {
        return Err(format!(
            "translation too short (length ratio {:.2}, min {:.2})",
            r, lo
        ));
    }
    if r > hi {
        return Err(format!(
            "translation too long (length ratio {:.2}, max {:.2})",
            r, hi
        ));
    }
    Ok(())
}

/// Strip placeholders/tags/URLs so only human text remains.
fn human_text(s: &str) -> String {
    let mut out = String::new();
    let mut depth_tag = false;
    let mut depth_brace = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => depth_tag = true,
            '>' if depth_tag => depth_tag = false,
            '{' => depth_brace += 1,
            '}' if depth_brace > 0 => depth_brace -= 1,
            '%' => {
                // skip %(name)s and %s
                if chars.peek() == Some(&'(') {
                    for x in chars.by_ref() {
                        if x == ')' {
                            break;
                        }
                    }
                }
                chars.next();
            }
            _ if depth_tag || depth_brace > 0 => {}
            _ => out.push(c),
        }
    }
    // drop URLs
    out.split_whitespace()
        .filter(|w| !w.starts_with("http://") && !w.starts_with("https://"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn script_check(src: &str, out: &str, lang: &str) -> Result<(), String> {
    let want = langs::expected_script(lang);
    if want == Script::Unknown {
        return Ok(());
    }
    let text = human_text(out);
    let letters: Vec<Script> = text
        .chars()
        .filter(|c| c.is_alphabetic())
        .filter_map(langs::script_of)
        .collect();
    if letters.len() < 3 {
        return Ok(());
    }
    let matches = |s: Script| match want {
        Script::Japanese => s == Script::Japanese || s == Script::Han,
        Script::Hangul => s == Script::Hangul || s == Script::Han,
        w => s == w,
    };
    let good = letters.iter().filter(|s| matches(**s)).count();
    let frac = good as f64 / letters.len() as f64;
    if want == Script::Latin {
        if frac < 0.5 {
            return Err("translation is not written in the Latin script".into());
        }
    } else if good == 0 {
        // Entirely Latin output for e.g. Russian: only acceptable when the
        // source itself is a proper noun / acronym (identical copy).
        let src_text = human_text(src);
        if text.trim() != src_text.trim() {
            return Err(format!(
                "translation contains no characters of the expected script ({:?})",
                want
            ));
        }
    }
    Ok(())
}

fn repetition(src: &str, out: &str) -> Result<(), String> {
    let words: Vec<String> = out.split_whitespace().map(|w| w.to_lowercase()).collect();
    let swords: Vec<String> = src.split_whitespace().map(|w| w.to_lowercase()).collect();
    // Same token repeated many times in a row.
    let mut run = 1;
    for i in 1..words.len() {
        if words[i] == words[i - 1] && words[i].chars().any(|c| c.is_alphanumeric()) {
            run += 1;
            if run >= 4 {
                return Err(format!("repetitive output ('{}' repeated)", words[i]));
            }
        } else {
            run = 1;
        }
    }
    // Frequent n-grams (LLM loops).
    for n in 2..=4 {
        if words.len() < n * 4 {
            break;
        }
        let mut counts: std::collections::HashMap<&[String], usize> =
            std::collections::HashMap::new();
        for w in words.windows(n) {
            *counts.entry(w).or_default() += 1;
        }
        for (gram, c) in counts {
            if c >= 4 {
                let sc = swords
                    .windows(n)
                    .filter(|w| {
                        w.iter()
                            .map(|s| s.as_str())
                            .eq(gram.iter().map(|s| s.as_str()))
                    })
                    .count();
                if c > sc * 2 + 2 {
                    return Err(format!("repetitive output ('{}' x{})", gram.join(" "), c));
                }
            }
        }
    }
    // CJK-style loops without spaces: a long chunk repeated many times.
    let chars: Vec<char> = out.chars().collect();
    if chars.len() > 40 {
        for len in 4..=12 {
            let mut i = 0;
            while i + len * 4 <= chars.len() {
                let chunk = &chars[i..i + len];
                if (1..4).all(|k| &chars[i + k * len..i + (k + 1) * len] == chunk)
                    && !src.contains(&chunk.iter().collect::<String>())
                {
                    return Err("repetitive output (looping text)".into());
                }
                i += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn py() -> Vec<String> {
        vec!["python-format".to_string()]
    }

    #[test]
    fn placeholder_extraction() {
        let p = placeholders(
            "Hello %(name)s, you have %d new <b>messages</b> {count} &amp; see https://x.org/a.",
            &py(),
        );
        assert!(p.contains(&"ph:%(name)s".to_string()));
        assert!(p.contains(&"ph:%d".to_string()));
        assert!(p.contains(&"ph:{count}".to_string()));
        assert!(p.contains(&"tag:<b>".to_string()));
        assert!(p.contains(&"tag:</b>".to_string()));
        assert!(p.contains(&"entity:&amp;".to_string()));
        assert!(p.contains(&"url:https://x.org/a".to_string()));
        assert!(placeholders("100% sure", &[]).is_empty());
        assert_eq!(
            placeholders("orig_iat field is required.", &[]),
            vec!["code:orig_iat".to_string()]
        );
        assert!(
            placeholders(
                "Upload odm_orthophoto.tif or use --fast-orthophoto, e.g. now",
                &[]
            )
            .len()
                == 2
        );
        assert!(placeholders("Done. Next step, etc. e.g. i.e.", &[]).is_empty());
        assert!(placeholders("Use {{ var }} here", &[]).contains(&"ph:{{var}}".to_string()));
    }

    #[test]
    fn accepts_good() {
        let r = check_one(
            "Restart required: %(plugin)s ",
            "Riavvio necessario: %(plugin)s",
            &py(),
            "it",
            None,
        );
        assert_eq!(r.unwrap(), "Riavvio necessario: %(plugin)s ");
        assert_eq!(check_one("CSS", "CSS", &[], "it", None).unwrap(), "CSS");
        assert_eq!(
            check_one("Save", "\"Salva\"", &[], "it", None).unwrap(),
            "Salva"
        );
        assert!(check_one("Save", "Сохранить", &[], "ru", None).is_ok());
        assert_eq!(
            check_one(
                "User account is disabled.",
                "用户账户已禁用 .",
                &[],
                "zh_Hans",
                None
            )
            .unwrap(),
            "用户账户已禁用."
        );
        assert_eq!(
            check_one(
                "Bad header. No key.",
                "无效的授权信头 。 未提供.",
                &[],
                "zh_Hans",
                None
            )
            .unwrap(),
            "无效的授权信头。未提供."
        );
        assert_eq!(
            check_one("Done.", "Terminato .", &[], "it", None).unwrap(),
            "Terminato."
        );
        assert_eq!(
            check_one("Loading ...", "Caricamento ...", &[], "it", None).unwrap(),
            "Caricamento ..."
        );
        assert_eq!(
            check_one("Warning!", "Attention !", &[], "fr", None).unwrap(),
            "Attention !"
        );
    }

    #[test]
    fn stray_symbols_are_flagged() {
        let f = py();
        let e = check_one("entry", "voce | voci", &f, "it", None).unwrap_err();
        assert!(e.contains("“|”") && e.contains("alternatives"), "{}", e);
        assert!(check_one("entry", "voce / voci", &f, "it", None).is_err());
        assert!(check_one("Save", "**Salva**", &f, "it", None).is_err());
        assert!(check_one("Save", "Salva\\", &f, "it", None).is_err());
        // Symbols the original has, or ordinary slashes, are fine.
        assert!(check_one("Use | to separate values", "Usa | per separare i valori", &f, "it", None).is_ok());
        assert!(check_one("and/or", "e/o", &f, "it", None).is_ok());
        assert!(check_one("Save", "Salva", &f, "it", None).is_ok());
        assert!(check_one("Save", "[IT] Salva", &f, "it", None).is_err());
        assert!(check_one("Save", "Salva ✅", &f, "it", None).is_err());
        assert!(check_one("Next", "→ Avanti", &f, "it", None).is_err());
        // Slashes between numbers/placeholders, and symbols inside placeholders, are fine.
        assert!(check_one("%(used)s of %(total)s used", "%(used)s / %(total)s käytetty", &f, "fi", None).is_ok());
        assert!(check_one("Upload %(file_name)s", "Carica %(file_name)s", &f, "it", None).is_ok());
        assert!(stray_symbols("Not a valid color_map value", "有効なcolor_mapの値ではない").is_ok());
        assert!(stray_symbols("Press Restart > From DEM", "Premi Riavvia → Da DEM").is_ok());
    }

    #[test]
        fn rejects_bad() {
        let f = py();
        assert!(check_one("Hello %(name)s", "Ciao", &f, "it", None).is_err());
        assert!(check_one("Hello %(name)s", "Ciao %(nome)s", &f, "it", None).is_err());
        assert!(check_one("Hello", "   ", &f, "it", None).is_err());
        assert!(check_one(
            "Hello world",
            "Here is the translation: Ciao mondo",
            &f,
            "it",
            None
        )
        .is_err());
        assert!(check_one("Click <b>here</b>", "Clicca qui", &f, "it", None).is_err());
        assert!(check_one(
            "Save",
            "Salva questo file adesso per favore subito ok grazie mille",
            &f,
            "it",
            None
        )
        .is_err());
        assert!(check_one(
            "This is a fairly long sentence for testing",
            "Breve",
            &f,
            "it",
            None
        )
        .is_err());
        assert!(check_one("Line one\nLine two", "Riga uno riga due", &f, "it", None).is_err());
        assert!(check_one("Save the file", "保存文件", &f, "it", None).is_err());
        assert!(check_one("Save the file", "Save the file now", &f, "ru", None).is_err());
        assert!(check_one(
            "Open the project settings page",
            "Open the project settings page",
            &f,
            "it",
            None
        )
        .is_err());
        assert!(check_one(
            "Processing images",
            "Elaborazione elaborazione elaborazione elaborazione elaborazione",
            &f,
            "it",
            None
        )
        .is_err());
        assert!(check_one("Hello", "```Ciao```", &f, "it", None).is_err());
        assert!(check_one(
            "orig_iat field is required.",
            "è richiesto un campo orig iat.",
            &f,
            "it",
            None
        )
        .is_err());
        assert!(check_one(
            "orig_iat field is required.",
            "Il campo orig_iat è obbligatorio.",
            &f,
            "it",
            None
        )
        .is_ok());
    }

    #[test]
    fn plurals() {
        let flags = py();
        let src = Src {
            msgid: "One file",
            msgid_plural: Some("%(count)s files"),
            flags: &flags,
            lang: "it",
            nplurals: 2,
        };
        assert!(check(&src, &["Un file".into(), "%(count)s file".into()]).is_ok());
        assert!(check(&src, &["Un file".into()]).is_err());
        assert!(check(&src, &["Un file".into(), "file".into()]).is_ok()); // numeric may be omitted
        assert!(check(&src, &["Un file %(x)s".into(), "%(count)s file".into()]).is_err());
    }
}
