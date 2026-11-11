// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistent settings (~/.config/polibre/config.json) and provider
//! definitions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    ClaudeCli,
    GeminiCli,
    CodexCli,
    Anthropic,
    Google,
    Openai,
    OpenaiCompatible,
    Libretranslate,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 8] = [
        ProviderKind::Libretranslate,
        ProviderKind::ClaudeCli,
        ProviderKind::GeminiCli,
        ProviderKind::CodexCli,
        ProviderKind::Anthropic,
        ProviderKind::Google,
        ProviderKind::Openai,
        ProviderKind::OpenaiCompatible,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::ClaudeCli => "claude-cli",
            ProviderKind::GeminiCli => "gemini-cli",
            ProviderKind::CodexCli => "codex-cli",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Google => "google",
            ProviderKind::Openai => "openai",
            ProviderKind::OpenaiCompatible => "openai-compatible",
            ProviderKind::Libretranslate => "libretranslate",
        }
    }

    pub fn from_id(s: &str) -> Option<ProviderKind> {
        let s = s.to_ascii_lowercase();
        let s = match s.as_str() {
            "claude" | "claude-code" => "claude-cli",
            "gemini" => "gemini-cli",
            "codex" => "codex-cli",
            "gemini-api" | "google-ai" => "google",
            "libre" | "lt" => "libretranslate",
            "ollama" | "openrouter" | "compatible" => "openai-compatible",
            other => other,
        };
        Self::ALL.iter().copied().find(|k| k.id() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::ClaudeCli => "Claude Code (local CLI)",
            ProviderKind::GeminiCli => "Gemini CLI (local)",
            ProviderKind::CodexCli => "Codex CLI (local)",
            ProviderKind::Anthropic => "Anthropic API",
            ProviderKind::Google => "Google Gemini API",
            ProviderKind::Openai => "OpenAI API",
            ProviderKind::OpenaiCompatible => "OpenAI-compatible (Ollama, OpenRouter, ...)",
            ProviderKind::Libretranslate => "LibreTranslate",
        }
    }

    pub fn is_llm(self) -> bool {
        self != ProviderKind::Libretranslate
    }
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn is_cli(self) -> bool {
        matches!(
            self,
            ProviderKind::ClaudeCli | ProviderKind::GeminiCli | ProviderKind::CodexCli
        )
    }
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn needs_key(self) -> bool {
        matches!(
            self,
            ProviderKind::Anthropic | ProviderKind::Google | ProviderKind::Openai
        )
    }
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn uses_url(self) -> bool {
        matches!(
            self,
            ProviderKind::Libretranslate | ProviderKind::OpenaiCompatible
        )
    }
    pub fn cli_binary(self) -> Option<&'static str> {
        match self {
            ProviderKind::ClaudeCli => Some("claude"),
            ProviderKind::GeminiCli => Some("gemini"),
            ProviderKind::CodexCli => Some("codex"),
            _ => None,
        }
    }
    /// Environment variables checked (in order) for an API key.
    pub fn key_env(self) -> &'static [&'static str] {
        match self {
            ProviderKind::Anthropic => &["ANTHROPIC_API_KEY"],
            ProviderKind::Google => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            ProviderKind::Openai => &["OPENAI_API_KEY"],
            ProviderKind::OpenaiCompatible => &["OPENAI_COMPATIBLE_API_KEY", "OPENROUTER_API_KEY"],
            ProviderKind::Libretranslate => &["LIBRETRANSLATE_API_KEY", "LT_API_KEY"],
            _ => &[],
        }
    }
    pub fn default_url(self) -> &'static str {
        match self {
            ProviderKind::Libretranslate => "https://libretranslate.com",
            ProviderKind::OpenaiCompatible => "http://localhost:11434/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::Google => "https://generativelanguage.googleapis.com",
            ProviderKind::Openai => "https://api.openai.com/v1",
            _ => "",
        }
    }
    /// Suggested models (first = default). Users can type any model id, and
    /// API providers can refresh the list from the provider.
    pub fn suggested_models(self) -> &'static [&'static str] {
        match self {
            ProviderKind::ClaudeCli => &[
                "opus",
                "sonnet",
                "haiku",
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
            ],
            ProviderKind::Anthropic => &[
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
                "claude-fable-5-2",
            ],
            ProviderKind::GeminiCli => &["", "gemini-2.5-pro", "gemini-2.5-flash"],
            ProviderKind::Google => &["gemini-2.5-pro", "gemini-2.5-flash"],
            ProviderKind::CodexCli => &["", "gpt-5", "gpt-5-mini"],
            ProviderKind::Openai => &["gpt-5", "gpt-5-mini", "gpt-4.1"],
            ProviderKind::OpenaiCompatible => &["llama3.1", "qwen2.5", "mistral"],
            ProviderKind::Libretranslate => &[],
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderSettings {
    pub model: String,
    pub api_key: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: Option<ProviderKind>,
    pub providers: BTreeMap<ProviderKind, ProviderSettings>,
    pub batch_size: usize,
    pub add_comment: bool,
    pub mark_fuzzy: bool,
    pub include_fuzzy: bool,
    pub source_lang: String,
    pub last_project: String,
    /// "light" or "dark".
    pub theme: String,
    /// Language shown for comparison in the editor ("" for none).
    pub reference_lang: String,
    /// The translation settings panel is collapsed.
    pub hide_settings: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: None,
            providers: BTreeMap::new(),
            batch_size: 0,
            add_comment: true,
            mark_fuzzy: true,
            include_fuzzy: false,
            source_lang: "en".into(),
            last_project: String::new(),
            theme: "light".into(),
            reference_lang: String::new(),
            hide_settings: false,
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLIBRE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    Some(config_dir()?.join("polibre").join("config.json"))
}

/// Settings saved under the program's former name, read once if no PoLibre
/// settings exist yet (the next save writes them to the new location).
fn legacy_config_path() -> Option<PathBuf> {
    if std::env::var("POLIBRE_CONFIG").is_ok() {
        return None;
    }
    Some(config_dir()?.join("autotranslate").join("config.json"))
}

fn config_dir() -> Option<PathBuf> {
    Some(if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(x)
    } else if cfg!(windows) {
        PathBuf::from(std::env::var("APPDATA").ok()?)
    } else {
        PathBuf::from(std::env::var("HOME").ok()?).join(".config")
    })
}

impl Config {
    pub fn load() -> Config {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .or_else(|| legacy_config_path().and_then(|p| std::fs::read_to_string(p).ok()))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<PathBuf> {
        let p = config_path()
            .ok_or_else(|| std::io::Error::other("cannot determine config directory"))?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(self).unwrap())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(p)
    }

    pub fn provider_settings(&self, k: ProviderKind) -> ProviderSettings {
        self.providers.get(&k).cloned().unwrap_or_default()
    }
}

/// Fully resolved provider configuration used for a run.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub model: String,
    pub api_key: String,
    pub url: String,
}

