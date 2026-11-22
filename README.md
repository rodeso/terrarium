# Terrarium

A personal productivity app that runs entirely on your own machine: a chat
assistant powered by a local [Ollama](https://ollama.com) model, plus web
search, a calendar, notes, a news reader and a calculator that the assistant
can use.

Written in Rust with [Leptos](https://leptos.dev) (server-rendered, hydrated
in the browser) and [Actix Web](https://actix.rs). Data is kept in a local
SQLite file. Nothing leaves your computer except the news feeds you subscribe
to and web searches, which go to search engines through your own
[SearXNG](https://docs.searxng.org).

## Features

- **Chat** streams replies from a local model as they're written, renders
  Markdown and code blocks, accepts a text file attachment (up to 2 MB), and has
  an optional **Think** mode for models that support reasoning.
- **Tools.** The assistant can call these on its own:
  - `calculate` evaluates expressions exactly instead of guessing at
    arithmetic (including `sum`, `prod`, `prime(k)`, `round`, `gcd`, …).
  - `search_notes`, `read_note` and `create_note` work with your notes.
  - `news_headlines` and `read_article` read your subscribed feeds.
  - `calendar_events` and `add_event` read and update your calendar.
  - `web_search` looks things up on the web when a question needs recent or
    specific facts.

  Each tool call is shown in the reply, with a link to the note, article, day
  or search results it used.
- **Search.** Web search through a SearXNG instance on your own machine, with no
  account and no tracking cookies.
- **Calendar.** Month view with timed and all-day events.
- **Notes.** Markdown notes with autosave, search and pinning.
- **Newsstand.** RSS, Atom and JSON Feed reader. Paste a feed or a site address.
  Article HTML is sanitized before it is stored.
- **Calculator.** Uses the same evaluator as the assistant, with a shared
  history of what you and the assistant worked out.
- **Themes.** Frutiger Aero, Frutiger Eco, Dark Aero, Technozen and DORFic,
  remembered per browser.

## Requirements

- [Ollama](https://ollama.com) running on your computer, with a model that
  supports tool calling. The default is `bonsai-2:27b`:
  ```bash
  ollama pull bonsai-2:27b
  ```
- [Docker](https://www.docker.com) (or any tool that runs `compose.yaml`) to
  run Terrarium and web search in containers.
- For development only: Rust 1.85 or newer (edition 2024) with the
  WebAssembly target, and [cargo-leptos](https://github.com/leptos-rs/cargo-leptos):
  ```bash
  rustup target add wasm32-unknown-unknown
  ```
  ```bash
  cargo install --locked cargo-leptos
  ```

Ollama stays outside Docker on purpose: on a Mac, containers can't use the
GPU, so a model in Docker would run on the CPU only and far more slowly.

## Running

### With Docker

```bash
docker compose up -d --build
```

Then open <http://127.0.0.1:8080>. This builds the optimized app (the first
build takes a while), starts it with SearXNG, and keeps both running in the
background, restarting with Docker. Run the same command again after changing
the code. Stop everything with `docker compose down`.

The Terrarium container reaches Ollama on your computer at
`host.docker.internal:11434`. On Linux, Ollama has to listen beyond localhost
for that (`OLLAMA_HOST=0.0.0.0`), or point `OLLAMA_URL` somewhere else. Model
and other `OLLAMA_*` settings can go in a `.env` file next to `compose.yaml`.

The Docker version keeps its data in a Docker volume (`terrarium-data`),
separate from `data/terrarium.db`. To copy your existing database in, stop the
dev server and run:

```bash
docker compose stop terrarium
```

```bash
docker compose run --rm --no-deps -v "$PWD/data:/import:ro" terrarium cp /import/terrarium.db /data/terrarium.db
```

```bash
docker compose start terrarium
```

### For development

Start only web search, then the dev server with live reload:

```bash
docker compose up -d searxng
```

```bash
cargo leptos watch
```

Then open <http://127.0.0.1:8080>. The dev server and the Docker version both
use port 8080; to run both at once, start Docker's on another port with
`TERRARIUM_PORT=8081 docker compose up -d`.

### Without Docker

Optimized build:

```bash
cargo leptos build --release
```

This produces the server binary `target/release/terrarium` and the browser
assets in `target/site`. Run it from the project folder so it finds
`target/site`. Note that when run directly, the binary listens on port 3000
unless you set the address:

```bash
LEPTOS_SITE_ADDR=127.0.0.1:8080 LEPTOS_ENV=PROD ./target/release/terrarium
```

At startup the server asks Ollama what the model supports. If Ollama isn't
running yet, the app still starts, but Think mode stays off until the next restart.

If SearXNG isn't running, the Search tab says so and the assistant tells you
search isn't available.

## Configuration

Everything is set with environment variables. All are optional.

| Variable | Default | What it does |
|---|---|---|
| `OLLAMA_URL` | `http://localhost:11434` | Where Ollama is running |
| `OLLAMA_MODEL` | `bonsai-2:27b` | Model used for chat |
| `OLLAMA_TEMPERATURE` | model's own (0.3 for Mistral models) | Sampling temperature, 0–2 |
| `OLLAMA_NUM_CTX` | `8192` | Context window in tokens (minimum 512; at least 12288 in Think mode) |
| `OLLAMA_NUM_PREDICT` | `2048` | Most tokens per response (minimum 64; at least 8192 in Think mode) |
| `OLLAMA_SYSTEM_PROMPT` | built-in | Replaces the base system prompt. The tool instructions are always appended |
| `TERRARIUM_DB` | `data/terrarium.db` | SQLite database file, created on first run |
| `SEARXNG_URL` | `http://127.0.0.1:8888` | SearXNG instance used for web search. Its JSON output must be on (see `searxng/settings.yml`) |
| `LEPTOS_SITE_ADDR` | `127.0.0.1:8080` with cargo-leptos, `127.0.0.1:3000` otherwise | Address the server listens on |
| `TERRARIUM_PORT` | `8080` | Docker only: the port on your computer that `compose.yaml` publishes |

In Docker, `TERRARIUM_DB`, `SEARXNG_URL` and `LEPTOS_SITE_ADDR` are set by the
image and `compose.yaml`, and `OLLAMA_URL` defaults to `http://host.docker.internal:11434`.

## Data

Notes, calendar events, feeds and articles are stored in `data/terrarium.db`.
The schema is upgraded automatically on startup. Back it up by copying the
file while the app is stopped. The `data/` folder is not tracked by git.

Chat history and the calculator history live in memory only. They are cleared
when the server restarts.

## Project layout

```
src/
  main.rs        server entry point (Actix + Leptos routes)
  lib.rs         module list and browser entry point (hydrate)
  app.rs         page shell, sidebar, routing, theme picker, clock
  chat.rs        Chat tab and its server functions
  server.rs      Ollama client, tool definitions, tool loop, streaming (server only)
  calendar.rs    Calendar tab
  notes.rs       Notes tab and note search/ranking
  news.rs        Newsstand tab
  feeds.rs       feed fetching, parsing and sanitizing (server only)
  search.rs      Search tab and its server function
  websearch.rs   SearXNG client and result formatting for the model (server only)
  calculator.rs  Calculator tab
  calc.rs        expression evaluator shared by the calculator and the model
  db.rs          SQLite storage and migrations (server only)
  dates.rs       calendar date arithmetic
  markdown.rs    safe Markdown rendering for model replies
  emoji.rs       emoji detection and wrapping
style/main.css   all styles and themes
public/          static files served as-is
Dockerfile       builds the optimized app into a small image
compose.yaml     Terrarium and SearXNG containers
searxng/         SearXNG settings (JSON output on, rate limiter off)
```

## Development

```bash
cargo test --features ssr
```

```bash
cargo clippy --features ssr
```

```bash
cargo check --lib --features hydrate --target wasm32-unknown-unknown
```

The server build uses the `ssr` feature and the browser build uses `hydrate`.
cargo-leptos picks the right one for each.

## Releases

Versions follow [Semantic Versioning](https://semver.org). While the app is
below 1.0, a minor bump (0.**x**.0) means new features or changes that need
attention (such as a new database migration), and a patch bump (0.x.**y**)
means fixes only.

Every change worth mentioning goes under `## [Unreleased]` in
[CHANGELOG.md](CHANGELOG.md) as it is made. To cut a release, start from a
clean working tree and run:

```bash
scripts/release.sh 0.2.0
```

The script:

1. moves the Unreleased notes under the new version and today's date;
2. sets the version in `Cargo.toml`;
3. runs the tests and a release build;
4. commits `Release v0.2.0` and creates an annotated `v0.2.0` tag whose
   message is that version's changelog entry.

It does not push. Publish with:

```bash
git push origin master --follow-tags
```

The running app shows its version at the bottom of the sidebar.

## Limitations

- There are no accounts or authentication. The server listens on localhost
  only, so keep it that way.
- There is one shared conversation for every browser tab.
- Attachments are pasted whole into the prompt. Files larger than the context
  window (about 30 KB of text at the default 8192 tokens) get cut short by
  Ollama.
- The assistant only sees search result snippets, not whole pages. Web text is
  untrusted: the model is told to treat it as information, never as
  instructions, but a small model can still be misled by it.
- SearXNG's Wikipedia, Wikidata and OpenStreetMap engines stay off until you
  set `general.contact_url` in `searxng/settings.yml`, because Wikimedia asks
  automated clients for a way to contact them.
