# Changelog

All notable changes follow [Keep a Changelog](https://keepachangelog.com/) and
semantic versioning.

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
  SOCKS5, rate limits), torlink-model download queue (active-slot cap,
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
