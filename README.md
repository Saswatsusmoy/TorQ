# torq

A torrent finder and downloader in one ~11MB binary: search a curated set of
sources from your terminal, add with one keypress, and let a daemon download
and seed while you do other things. Built in Rust on [librqbit](https://github.com/ikatson/rqbit).

```
          𐓏
 ▀█▀ █▀█ █▀█ █▀█    torq @ http://127.0.0.1:8170
  █  █▄█ █▀▄ █▄█▀
 ───────────────────────────────────────────────────────────────────────────
 ▌ All (2)     ╭─ Search ────────────────────────╮ ╭─ Details ─────────────╮
   Games       │ ❯ Search or paste a magnet…     │ │ Inception (2010)  YTS │
   Movies (1)  ╰─────────────────────────────────╯ │ Size     1.90 GB      │
   TV (1)                                          │ Health   922 seeders… │
   Anime                                          │ Status   downloading  │
   Downloads (1) ╭─ Results (2) ────────────────╮ │           ████████░░  │
   Seeding      │ Name                    Size %│ │ Rates    ↓1.2 MB/s…  │
                │ ❯ Inception 2010   1.90 GB 58%│ │ P Play p Pause x …   │
                ╰───────────────────────────────╯ ╰───────────────────────╯
 ↓ 1.2 MB/s ↑ 300 KB/s · 1 active · 0/3 queue
 ↑↓←→ Move  ↵ Inspect  d Download  s Sort  / Search  tab Switch  ? Keys
```

## Quickstart

One command, no build, no runtime dependencies:

```sh
# macOS or Linux — installs ~/.local/bin/torq
curl -fsSL https://raw.githubusercontent.com/Saswatsusmoy/TorQ/main/install.sh | sh

# or with Homebrew (macOS)
brew install saswatsusmoy/torq/torq

# or from source / crates.io
cargo install torqtui
```

Then:

```sh
torq tui      # starts the daemon automatically, opens the TUI
```

In the TUI: type a query and Enter to search (empty query = browse latest),
or `/` to edit a running search; `d` adds the selected result, `P` opens it
in your player. Terminals ≥120 cols get a three-pane layout — sidebar,
results list, and a right-hand **Details** inspector that always shows the
selected item (live transfer state, files, actions); `tab`/`←→` cycle the
panes, and a one-row activity strip keeps aggregate rates and queue depth on
screen from every section. The sidebar filters results by category and lists
Downloads/Seeding with live counts: `p` pause/resume, `x` remove, `D` remove
and delete files, `s` sort, `?` help, `q` quits the TUI — **the daemon keeps
downloading**.
`torq status` and `torq search <query>` work from any shell, no TUI needed.

Already have the repo checked out? `cargo build --release` gives you
`target/release/torq` directly.

## Architecture

One long-lived daemon owns the torrent engine and a REST API on
127.0.0.1 (bearer token in `~/.config/torq/config.toml`); the TUI, CLI, and
scripts are stateless clients. Reattach is just reconnect.

```
torq tui ─┐
torq add  ├─ REST + SSE (127.0.0.1:8170, token auth) ─> torq daemon
scripts ──┘                                              └─ librqbit session (DHT/trackers/uTP/TCP)
```

## Commands

| Command | What it does |
|---|---|
| `torq tui` | Terminal UI; auto-starts the daemon if needed |
| `torq daemon` | Run the engine + API in the foreground |
| `torq daemon --install` | Install as a login service (launchd / systemd user unit) |
| `torq search <query>` | Aggregate search across all sources, deduped |
| `torq add <magnet\|infohash\|file.torrent>` | Add a download to the daemon |
| `torq status` | Downloads with progress/speed |
| `torq stream <id>` / `torq play <id>` | Print / open the stream URL (works mid-download) |
| `torq rss add <url> [--title-re …] [--min-size …] [--interval …]` | Subscribe; matches auto-download |
| `torq rss list` / `torq rss remove <id>` | Manage subscriptions |
| `torq library scan` / `torq library status` | Cross-seed index of existing `.torrent`s |
| `torq limits --upload N --download N` | Live rate limits (bytes/sec) |
| `torq update [--check]` | Manifest-based self-update (`TORQ_UPDATE_URL`) |

## Sources and plugins

Ten curated sources (FitGirl, YTS, TPB ×2, 1337x ×2, EZTV, Nyaa, SubsPlease,
BitTorrented) run through two declarative runners — a JSON-API runner and an
RSS/Atom runner — plus bespoke adapters where the shape genuinely differs.
Results are deduped by infohash (hex and base32 normalized) and sources that
fail are reported as offline, never fatal.

Add your own site with a TOML plugin in `~/.config/torq/plugins/`:

```toml
kind = "rss"
id = "mysite"
label = "MySite"
homepage = "https://mysite.example"
hosts = ["https://mysite.example"]
path = "/feed"
```

## Config

`~/.config/torq/config.toml` (macOS: `~/Library/Application Support/torq/`):

```toml
download_dir = "/Users/me/Downloads"
socks_proxy = "socks5://127.0.0.1:1080"   # peers + sources
upload_bps = 0                             # 0 = unlimited
download_bps = 0
watch_dirs = ["/Users/me/inbox"]           # drop .torrent/magnet files here
library_dirs = ["/mnt/library"]            # cross-seed from existing data
schedule = []                              # e.g. [{start="23:00", end="07:00", download_bps=10485760}]
```

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CHANGELOG.md](CHANGELOG.md).
