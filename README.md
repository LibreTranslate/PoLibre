# PoLibre

<img src="assets/icon.svg" width="72" alt="PoLibre">

A free and open source `.po` editor for machine assisted translation with built-in support for
[LibreTranslate](https://github.com/LibreTranslate/LibreTranslate) or both offline and online LLMs (Claude, Gemini, Ollama). Single binary desktop app with a command line.


## Features

- Automatic translation suggestions for every `.po` file and language in a project.
- Works with LibreTranslate, Claude, Gemini, OpenAI and OpenAI-compatible servers (Ollama,
  OpenRouter), or the Claude Code, Gemini and Codex command line tools.
- Gives LLMs context: the code around each string, a project description, a glossary and
  developer comments.
- Checks every translation (placeholders, HTML, URLs, plurals, length, ...) before writing it.
- Marks new translations `#, fuzzy` with a note naming the model; other entries stay untouched.
- Built-in editor to review: approve or fix translations, get suggestions, reuse existing
  translations, compare languages, keyboard shortcuts.
- Command line for scripts and CI, with GitHub Actions annotations and a JSON report.

## Install

Download from the [releases page](../../releases)

## Build

```sh
cargo build --release                        # desktop app + command line
cargo build --release --no-default-features  # command line only
```

## Usage

Run `polibre` to open the app: pick a project folder and a translation service, tick the
languages and press **Translate**. Click **Edit** on a language to review its translations.

The same binary works from the command line:

```sh
polibre scan path/to/project                      # languages and missing strings
polibre translate . -l it,de -p libretranslate -k KEY --dry-run
polibre translate . -p anthropic -m claude-opus-5-5 --limit 20
polibre check . -l it --only-auto                 # re-check PoLibre's translations
```

Services (`-p`): `libretranslate`, `anthropic`, `google`, `openai`, `openai-compatible`
(with `--url`), and `claude-cli`, `gemini-cli`, `codex-cli` (installed tools, no key needed).
API keys can also come from `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `OPENAI_API_KEY` or
`LIBRETRANSLATE_API_KEY`. Settings are saved in `~/.config/polibre/config.json` and shared
with the app; `--no-config` ignores them. Run `polibre --help` for all options.

### CI

```sh
polibre translate . --no-config -p anthropic -m claude-opus-5-5 --report report.json
```

Under GitHub Actions, rejected strings show up as warnings. `--fail-on-reject` makes the
job fail when any are rejected (exit code 3). See
[`examples/github-workflow.yml`](examples/github-workflow.yml) for a workflow that opens a
pull request with the new translations.

## License

[AGPL-3.0-or-later](LICENSE). The bundled Inter font is © The Inter Project Authors,
under the [SIL Open Font License 1.1](assets/fonts/Inter-LICENSE.txt).

## Credits

Originally inspired by [dj-translatemessages](https://github.com/efe/dj-translatemessages)