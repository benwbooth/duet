# Downloading from Anna's Archive and Audiobook Bay

## Anna's Archive torrents: shards, not the tar

Verified against the live `/torrents/` listing and the actual `.torrent`
files (the `small_file` URLs download anonymously):

- **Per-file shards** (`annas_archive_data__aacid__<collection>__<ts>--<next>.torrent`,
  download path `/dyn/small_file/torrents/managed_by_aa/annas_archive_data__aacid/…`):
  multi-file torrents (tens of thousands of entries), each file a bare
  `aacid__…` id. The AACID's collection + timestamp name the exact shard,
  so duet derives the shard URL arithmetically — no catalog needed — adds
  it to qBittorrent, and selects only that one file. Search cards carry the
  shard link too when present, and it wins over the derived URL.
- **Range dumps** (`pilimi-zlib2-…​.torrent` and friends): single-file
  multi-hundred-GB `.tar` archives. qBittorrent can't partially fetch one
  file out of those (no per-piece selection API), so duet never touches
  them — they exist for whole-collection seeding, not single books.
- **Same MD5, LibGen's mirror**: Anna's Archive and LibGen key files by
  MD5, so duet tries `libgen.li/ads.php?md5=…` first for every record —
  no session, key, or browser when LibGen has the file.

## Anna's Archive: why there is no single giant torrent to search

