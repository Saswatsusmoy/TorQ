# Profiling fixtures

Real payloads captured from the live sites on 2026-08-11 (query "inception"
unless noted), served back to the production parse code by the fixture
server in `examples/search_profile.rs`. Fixture **bytes are the real site
output**; nothing here is a synthetic toy.

| File | Source | Real? | Contents |
|---|---|---|---|
| `tpb_search.json` | apibay.org `/q.php?q=inception` | yes | 100 rows |
| `tpb_browse.json` | apibay.org `/precompiled/data_top100_207.json` | yes | top-100 movies |
| `tpb_tv_browse.json` | apibay.org `/precompiled/data_top100_208.json` | yes | top-100 TV |
| `eztv_browse.json` | eztvx.to `/api/get-torrents?limit=100&page=1` | yes | 100 rows |
| `bittorrented_search.json` | bittorrented.com search API | yes | 50 rows |
| `fitgirl_search.xml` | fitgirl-repacks.site `?s=inception&feed=rss2` | yes | 5 items |
| `fitgirl_feed.xml` | fitgirl-repacks.site `/feed/` | yes | 10 items |
| `nyaa_inception.xml` | nyaa.si search RSS | yes | 5 items |
| `nyaa_search.xml` | nyaa.si search RSS (query "dandadan") | yes | 75 items |
| `nyaa_browse.xml` | nyaa.si browse RSS | yes | 75 items |
| `subsplease_search.json` | subsplease.org search API (query "dandadan") | yes | 24 releases |
| `subsplease_latest.json` | subsplease.org latest API | yes | — |
| `yts_big.json` | yts.am list API (query "love", limit 50) | yes | 50 movies |
| `yts_search.json` | yts.am list API (query "inception") | yes | 1 movie |
| `x1337_list.html` | **synthesized** | no | 40-row search page |
| `x1337_detail_{0..3}.html` | **synthesized** | no | 4 detail pages |

## Why 1337x is synthesized

Every 1337x host (1337x.to, 1337x.st, x1337x.ws, 1337xx.to) answers
Cloudflare's JS challenge from this network — a 403 challenge page, no
torrent rows. `x1337_list.html` reproduces the real 1337x markup the parser
targets (`table.table-list > tr`, `td.coll-1.name > a[href^='/torrent/']`,
`td.coll-2.seeds`, `td.coll-3.leeches`, `td.coll-4.size`) at realistic scale
(40 rows, real-looking names/sizes/seeder counts), and the detail pages carry
a magnet link in the shape the parser extracts. The **parse cost** is what is
measured, and the markup is the exact structure the selectors match; the site
bodies themselves were unobtainable from this network.

## yts / subsplease

Both hardcode their hosts in code, so the harness cannot redirect them to the
fixture server; they are profiled in `live` mode instead. Their fixtures are
kept here for reference and for the live-mode payload comparison.
