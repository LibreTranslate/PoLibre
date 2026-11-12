// PoLibre - fill untranslated gettext .po strings automatically.
// Copyright (C) 2026 PoLibre contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

mod cli;
mod config;
mod engine;
#[cfg(feature = "gui")]
mod gui;
#[cfg(feature = "gui")]
mod icon;
mod langs;
mod po;
mod project;
mod providers;
mod validate;

/// Whether a comment line is a "Translated by …" note that earlier versions
/// of this tool wrote. They're removed when an entry is translated again or
/// approved. Covers every wording used: "# Translated by <model>",
/// "# Translated by <model> on <date>[, needs review]" and, from before the
/// rename to PoLibre, "# AutoTranslate: ...".
pub fn is_auto_comment(line: &str) -> bool {
    let Some(c) = line.strip_prefix("# ") else {
        return false;
    };
    if c.starts_with("AutoTranslate:") {
        return true;
    }
    let Some(rest) = c.strip_prefix("Translated by ") else {
        return false;
    };
    // "Translated by <model>": model ids and service names have no spaces.
    if !rest.is_empty() && !rest.contains(char::is_whitespace) {
        return true;
    }
    // Older: "Translated by <model> on YYYY-MM-DD[, needs review]"
    let rest = rest.strip_suffix(", needs review").unwrap_or(rest);
    let Some((by, date)) = rest.rsplit_once(" on ") else {
        return false;
    };
    let d = date.as_bytes();
    !by.is_empty()
        && !by.contains(' ')
        && d.len() == 10
        && d.iter().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn old_notes_are_recognized() {
        assert!(super::is_auto_comment("# Translated by claude-opus-5-5"));
        assert!(super::is_auto_comment(
            "# Translated by opus on 2026-11-11, needs review"
        ));
        assert!(super::is_auto_comment(
            "# AutoTranslate: opus, needs review"
        ));
        assert!(!super::is_auto_comment(
            "# Translated by John Smith on Monday"
        ));
        assert!(!super::is_auto_comment("# Translated by maria on 2020"));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(feature = "gui")]
    {
        if args.is_empty() || args[0] == "gui" {
            let path = args.get(1).cloned();
            if let Err(e) = gui::run(path) {
                eprintln!("error: cannot start GUI: {}", e);
                eprintln!("hint: run `polibre --help` for the command line interface");
                std::process::exit(1);
            }
            return;
        }
    }
    std::process::exit(cli::main(args));
}
