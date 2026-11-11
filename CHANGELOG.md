# Changelog

All notable changes to Terrarium are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Add new entries under **Unreleased** as you make changes, grouped as Added,
Changed, Deprecated, Removed, Fixed or Security. `scripts/release.sh` turns
that section into a version when you release.

## [Unreleased]

### Added

- Chat with a local Ollama model, with streaming replies, Markdown and code
  blocks with a copy button, text file attachments (up to 2 MB) and an optional
  Think mode for models that support reasoning.
- Assistant tools: `calculate`, `search_notes`, `read_note`, `create_note`,
  `news_headlines`, `read_article`, `calendar_events` and `add_event`, each shown
  in the reply with a link to what it used.
- Calendar tab with timed and all-day events, stored in SQLite.
- Notes tab with Markdown, autosave, search and pinning, stored in SQLite.
- Newsstand tab: an RSS, Atom and JSON Feed reader with sanitized article HTML
  and automatic refresh of stale feeds.
- Calculator tab, sharing the assistant's exact expression evaluator and
  history.
- Theme picker with five themes (Frutiger Aero, Frutiger Eco, Dark Aero,
  Technozen, DORFic), remembered per browser.
- Clock and app version in the sidebar.
- Configuration through `OLLAMA_*`, `TERRARIUM_DB` and `LEPTOS_SITE_ADDR`
  environment variables.
- README, this changelog and a release script.
