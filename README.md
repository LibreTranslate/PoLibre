# PoLibre

<img src="assets/icon.svg" width="72" alt="PoLibre" align="right">

Your free and open source `.po` editor for machine assisted translation with built-in support for
[LibreTranslate](https://github.com/LibreTranslate/LibreTranslate) and both offline/online LLMs (Claude, Gemini, Ollama). 

<img alt="PoLibre Screenshot" src="https://github.com/user-attachments/assets/2dfdca01-19c1-43b5-81f2-cc1f76d4137c" />

<img alt="PoLibre Screenshot" src="https://github.com/user-attachments/assets/b559b82d-f8a8-4f2b-be35-c4577da0a9ba" />

It's a desktop application, but also comes with CLI support for scripts and workflow automations. Machine translation features are optional and the software can be used as a standalone .po editor.

<p><a href="../../releases"><img alt="Download" src="https://img.shields.io/badge/Download-0060B8?style=for-the-badge&logo=data%3Aimage%2Fsvg%2Bxml%3Bbase64%2CPHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0ibm9uZSIgc3Ryb2tlPSJ3aGl0ZSIgc3Ryb2tlLXdpZHRoPSIyLjUiIHN0cm9rZS1saW5lY2FwPSJyb3VuZCIgc3Ryb2tlLWxpbmVqb2luPSJyb3VuZCI%2BPHBhdGggZD0iTTEyIDN2MTIiLz48cGF0aCBkPSJNNyAxMGw1IDUgNS01Ii8%2BPHBhdGggZD0iTTQgMjBoMTYiLz48L3N2Zz4%3D"></a></p>

## Features

- Automatic translation suggestions for every `.po` file and language in a project.
- Works with [LibreTranslate](https://github.com/LibreTranslate/LibreTranslate), Claude, Gemini, OpenAI and OpenAI-compatible servers (Ollama,
  OpenRouter), or the Claude Code, Gemini and Codex command line tools.
- Gives LLMs context: the code around each string, a project description, a glossary and
  developer comments.
- Checks every translation (placeholders, HTML, URLs, plurals, length, ...) before writing it.
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

Run `polibre` to open the app: pick a project folder and a translation service, select the
languages and press **Translate**. Click **Edit** on a language to review its translations.

It also works from the command line:

```sh
polibre scan path/to/project                      # languages and missing strings
polibre translate . -l it,de -p libretranslate -k KEY --dry-run
polibre translate . -p anthropic -m claude-opus-5-5 --limit 20
polibre check . -l it --only-auto                 # re-check PoLibre's translations
polibre translate . --no-config -p anthropic -m claude-opus-5-5 --report report.json
```

Run `polibre --help` for all options.

## License

[AGPLv3](LICENSE). 

The bundled Inter font is © The Inter Project Authors,
under the [SIL Open Font License 1.1](assets/fonts/Inter-LICENSE.txt).

## Credits

Originally inspired by [dj-translatemessages](https://github.com/efe/dj-translatemessages)
