# torq

A torrent finder and downloader in one ~11MB binary: search a curated set of
sources from your terminal, add with one keypress, and let a daemon download
and seed while you do other things. Built in Rust on [librqbit](https://github.com/ikatson/rqbit).

```
┌ torq ──────────────────────────────────────────────────────────────┐
│ torq  search  downloads   http://127.0.0.1:8170                   │
│ inception                                                          │
│ type to search, Enter to run (empty = browse latest), d = download │
│ name                    size     seeds source                     │
│ Inception (2010) 1080p BrRip x264 - 1.85GB - YIFY   1.9 GB  922   │
│ Inception 2010 1080p BluRay HEVC x265 5.1 BONE      2.0 GB  377   │
│ Inception.2010.1080p.BluRay.DDP5.1.x265.10bit       2.9 GB  102   │
│ …                                                                │
└───────────────────────────────────────────────────────────────────┘
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

In the TUI: type a query, Enter to search (empty query = browse latest), `d`
to add; `1`/`2` switch between search and downloads; `p` pause/resume, `x`
remove, `?` help, `q` quits the TUI — **the daemon keeps downloading**.
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

See `docs/PLAN.md` for the full design and `docs/SPIKE.md` for the librqbit
capability report.

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
