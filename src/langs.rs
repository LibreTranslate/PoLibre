// SPDX-License-Identifier: AGPL-3.0-or-later
//! Locale code helpers: human readable names and expected writing scripts.

pub const NAMES: &[(&str, &str)] = &[
    ("af", "Afrikaans"),
    ("ain", "Ainu"),
    ("ang", "Old English"),
    ("enm", "Middle English"),
    ("frc", "Cajun French"),
    ("frm", "Middle French"),
    ("hak", "Hakka Chinese"),
    ("mnc", "Manchu"),
    ("mnw", "Mon"),
    ("nan", "Min Nan Chinese"),
    ("nan_Hant", "Min Nan Chinese (Traditional)"),
    ("yue", "Cantonese"),
    ("zun", "Zuni"),
    ("am", "Amharic"),
    ("ar", "Arabic"),
    ("as", "Assamese"),
    ("ast", "Asturian"),
    ("az", "Azerbaijani"),
    ("be", "Belarusian"),
    ("bg", "Bulgarian"),
    ("bn", "Bengali"),
    ("bo", "Tibetan"),
    ("br", "Breton"),
    ("bs", "Bosnian"),
    ("ca", "Catalan"),
    ("ckb", "Central Kurdish"),
    ("cs", "Czech"),
    ("cy", "Welsh"),
    ("da", "Danish"),
    ("de", "German"),
    ("dsb", "Lower Sorbian"),
    ("el", "Greek"),
    ("en", "English"),
    ("en_GB", "English (UK)"),
    ("en_AU", "English (Australia)"),
    ("eo", "Esperanto"),
    ("es", "Spanish"),
    ("es_AR", "Spanish (Argentina)"),
    ("es_MX", "Spanish (Mexico)"),
    ("et", "Estonian"),
    ("eu", "Basque"),
    ("fa", "Persian"),
    ("fi", "Finnish"),
    ("fil", "Filipino"),
    ("fr", "French"),
    ("fy", "Frisian"),
    ("ga", "Irish"),
    ("gd", "Scottish Gaelic"),
    ("gl", "Galician"),
    ("gu", "Gujarati"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hr", "Croatian"),
    ("hsb", "Upper Sorbian"),
    ("hsn", "Xiang Chinese"),
    ("hu", "Hungarian"),
    ("hy", "Armenian"),
    ("ia", "Interlingua"),
    ("id", "Indonesian"),
    ("ig", "Igbo"),
    ("io", "Ido"),
    ("is", "Icelandic"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("jv", "Javanese"),
    ("ka", "Georgian"),
    ("kab", "Kabyle"),
    ("kk", "Kazakh"),
    ("km", "Khmer"),
    ("kn", "Kannada"),
    ("ko", "Korean"),
    ("ku", "Kurdish"),
    ("ky", "Kyrgyz"),
    ("lb", "Luxembourgish"),
    ("lo", "Lao"),
    ("lt", "Lithuanian"),
    ("lv", "Latvian"),
    ("lzh", "Literary Chinese"),
    ("mk", "Macedonian"),
    ("ml", "Malayalam"),
    ("mn", "Mongolian"),
    ("mr", "Marathi"),
    ("ms", "Malay"),
    ("my", "Burmese"),
    ("nb", "Norwegian Bokmål"),
    ("nb_NO", "Norwegian Bokmål"),
    ("nds", "Low German"),
    ("ne", "Nepali"),
    ("nl", "Dutch"),
    ("nn", "Norwegian Nynorsk"),
    ("oc", "Occitan"),
    ("or", "Odia"),
    ("os", "Ossetic"),
    ("pa", "Punjabi"),
    ("pl", "Polish"),
    ("ps", "Pashto"),
    ("pt", "Portuguese"),
    ("pt_BR", "Portuguese (Brazil)"),
    ("pt_PT", "Portuguese (Portugal)"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("si", "Sinhala"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("sq", "Albanian"),
    ("sr", "Serbian"),
    ("sr_Latn", "Serbian (Latin)"),
    ("sv", "Swedish"),
    ("sw", "Swahili"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("tg", "Tajik"),
    ("th", "Thai"),
    ("tk", "Turkmen"),
    ("tl", "Tagalog"),
    ("tr", "Turkish"),
    ("tt", "Tatar"),
    ("udm", "Udmurt"),
    ("ug", "Uyghur"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("uz", "Uzbek"),
    ("vi", "Vietnamese"),
    ("yo", "Yoruba"),
    ("zh", "Chinese"),
    ("zh_CN", "Chinese (Simplified)"),
    ("zh_Hans", "Chinese (Simplified)"),
    ("zh_TW", "Chinese (Traditional)"),
    ("zh_Hant", "Chinese (Traditional)"),
    ("zh_HK", "Chinese (Hong Kong)"),
];

pub fn normalize(code: &str) -> String {
    code.replace('-', "_")
}

pub fn base(code: &str) -> &str {
    code.split(['_', '-', '@']).next().unwrap_or(code)
}

pub fn name(code: &str) -> String {
    let n = normalize(code);
    if let Some((_, v)) = NAMES.iter().find(|(k, _)| k.eq_ignore_ascii_case(&n)) {
        return v.to_string();
    }
    let b = base(&n);
    if let Some((_, v)) = NAMES.iter().find(|(k, _)| k.eq_ignore_ascii_case(b)) {
        if b.len() < n.len() {
            return format!("{} ({})", v, &n[b.len() + 1..]);
        }
        return v.to_string();
    }
    code.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Han,      // Chinese
    Japanese, // Kana and/or Han
    Hangul,
    Thai,
    Devanagari,
    Bengali,
    Tamil,
    Telugu,
    Kannada,
    Malayalam,
    Gujarati,
    Gurmukhi,
    Georgian,
    Armenian,
    Tibetan,
    Khmer,
    Lao,
    Myanmar,
    Sinhala,
    Ethiopic,
    Unknown,
}

/// The script a translation into `code` is expected to be written in.
pub fn expected_script(code: &str) -> Script {
    let n = normalize(code).to_lowercase();
    if n == "sr_latn" || n.ends_with("@latin") {
        return Script::Latin;
    }
    match base(&n) {
        "ru" | "uk" | "bg" | "be" | "mk" | "sr" | "kk" | "ky" | "tg" | "tt" | "mn" | "os"
        | "udm" => Script::Cyrillic,
        "el" => Script::Greek,
        "ar" | "fa" | "ur" | "ps" | "ug" | "ckb" => Script::Arabic,
        "he" | "yi" => Script::Hebrew,
        "zh" | "lzh" | "hsn" | "yue" | "hak" | "nan" => Script::Han,
        "ja" => Script::Japanese,
        "ko" => Script::Hangul,
        "th" => Script::Thai,
        "hi" | "mr" | "ne" => Script::Devanagari,
        "bn" | "as" => Script::Bengali,
        "ta" => Script::Tamil,
        "te" => Script::Telugu,
        "kn" => Script::Kannada,
        "ml" => Script::Malayalam,
        "gu" => Script::Gujarati,
        "pa" => Script::Gurmukhi,
        "ka" => Script::Georgian,
        "hy" => Script::Armenian,
        "bo" | "dz" => Script::Tibetan,
        "km" => Script::Khmer,
        "lo" => Script::Lao,
        "my" => Script::Myanmar,
        "si" => Script::Sinhala,
        "am" | "ti" => Script::Ethiopic,
        "" => Script::Unknown,
        _ => Script::Latin,
    }
}

pub fn script_of(c: char) -> Option<Script> {
    let u = c as u32;
    let s = match u {
        0x41..=0x5A | 0x61..=0x7A | 0xC0..=0x24F | 0x1E00..=0x1EFF => Script::Latin,
        0x370..=0x3FF | 0x1F00..=0x1FFF => Script::Greek,
        0x400..=0x52F => Script::Cyrillic,
        0x530..=0x58F => Script::Armenian,
        0x590..=0x5FF => Script::Hebrew,
        0x600..=0x6FF | 0x750..=0x77F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Script::Arabic,
        0x900..=0x97F => Script::Devanagari,
        0x980..=0x9FF => Script::Bengali,
        0xA00..=0xA7F => Script::Gurmukhi,
        0xA80..=0xAFF => Script::Gujarati,
        0xB80..=0xBFF => Script::Tamil,
        0xC00..=0xC7F => Script::Telugu,
        0xC80..=0xCFF => Script::Kannada,
        0xD00..=0xD7F => Script::Malayalam,
        0xD80..=0xDFF => Script::Sinhala,
        0xE00..=0xE7F => Script::Thai,
        0xE80..=0xEFF => Script::Lao,
        0xF00..=0xFFF => Script::Tibetan,
        0x1000..=0x109F => Script::Myanmar,
        0x10A0..=0x10FF => Script::Georgian,
        0x1200..=0x139F => Script::Ethiopic,
        0x1780..=0x17FF => Script::Khmer,
        0x3040..=0x30FF => Script::Japanese,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => Script::Han,
        0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => Script::Hangul,
        _ => return None,
    };
    Some(s)
}

/// Languages whose text is typically much shorter (in characters) than English.
pub fn is_compact(code: &str) -> bool {
    matches!(
        expected_script(code),
        Script::Han | Script::Japanese | Script::Hangul
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(name("it"), "Italian");
        assert_eq!(name("pt_BR"), "Portuguese (Brazil)");
        assert_eq!(name("zh-Hans"), "Chinese (Simplified)");
        assert_eq!(name("de_CH"), "German (CH)");
        assert_eq!(expected_script("uk"), Script::Cyrillic);
        assert_eq!(expected_script("it"), Script::Latin);
    }
}
