# Changelog

All notable changes follow [Keep a Changelog](https://keepachangelog.com/) and
semantic versioning. Unreleased work lives under [Unreleased].

## [Unreleased]

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