Anna's Archive's "ultimate unified list" of preservation torrents (https://annas-archive.org/torrents)
is split into **hundreds of torrents**, not one — e.g. `zlib` alone is 277 torrents (54 TB),
`ia` 182 torrents (136 TB). They overlap (cumulative) and exist for mirroring the whole
library, not for fetching one book. The site says so itself: "These torrents are not meant
for downloading individual books."

Two consequences:

* **File names inside most bulk torrents are hashes, not titles.** Searching a .torrent's
  file list for "Pride and Prejudice" almost never matches. To find a book by title you need
  the *metadata* (about 21 GB of `zlib3_records` JSONL, or the site's own search), not the
  torrent contents.
* **qBittorrent can't search a torrent it hasn't met.** Selective download works (add paused,
  set every file to "do not download" except the one you want, resume), but you must already
  know which torrent and which file inside it.

So duet does it in the order that actually works:

1. **Search (Discover → Anna's Archive).** The mirror's own search page is scraped for title /
   author / format / MD5. The site puts anonymous visitors behind a DDoS-Guard challenge, so
   this needs your **member secret key** (your account page → Settings → Free sources; it is
   the same key the fast-download API uses). duet trades it for a session cookie in memory
   and never logs it. Results download straight through the member fast-download API as
   individual files (`/dyn/api/fast_download.json`). No multi-terabyte torrents involved.
2. Magnets: if you already have a magnet for a big torrent, add it in qBittorrent, let its
   metadata arrive (once), then use qBittorrent's own file list: uncheck everything except
   the file you want.

API summary:

| Route | What it does |
| --- | --- |
| `GET /api/search?q=...&source=annas_archive` | Title/author search, fast-download ready |
| `GET /api/search?q=...&source=libgen` | LibGen (incl. RandomBook mirrors) search |
| `GET /api/search?q=...&source=audiobookbay` | Audiobook search, magnet links |

## Open SLUM: what is covered and what isn't

| SLUM source | duet | How |
| --- | --- | --- |
| Anna's Archive | ✅ search + fast download | above |
| Library Genesis (libgen.li/bz/vg/…) | ✅ search + direct download | below |
| RandomBook (randombook.org, libgen.pw/.me/.xyz) | ✅ same source as above | LibGen's "Mirrors" column links RandomBook for every file; it's the same database |
| Z-Library | ⚠️ no scraper — every mirror is PROTECTED | its books are indexed by Anna's Archive search |
| Sci-Hub | ⚠️ no scraper — PROTECTED/down, and it's papers not ebooks | those papers are indexed by Anna's Archive search too |
| WeLib | ⚠️ PROTECTED | an Anna's Archive mirror; use the AA entry |
| FMHY | ❌ | a piracy directory/wiki, not a book library; nothing to search or download |
| Liber3, Memory of the World | ❌ | tiny community collections with no search API; not worth a scraper |

### Mirror failover (Open SLUM)

Domains rotate, get seized, or stay PROTECTED for hours. A background task
(`crates/server/src/mirrors.rs`) scrapes open-slum.org on startup and every
30 minutes, and duet tries your **Settings primary first, then each live
mirror** until one answers with results (`free::failover`). Your primary
stays authoritative — the SLUM list is only the fallback chain. A scrape
that doesn't parse (site redesign) keeps the previous lists rather than
wiping them. `GET /api/mirrors` shows the lists duet is actually trying
now, so you can check freshness while troubleshooting.

## Library Genesis (+ RandomBook)

`GET {mirror}/index.php?req=…&columns[]=t&columns[]=a&objects[]=f&topics[]=l&res=25&filesuns=all`
returns the `table#tablelibgen` rows. Each row's "Mirrors" cell carries the file's MD5
(`/ads.php?md5=` for LibGen, `randombook.org/book/…`, `libgen.pw/book/…`,
`libgen.me/book/…`, Anna's Archive — all the same file), so duet reads title, author,
size, extension, and MD5 straight from one page. Downloading follows the site's own hop:
fetch `/ads.php?md5=…`, find the `get.php?md5=…&key=…` link, download that. No login, no
captcha on the mirrors checked — if a mirror starts challenging, switch it in Settings →
Free sources → LibGen mirror (Open SLUM lists the live ones).

Every LibGen hit downloads straight through LibGen's own hop — no Anna's
Archive key, no bot challenges at any point in the chain.

## Audiobook Bay

There is no official API and the RSS feed doesn't carry magnets (they require a page view,
which rate-limits the IP). duet follows exactly what the site's own qBittorrent search plugin
does:

1. `GET {site}/?s=query` → listing entries (title, detail link).
2. For each hit (max 12, one at a time — deliberately gentle), fetch the detail page
   (`/abss/slug/`), find its `Info Hash:` (40 hex) and tracker table.
3. Build a magnet (`magnet:?xt=urn:btih:<hash>&dn=<title>&tr=...`, page trackers first, then a
   small public fallback set) and hand it to the normal torrent flow, so it downloads through
   your configured qBittorrent and auto-imports into the library.

If the default mirror (`https://audiobookbay.lu`) stops working, change it in
Settings → Free sources → Audiobook Bay site. Domains rotate; so do Anna's Archive mirrors —
check the current list before changing a mirror, because your Anna's Archive key is sent to
whichever mirror you pick.

Tip: a book's page on the Anna's Archive site ends in its MD5. Paste that page's URL into
LibGen search or into your own qBittorrent client to get the same file another way.

## Remote browser (solves the check when Discover needs a human)

Anna's Archive challenges server-side searches (DDoS-Guard, currently with
a manual checkbox no headless solver can click), so duet drives a real
Firefox through the `selenium` sidecar (same pattern as the avistaz
project: shares pia's network, so it browses from the VPN IP):

1. Search in Discover as normal. duet tries plain HTTP first (works with a
   member key or a fresh session), else drives the selenium Firefox to the
   search URL.
2. If the manual check is up, duet pops that Firefox's screen in a modal.
   Click the checkbox there, then press **I've solved it — retry search**.
   The same browser keeps its session, so the retry loads results with no
   further clicks — and later searches skip the modal until the session
   expires.
3. Downloads use the keyless shard torrent (AACID → shard URL derived
   arithmetically), so the human click is the only manual step.

Setup (home-services repo):

```bash
cd ~/src/home-services
docker compose up -d selenium
```

Settings → Free sources: Selenium WebDriver address
(`http://localhost:8191`-style default `http://localhost:4444`) and screen
address (default built from duet's own host, port 7900). One search at a
time goes through it — deliberately gentle.

Notes:

- The old `aa-browser` sidecar and cookie harvest remain as fallback but
  are no longer part of the flow; they'll be removed once selenium has
  proven itself.
- Keep automated traffic off banned sites while a ban is active — even the
  proxy IP can get banned by hammering. The browser is for hands, not bots:
  one page at a time.
- Fresh VPN IP: `docker compose restart pia-vpn` reconnects (usually a new
  exit IP), but it drops the shared network namespace — restart the
  dependents after (`qbittorrent-vpn slskd prowlarr vpn-proxy aa-browser
  selenium`), otherwise they sit running with no network.
  Verify with the proxy:
  `curl -x http://127.0.0.1:8888 https://api.ipify.org`.

## Legal note

Both sources index material that may be copyrighted. duet fetches what you ask for, the way a
browser would; check what you're allowed to access in your jurisdiction and use a VPN when
seeding. The Anna's Archive fast-download path requires a membership donation.
