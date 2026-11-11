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

1. **Search (Discover → Anna's Archive).** The mirror's own search page is
   scraped for title / author / format / MD5 / AACID. Anna's Archive puts
   anonymous visitors behind a DDoS-Guard challenge, so duet searches with
   your solved session:
   - Direct HTTP first, using cookies harvested from your browser (see below)
     or a member secret key if set (Settings → Free sources; your account
     page → Settings → Free sources — same key the fast-download API uses).
   - If the check blocks direct HTTP, the driven visible browser below
     retries with its native session.
   - If the manual checkbox is up, duet shows the browser in a modal. Solve
     it there, then retry — the solve lives in that same browser.
2. **Download per record:** LibGen MD5 bridge (`libgen.li/ads.php?md5=…`,
   no session) → keyless shard torrent (AACID → shard URL derived
   arithmetically, added paused, single file selected, resumed) →
   keyed fast-download (`/dyn/api/fast_download.json`) → friendly failure
   naming the bot check. The failure reason is stored on the download row.
3. Magnets: if you already have a magnet for a big torrent, add it in qBittorrent, let its
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
| Anna's Archive | ✅ search + LibGen bridge / shard torrent / fast download | above |
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

## Remote browser (one Firefox, driven in the open)

Anna's Archive challenges server-side searches (DDoS-Guard, currently with
a manual checkbox). A headless solver can't pass it — and neither can
cookies harvested for server-side requests (proven: a fresh solved session
still gets `?check=1` over plain HTTP, and a headless driven browser stays
on the check page for the full 45s wait). So duet drives the same visible
Firefox you see:

- **One browser:** visible Firefox in the `aa-browser` container, opened
  straight at the mirror, with Marionette on `127.0.0.1:2828`. geckodriver
  (same `pia-vpn` netns, published as `:4444`) drives it; you watch every
  search happen in the viewer. A completed manual check clears in that
  same browser, so its session is native — nothing to harvest or plant.
- **Direct HTTP first** (member key or pasted/harvested cookies when set),
  then the driven visible browser. duet ensures driven mode itself before
  every search (relaunching with Marionette if you left it clean for a
  login), so clean mode is only ever a temporary excursion.

Flow:

1. Search in Discover as normal. Direct HTTP with a member key when set;
   else the driven visible browser below.
2. If the manual check is up, duet shows that same browser in a modal
   (`viewer_url`, default built from duet's host, port 3001). Solve the
   checkbox there, then press **I've solved it — retry search**.
3. Downloads use the LibGen bridge → shard torrent chain above, so the
   human click is the only manual step.

Setup (home-services repo):

```bash
cd ~/src/home-services
docker compose up -d aa-browser geckodriver
```

Settings → Free sources: WebDriver address (default
`http://127.0.0.1:4444`), browser screen address (port 3001), Browser
profile folder (`/mnt/stuff/browser-profile`). One search at a time goes
through the driven browser — serialized in `selenium.rs`, deliberately
gentle (at most 1 concurrent connection per host). Phones and tablets reach
duet and the viewer over LAN HTTPS (`https://<duckdns-name>` for duet,
`:4443` for the viewer, via the `tls-proxy` service which reuses
screentime's Let's Encrypt cert) — plain HTTP would trip the viewer's
secure-context requirement. Set Settings → Free sources → Browser screen
address to the full https viewer URL when using it. Every WebDriver session
is deleted in the same command that created it: geckodriver allows exactly
one, and an orphaned session bricks all later searches until the
containers restart. All site traffic goes through the VPN proxy
(`http://localhost:8888`, Settings configurable); qBittorrent is at
`:8080`.

Notes:

- Firefox autoconfig `navigator.webdriver` patching was proven inert in
  this build (no marker ever executed) — do not rely on it. The split
  browsers are the fix: the human side never shows automation.
- Keep automated traffic off banned sites while a ban is active — even the
  proxy IP can get banned by hammering. The browser is for hands, not bots:
  one page at a time.
- Fresh VPN IP: Settings → VPN → **Get new VPN IP** (configure the VPN
  container, compose folder, and sidecars there first — every setup names
  them differently). duet restarts the tunnel, waits for it (polling the
  exit IP through the proxy), then recreates the sidecars on the new
  network namespace — otherwise they sit running with no network and the
  driven-browser state goes stale. Takes a couple of minutes; the button
  reports old → new IP. Manual equivalent: `docker restart <vpn>`, then
  `docker compose up -d` the sidecars in the compose folder. Verify with
  the proxy: `curl -x http://127.0.0.1:8888 https://api.ipify.org`.
- AA cookies expire quickly (~20 min). A solve must be followed promptly
  by search/download, otherwise the modal flow repeats.

## Legal note

Both sources index material that may be copyrighted. duet fetches what you ask for, the way a
browser would; check what you're allowed to access in your jurisdiction and use a VPN when
seeding. The Anna's Archive fast-download path requires a membership donation.
