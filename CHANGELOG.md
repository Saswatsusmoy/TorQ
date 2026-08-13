# Changelog

All notable changes follow [Keep a Changelog](https://keepachangelog.com/) and
semantic versioning.

## [0.1.10] — 2026-08-13

### Fixed

- **Players can actually stream now**: `/stream` was behind the bearer-token
  middleware, but VLC/IINA/mpv can't send HTTP headers — the first real
  player attempt got `401 authentication failed without realm`. `/play` now
  mints a short-lived capability token (1h, swept on mint) and embeds it in
  the stream URL (`?token=…`); the stream route accepts it in place of the
  header. The rest of the API stays header-only, so the auth boundary is
  unchanged — the URL itself is the ticket.

## [0.1.9] — 2026-08-13

### Added

- **One-key play now (Stremio-style)**: `P` on a result that isn't queued
  adds the torrent, waits for metadata (DHT/swarm for magnets, instant for
  `.torrent` files), and opens the player the moment the stream URL is
  live — no manual download step, no full-download wait. On queued items it
  just plays. The inspector and detail view advertise it; the help card
  documents it.
- **`torq play` / `torq stream` accept any source**: a torrent id, a magnet
  link, a 40-char infohash, or a `.torrent` file path. Non-ids are added
  first and streamed once playable, sharing the same add→wait→resolve
  helpers (`torq_core::rest`) as the TUI.

### Fixed

- Bearer auth double-prefix in the new REST helpers (`Bearer Bearer …`
  401) — the shared helpers take the raw token now.

## [0.1.8] — 2026-08-13

### Added

- **Real video player for streaming**: `P` and `torq play` no longer hand
  the stream URL to the browser. A new `torq_core::player` module resolves
  the best installed player — VLC > IINA > mpv > ffplay (macOS app bundles
  and PATH binaries) — falling back to QuickTime Player on macOS and the
  system opener elsewhere. The TUI shows a `▶ playing in <player>` notice.
- **`player` config key**: `~/.config/torq/config.toml` → `player = "iina"`
  forces a player by name; a path is used verbatim; `"browser"` restores
  the old behavior. Both the TUI and `torq play` honor it.

## [0.1.7] — 2026-08-13

### Fixed

- **Streaming reachable from search**: `P` (play), `p` (pause/resume), and
  `x`/`D` (remove) now work on a result that is already downloading, from
  the search list and the detail view — previously they only acted in the
  Downloads/Seeding sections, so playing a torrent you'd just queued forced
  a section switch. The inspector's `P Play` hint is no longer dead. All
  action keys route through one shared helper.
- Render-test fixtures no longer embed wall-clock-relative timestamps
  (`Added` rows used `relative_time`), which made the pixel tests flake
  across hour/day boundaries.

## [0.1.6] — 2026-08-12

### UI — full layout revamp (theme unchanged)

- **Three-pane layout** on terminals ≥120 cols: sidebar + results/list +
  a persistent right-hand **Details** inspector. The full-screen detail
  swap is gone — selection is inspection, and `Enter` (or `→`) focuses the
  inspector; `esc` returns. Narrower terminals keep the single-pane view.
- **Activity strip**: one always-on row above the key hints with aggregate
  ↓/↑ rates, active count, queue depth (n/cap), and seeding count — no
  section switch needed to see that something is moving.
- **Live sidebar badges** on every section: per-category result counts once
  a search lands, active/seeding counts for the torrent lists (accented).
- **Progress column** in search results (wide terminals): a result already
  in the queue shows its live percent (or `seed`) next to Size.
- `tab` now cycles Sidebar → List → Inspector; `←→`/`hl` move one pane.
  Inspector and list share the action keys (d/p/x/D/r/P).

### Added

- `GET /config` reports `max_active` (concurrent transfer slots) — backs
  the strip's queue depth; the TUI refreshes it with every snapshot.

## [0.1.5] — 2026-08-11

### Performance

- Parallel host failover: `fetch_with_failover` probes every mirror at once
  (remembered-good host first) and takes the first success. Sequential
  probing made three dead mirrors cost ~8s each (~30s worst case for 1337x
  behind a Cloudflare flap); the shared HTTP client also gets a 5s connect
  timeout. Measured against torlink on the same query and window: full
  10-source search median 2.8s vs 7.6s (~2.7x), x1337 ~3-5s vs 7.5s.

### Added

- `GET /torrents/{id}/play` resolves the playable stream URL (largest video
  file, fallback largest file) — one implementation shared by `torq play`
  and the TUI.
- TUI `P` key in Downloads/Seeding opens the selected torrent in the OS
  player; footer always reserves the `?` help hint from truncation.

## [0.1.4] — 2026-08-11

### Changed

- TUI redesigned with a pane-based layout, re-themed on the everforest
  palette (sainnhe/everforest dark): green accent family — selection/pointers
  green, success/checks aqua, errors red, warnings yellow; wordmark in a
  cream→green gradient. Layout: sidebar rail (All/Games/Movies/TV/Anime +
  Downloads/Seeding with counts), rounded panels (`╭─ Title (n) ─╮`),
  a bordered results table (`│` column dividers; `Seeds`/`Lch`/`Size`/`Src`
  columns), animated progress bars with sheen, per-source tags, results
  detail view, sortable columns, contextual footer hints, two-column help
  card, and a centered splash with an editable search bar. Layout and colors
  are verified by buffer-exact render tests.

## [0.1.3] — 2026-08-11

### Performance

- Search wall time cut ~4x (21s → ~5s on live sources): 1337x now fetches its
  top detail pages in parallel instead of sequentially — that round-trip chain
  was the dominant cost of every search. Result order and the 4-row cap are
  unchanged.
- Host failover remembers the last working host per source (5-minute TTL), so
  dead mirrors (e.g. yts.mx) are skipped instead of probed on every request.
- Dedupe keeps the max-seeders row by swap instead of cloning the whole row;
  the JSON sources map borrowed arrays instead of cloning the serde_json
  value tree; four per-byte-allocation URL-encoder copies are one shared
  preallocated implementation.
- New profiling harness (`examples/search_profile.rs`): deterministic fixture
  replay of the real search pipeline (captured live payloads), plus
  live/dhat/sample modes.

## [0.1.2] — 2026-08-11

### Fixed

- `torq update` wrote the release `.tar.gz` archive over the binary instead of
  extracting it, producing a corrupt executable (exec format error). The
  `torq` entry is now extracted before the atomic swap.

## [0.1.1] — 2026-08-11

### Changed

- Rust edition 2021 → 2024 (MSRV 1.85); nested `if let`s collapsed to
  let-chains. No behavioral change.
- librqbit switched to its `rust-tls` backend — the binary no longer links
  openssl and Linux cross-builds work.

### Added

- `torq add <magnet|infohash|file.torrent>` CLI verb; `POST /torrents`
  accepts `torrent_b64` (base64 `.torrent` bytes) in addition to magnets.
- Distribution: crates.io publish (`torqtui` — the `torq` name is taken),
  GitHub Releases with per-platform binaries + update manifest, Homebrew tap
  (`saswatsusmoy/torq`), `install.sh`.

## [0.1.0] — 2026-08-11

### Added

- Daemon core: librqbit engine session (DHT, trackers, uTP/TCP, persistence,
  SOCKS5, rate limits), download queue (active-slot cap,
  auto-promotion, paused/queued/completed/failed statuses), `queue.json`
  persistence.
- REST API on 127.0.0.1 (bearer token auth): `/health`, `/torrents` (list/add/
  pause/resume/delete), `/events` (SSE).
- Watch folders: dropped `.torrent` / magnet files start downloading.
- `torq daemon` and `torq status` CLI verbs; `config.toml` with auto-generated
  auth token.
- Search: `GET /search` aggregates all 10 curated sources (FitGirl, YTS, TPB
  ×2, 1337x ×2, EZTV, Nyaa, SubsPlease, BitTorrented), dedupes by infohash
  (canonicalized across hex/base32), reports offline sources without failing.
- Declarative source engine: JSON-API and RSS/Atom runners shared by the
  built-ins and by user plugins (`~/.config/torq/plugins/*.toml`) — adding a
  site is config, not code.
- `torq search <query>` CLI verb; 8s per-host failover so dead mirrors cost
  seconds, not minutes.
- RSS subscriptions: `GET/POST/DELETE /rss` + `torq rss` verbs — feed URL,
  title-regex and size-window filters, jittered polling, auto-download of
  matches with per-subscription dedupe (works on Nyaa-style namespaced feeds
  via generic infohash extension fallback).
- Streaming: `GET /torrents/{id}/stream/{file}` with HTTP ranges over
  librqbit's on-demand FileStream (playback starts mid-download); `torq
  play` / `torq stream`.
- Resource controls: live rate limits (`PATCH /config/limits`, `torq
  limits`), time-of-day bandwidth schedule in config, SOCKS5 proxy (peers +
  sources) via config.
- Cross-seed: library index of existing `.torrent` dirs (`torq library`),
  re-adding a matching hash points the engine at the existing data.
- Bound magnet metadata resolution at 30s so seedless magnets error instead
  of hanging the API.
- Packaging: release binary, `torq daemon --install` (launchd / systemd),
  `torq update` manifest-based self-update, README.
