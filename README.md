# xitter-dl

A local-first library for your X bookmarks. A companion userscript reads the
bookmarks X already sends your own browser, hands them to a desktop app over
loopback, and they land in a single SQLite file you can search offline in
milliseconds.

No API, no OAuth, no scraping service, no cloud, no account, no recurring cost.

---

## What it does

- **Captures your bookmarks** as X serves them to your own browser, including
  quoted posts and the preview images the timeline draws.
- **Searches them offline** with SQLite FTS5 — whole-word, prefix and phrase
  queries in milliseconds.
- **Files them into folders** you define, by drag and drop, with a description
  per folder that a JEV model uses to decide where each post belongs.
- **Stays current as you browse** — new captures appear in the window while it
  is open, and a refresh never collapses a scrolled feed.
- **Keeps everything on disk** in one library file, plus a media directory,
  that you can copy or back up at any time.

---

## Requirements

- Windows 10 or later.
- To build from source: the Rust toolchain (MSVC), Node 20+ and pnpm.

## Building

```powershell
cargo fetch
cd ui; pnpm install; cd ..

cd ui; pnpm build; cd ..
cargo build --release -p xitter-dl-app --features custom-protocol
.\target\release\xitter-dl.exe
```

The release build embeds the frontend. A **debug** build instead loads
`http://localhost:5273`, so it needs `pnpm tauri dev` running; on its own it
shows `ERR_CONNECTION_REFUSED`.

## Your library

One SQLite file holds everything:

| | |
|---|---|
| Default | `%APPDATA%\xitter-dl\library.sqlite` |
| Override | `XITTER_DL_DB` |

`bridge.json` sits beside it (pairing), and `media/` holds the cached previews
and avatars. Back up that folder and you have backed up the library.

The app and the command-line tool resolve the path the same way, so they always
open the same file.

---

## Setting up capture

