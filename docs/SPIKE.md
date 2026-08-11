# TorQ spike report

Two questions, answered before any code:

1. Does librqbit give us everything the plan needs (streaming, limits, proxy,
   persistence, file selection, storage)?
2. Are torlink's 8 sources portable and alive?

## 1. librqbit capability report

Verified against source: `github.com/ikatson/rqbit` at `4e5f94cb` (2026-07-22,
workspace version 9.0.0-rc.0). crates.io stable is **8.1.1** — we pin that and
re-verify the APIs compile during daemon-core (minor API drift possible between
8.1.1 and 9.0.0-rc).

| Capability | Verdict | API (verified in source) |
|---|---|---|
| Sequential/streaming download | ✅ | `torrent_state/streaming.rs`: `FileStream` implements `AsyncRead` + `AsyncSeek`; `PER_STREAM_BUF_DEFAULT = 32MB` lookahead. HTTP handler `h_torrent_stream_file` (http_api/handlers/streaming.rs) serves ranges (`Accept-Ranges: bytes`), sets MIME + DLNA headers. |
| Per-torrent rate limits | ✅ | `AddTorrentOptions.ratelimits: LimitsConfig { upload_bps, download_bps }` (session.rs:278). Session-level: `SessionOptions.ratelimits` → `Session.ratelimits: Limits` with `set_upload_bps`/`set_download_bps` (limits.rs). Governor token buckets. |
| SOCKS5 proxy (peers) | ✅ | `ConnectionOptions { proxy_url: Option<String>, enable_tcp, peer_opts }` (stream_connect.rs:38) — "all outgoing connections will go through the proxy over TCP"; parsed via `SocksProxyConfig::parse`. HTTP traffic proxied via reqwest `Proxy::all` (session.rs:706-710). Env var `RQBIT_SOCKS_URL`. |
| Session persistence | ✅ | `SessionPersistenceConfig::Json { folder }` (session.rs:379); auto-saves periodically + on change; restores torrents incl. `only_files`, paused state, output folder (session_persistence/json.rs). Have-bitfields via `BitVFactory` (DiskBackedBitV). DHT persistence separate (`DhtPersistenceConfig`). |
| File selection | ✅ | Add-time: `AddTorrentOptions.only_files: Option<Vec<usize>>` / `only_files_regex` (session.rs:249-252). Runtime: `Session::update_only_files(&handle, &HashSet<usize>)` (api.rs:344, torrent_state/mod.rs:617). |
| Storage | ✅ | Sync, zero-copy by design (storage/mod.rs): `pwritev` positioned vectored writes, reads straight into socket buffers, no tokio-fs double buffering. `MmapFilesystemStorageFactory` behind `storage_examples` feature. |
| DHT / trackers / uTP / TCP / PEX / WebSeed / LSD | ✅ | `SessionOptions.dht: Option<DhtSessionConfig>` (default on, persistent), `listen: Option<ListenerOptions>`, `connect`, `disable_local_service_discovery`, `trackers: HashSet<Url>` (per-session always-on trackers). |
| Peer caps + blocklist | ✅ | `Session.peer_limit: Option<usize>` (global), `AddTorrentOptions.peer_limit` (per-torrent), `IpRanges` blocklist/allowlist (URL-loadable). |
| Queue/state API | ✅ | `ManagedTorrent`: `id`, `name`, `info_hash`, `output_folder`, `only_files`, `is_paused`, `stats() -> TorrentStats`, `live()`, `with_metadata`, `wait_until_initialized/completed`. `Api` facade (api.rs) shows serializable DTO patterns to mirror. |

**Implication**: nothing blocked. Streaming, limits, proxy, persistence, file
selection, and fast storage are all first-class. Our daemon is a thin layer on
`Session` + `ManagedTorrentHandle` — we own queue semantics, the REST API, RSS,
plugins, and the library index.

## 2. Source feasibility

torlink's 10 adapters analyzed; structure captured for the Rust ports.

| Source | Type | Endpoint (verified) | Status |
|---|---|---|---|
| YTS | JSON API | `https://yts.mx/api/v2/list_movies.json?query_term=...&limit=50`; hosts yts.mx / yts.am / yts.rs | ⚠️ DNS-blocked here (yts.mx → sinkhole 49.44.79.236; yts.rs → Cloudflare 172.67.149.18, HTTP 500). Port from torlink's TS. |
| TPB (Movies/TV) | JSON API | `https://apibay.org/q.php?q=...` + `precompiled/data_top100_207|208.json`; cats 201/202/207/209 (movies), 205/208 (TV) | ✅ live (q.php returns YIFY results with real seeders) |
| 1337x (Movies/TV) | HTML scrape | `/category-search/<q>/<Cat>/1/` or `/popular-<cat>`; hosts 1337x.to / .st / x1337x.ws / 1337xx.to; rows in `table-list`, `coll-2 seeds`, `coll-3 leeches`, `coll-4 size`, detail page for magnet | ⚠️ all hosts DNS-blocked here. Selectors captured from x1337.ts; port with multi-host failover. |
| EZTV | JSON API | `https://eztvx.to/api/get-torrents?limit=100&page=1` | ⚠️ DNS-blocked here (sinkhole). Single-host in torlink; same outage affects torlink. |
| Nyaa | RSS | `https://nyaa.si/?page=rss&q=...&c=0_0&f=0`; `nyaa:infoHash`, `nyaa:seeders`, `nyaa:size` | ✅ live (75 items for a test query) |
| SubsPlease | JSON API | `https://subsplease.org/api/?f=latest|search&s=...&tz=UTC`; res preference 1080 > 720 > 480 | ✅ live (magnet in `downloads[].magnet`) |
| BitTorrented | JSON API | `https://bittorrented.com/api/search/torrents?q=...&type=video&limit=50&sortBy=seeders`; min query 3 chars | ✅ live |
| FitGirl | WordPress RSS | `https://fitgirl-repacks.site/?s=<q>&feed=rss2` or `/feed/`; `href="magnet:..."`, `pubDate` | ✅ live (10 items) |