impl ProviderConfig {
    /// Resolve settings: explicit value > saved config > environment > default.
    pub fn resolve(kind: ProviderKind, s: &ProviderSettings) -> ProviderConfig {
        let mut api_key = s.api_key.clone();
        if api_key.is_empty() {
            if let Ok(k) = std::env::var("POLIBRE_API_KEY") {
                api_key = k;
            }
        }
        if api_key.is_empty() {
            for e in kind.key_env() {
                if let Ok(k) = std::env::var(e) {
                    if !k.trim().is_empty() {
                        api_key = k;
                        break;
                    }
                }
            }
        }
        let model = if s.model.is_empty() {
            kind.suggested_models()
                .first()
                .map(|m| m.to_string())
                .unwrap_or_default()
        } else {
            s.model.clone()
        };
        let url = if s.url.is_empty() {
            kind.default_url().to_string()
        } else {
            s.url.clone()
        };
        ProviderConfig {
            kind,
            model,
            api_key: api_key.trim().to_string(),
            url: url.trim_end_matches('/').to_string(),
        }
    }

    pub fn describe(&self) -> String {
        if self.kind.is_llm() && !self.model.is_empty() {
            format!("{}/{}", self.kind.id(), self.model)
        } else {
            self.kind.id().to_string()
        }
    }
}