1. Start the app. The left rail shows the connection state.
2. Install [`userscript/xitter-dl-capture.user.js`](https://raw.githubusercontent.com/fuselayer/local-x-bookmarks/main/userscript/xitter-dl-capture.user.js)
   in Tampermonkey or Violentmonkey. Install it from the raw URL rather than
   pasting the file: Tampermonkey reads `@updateURL` from the copy it already
   has, so a pasted script never updates itself.
3. Ask the app for a pairing code (**Connect browser**), open
   `https://x.com/i/history`, click the pill in the bottom-right corner and type
   the code.

The code is valid for two minutes. After pairing, capture survives restarts,
browser updates and app updates. You re-pair only if you revoke the browser,
reinstall the script, clear its storage, or add a second browser — **Add another
browser** issues a fresh code without disturbing the ones already paired.

Everything X sent while the bookmarks page was scrolled is stored locally; the
script requests nothing from X's timeline and holds no X credential.

### The pill

The pill appears on every x.com page so it can always tell you what it is doing;
capture itself only happens on the bookmarks routes (`/i/history`, and the older
`/i/bookmarks`). Its panel names the route it read and whether that route counts
as capturing, so a route that X has renamed is visible on screen instead of
looking like a broken script.

The Tampermonkey menu has switches for network hooks, thumbnails and debug
logging, so they remain reachable even if the page is misbehaving.

---

## Using the app

### The feed

Every bookmark, newest first, with the preview the X timeline draws and the post
text collapsed to five lines behind **Show more**. t.co links are drawn as the
link they stand for. Search filters the feed as you type.

Themes: **Default**, **Dim** and **Lights out**, or follow Windows.

### Folders

Folders are the tab strip under the page title: the open one is bold with an
accent bar, and long names scroll sideways instead of being cut off. Selecting
one filters the feed, and a parent includes everything in its children.

The right rail is the editor. Drag one folder onto another to nest it, onto the
top or bottom edge to reorder, onto the header to pull it back out. Double-click
to rename. Each folder is a name plus a sentence saying what belongs in it —
the sentence is what the classifier reads, so write it as the boundary between
that folder and its neighbours.

### Classification (optional)

**Settings → Classification** takes a provider, a model, a requests-per-minute
figure and an API key.

| Provider | Endpoint | Free option |
|---|---|---|
| BeatAPI | `api.beatapi.io/v1` | `jev-1.14-free` — free, reads images |
| TypeSafe | `api.typesafe.ai` | `jev-latest` |
| OpenCode Zen | `opencode.ai/zen` | `jev-1.13-free` |
| Custom | any host serving the same API | — |

The key goes to Windows Credential Manager. It is never written to
`settings.json`, never sent to the window, and cannot be read back after saving
— the field shows only `saved ••••1234`. Switching providers switches the key
with it: each provider keeps its own.

**Requests per minute** is the pace your account is allowed, and the app spaces
requests to stay under it. BeatAPI's free model allows one successful request a
minute; the preset fills that in. A run therefore takes about an hour and forty
minutes for 100 posts — the panel tells you the estimate before you start, and
the run is resumable, so stopping is never lossy. After a top-up, raise it to 10
for a ten-minute run of 100.

Press **File N posts** to start. It sends 25, 100, 250 or 500 posts (newest
first) unless you choose *all*, because a library-wide run is a bill. While it
runs the panel counts `filed / failed` as each post completes and shows the
provider's own message the moment something goes wrong; a rate limit pauses and
counts down, and **Stop** takes effect immediately.

Each post is sent as `state` with one `choice` question over your folders —
described by path and sentence, not by name alone — plus a `score` for how
central the post is to the folder it lands in. An image-only post is sent as an
image; a post with a picture and a paragraph is sent as text. Turn images off in
Settings for a model without vision.

**Unsorted** is created the first time you classify. It exists so the question
has a way out: without it every post must land somewhere, and a bare link has
nothing to be unsure about. It is an ordinary folder — rename it, nest it, empty
it.

Every verdict stores its confidence, score and model, together with a hash of
the folder set it was answered under. Change any folder's description and every
verdict goes stale, so the next run asks again with the new wording — which is
the point of writing the descriptions carefully. A post that failed has no
verdict and is retried next time. **Unsort everything** deletes all verdicts and
leaves the folders alone, for when you want to start over.

### Media

A bookmark shows the preview the X timeline draws — `?name=small`, 680 px wide,
about 47 kB — and nothing larger. No originals, no video, no audio: a bookmarks
feed holds videos of hundreds of megabytes, and pulling those down implicitly is
a bandwidth and disk decision you never made.

- **Where** — `media\<sha256>.<ext>` beside the library. Content-addressed, so
  the same picture on twenty bookmarks is one file.
- **Avatars** — stored the same way, one file per account. Accounts X marks as
  square (logos) are drawn slightly rounded rather than cropped into a circle.
- **Gaps** — a bookmark captured before thumbnails existed shows a "not archived
  yet" frame until the bookmarks page is scrolled again; that pass fills them in.

---

## Command line

`xdl` runs the whole pipeline without a window, against the same library:

```powershell
cargo run -p xitter-dl-cli -- import .\fixtures\tweet-simple.json
cargo run -p xitter-dl-cli -- search "localfirst"
cargo run -p xitter-dl-cli -- show 1843712994563928064
cargo run -p xitter-dl-cli -- stats
```

`import` takes a file, a directory, NDJSON, or a HAR. `--db <path>` points any
command at a different library file.

`xdl bridge` runs the pairing receiver in the foreground and prints the code as
it rotates — useful for diagnosing a browser that will not connect, without the
window.

## Importing without the script

The app's **Import** button takes a pasted payload:

1. Open `x.com/i/history`.
2. DevTools → Network → filter `graphql` → find a `Bookmarks` request.
3. Right-click → Copy → Copy response.
4. Paste into the Import box.

Nothing is sent anywhere; the payload is parsed and stored locally.

---

## Troubleshooting

**The pill is missing.** Check the browser's Tampermonkey dashboard that the
script is installed and enabled for `x.com`. The console prints
`[xitter-dl] <version> loaded` on every page; if that line is absent, the script
is not running.

**The pill is there but nothing arrives.** Its panel names the route it read and
whether that route captures. Capture works on `/i/history` and `/i/bookmarks`;
if X has moved the page again, that is what the panel will show.

**"Add an API key in Settings first."** The classifier needs a key for the
provider currently selected. Each provider keeps its own — switching the
dropdown does not carry a key across.

**HTTP 401 / invalid credential.** The saved key is not accepted by that
provider. Paste a fresh one; **Clear key** removes it.

**HTTP 429 / rate limited.** The account is being asked for more requests per
minute than it allows. Lower **Requests per minute** in Settings to what the
provider's plan states. A paused run counts down and continues on its own.

**"Stop" says it is waiting.** A request already on the wire cannot be recalled;
the run ends as soon as it returns.

**Thumbnails show a frame that says "not archived yet".** Scroll the bookmarks
page again to backfill previews for older captures.

---

## Limits

- **Thumbnails only.** No original images, no video, no audio.
- **Folders are yours, not X's.** They are not X's interest tabs and nothing is
  inferred about your account.
- One library per app at a time; the CLI can open others with `--db`.

## What this will never do

No X API. No OAuth. No headless browser, Playwright, Puppeteer or Selenium. No
`auth_token` or `ct0` anywhere near the Rust code. No backend-issued GraphQL. No
third-party scraping API, proxies or residential IPs. No cloud sync, telemetry
or phone-home.

The app holds no X credential and cannot generate X traffic. One HTTP client is
linked in, and it exists for exactly one caller: the classifier, pointed at the
provider you configured. It knows no X host, and what it sends is your post text
and the previews you allowed it to send.