**Live check method**: `curl -A <browser UA>` against real endpoints, 2026-08-11,
from the dev machine. 5/8 verified end-to-end; the 3 blocked domains all resolve to
the same sinkhole IP (49.44.79.236) — network-level block, not schema drift. The
JSON/RSS APIs (6/8 sources) port to reqwest+serde trivially; 1337x needs the
`scraper` crate for `table-list` rows + detail-page magnet regex.

**Implication**: port all 10 adapters; keep torlink's multi-host failover + per-source
health reporting ("source X offline" in search results). A down source must never
fail a search — same degradation model as torlink.

## Decisions locked by this spike

1. Pin `librqbit = "8.1.1"`; verify compile against our usage in daemon-core.
2. Sequential streaming = librqbit's `FileStream` + our axum range handler (torlink's
   `files` mode becomes `/stream/<id>/<file>` on the daemon).
3. Proxy = `ConnectionOptions.proxy_url` (peers) + reqwest proxy (HTTP). Both SOCKS5.
4. Persistence = `SessionPersistenceConfig::Json` in state dir; our queue/config in
   `config.toml`; library index in SQLite.
5. Source health must be surfaced in search results (torlink parity).

## 8.1.1 delta (learned during daemon-core)

The cloned HEAD is 9.0.0-rc.0; crates.io stable is 8.1.1. Verified against the
`v8.1.1` tag:

- **SessionOptions (8.1.1)**: `disable_dht`, `disable_dht_persistence`,
  `dht_config: Option<PersistentDhtConfig { dump_interval, config_filename }>`,
  `fastresume`, `persistence`, `peer_id`, `peer_opts`, `listen_port_range:
  Option<Range<u16>>`, `enable_upnp_port_forwarding`, `defer_writes_up_to` (MB of
  buffered deferred writes), `default_storage_factory`, `socks_proxy_url:
  Option<String>` (peer SOCKS5 lives here, not in a `ConnectionOptions`),
  `cancellation_token`, `concurrent_init_limit`, `root_span`, `ratelimits:
  LimitsConfig`, `blocklist_url`, `trackers: HashSet<Url>`.
- **No per-torrent/session peer caps in 8.1.1** (`peer_limit` is 9.x). Connection
  limiting returns in the Controls phase via `defer_writes_up_to` (memory) +
  ratelimits; revisit librqbit 9.x for peer caps.
- **`ManagedTorrentHandle` is unnameable outside librqbit** (private
  `torrent_state` module). Use `AddTorrentResponse` (public, pattern-matchable) and
  `librqbit::api::{Api, TorrentIdOrHash, TorrentListResponse, TorrentStats}`.
- **`Speed` has a public `mbps: f64` field**, not an `mbps()` method.
- **librqbit refuses `pause` while a torrent is initializing** — the daemon records
  intent in meta and the 1s reconcile tick enforces it (retry until it takes).
- **`AddTorrentOptions.overwrite` defaults to false**, which makes re-adding a
  torrent with existing files fail instead of resuming. Daemon defaults it to true
  (resume is core downloader behavior; the piece check validates on-disk data).
- **`api_torrent_list_ext(with_stats: true)`** is the status source of truth:
  `TorrentDetailsResponse { id, info_hash, name, output_folder, files, stats }`,
  `TorrentStats { state, error, progress_bytes, uploaded_bytes, total_bytes,
  finished, live }`, `LiveStats { download_speed, upload_speed, snapshot }`,
  `Snapshot.peer_stats.live` = connected peers.
- **Known librqbit wart**: `stats()`/list under the torrent state lock can block
  long-running sync work; our daemon's `views()` must never hold our meta lock
  while calling it (we deadlocked on that — fixed by computing queue decisions
  before taking the meta guard).
- **TLS backend**: librqbit's default features pull `reqwest/default-tls`
  (native-tls → openssl-sys), which breaks Linux cross-compilation. Use
  `default-features = false, features = ["rust-tls"]` (rustls + ring) — also
  drop the `librqbit-core` default `sha1-crypto-hash` in favor of
  `sha1-ring` so the two don't conflict.

