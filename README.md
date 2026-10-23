# xitter-dl

A local-first library for your X bookmarks. A companion userscript reads the
bookmarks X already sends your own browser, hands them to a desktop app over
loopback, and they land in a single SQLite file you can search offline in
milliseconds.

No API, no OAuth, no scraping service, no cloud, no account, no recurring cost.

---

## Status

| Piece | State |
|---|---|
| `crates/core` | Schema, X payload parsing, importers, FTS5 search, folders — 183 tests |
| `crates/core/src/bridge` | Loopback receiver and pairing — 57 of those, over real sockets |
| `crates/cli` | `xdl import / search / show / stats / bridge` |
| `crates/core/src/inference` | JEV client, answer shapes, resumable filing — 12 of those |
| `crates/app` | Tauri v2 shell: window, IPC commands, receiver lifecycle |
| `ui` | Svelte 5 frontend, three X themes, capture and pairing panel |
| `userscript` | Capture script — thumbnails included — 41 JS tests |
| Continuous capture | Not implemented |
| Categories | Folders in the right rail, drag to nest, SQL filter |
| Classifier | User-defined folders, JEV over your own key — see [Categories and classification](#categories-and-classification) |
| Media | Thumbnails only — see [Media is thumbnails only](#media-is-thumbnails-only) |

Rust: `201` tests. JavaScript: `41`.

---

## Building and running

Dependencies are fetched once; after that everything is offline.

```powershell
cargo fetch
cd ui; pnpm install; cd ..
```

Run the app:

```powershell
cd ui; pnpm build; cd ..
cargo build --release -p xitter-dl-app --features custom-protocol
.\target\release\xitter-dl.exe
```

Or dogfood the pipeline from a terminal, against the same library the app uses:

```powershell
cargo run -p xitter-dl-cli -- import .\fixtures\tweet-simple.json
cargo run -p xitter-dl-cli -- search "localfirst"
cargo run -p xitter-dl-cli -- show 1843712994563928064
cargo run -p xitter-dl-cli -- stats
```

### Debug builds need the dev server; release builds do not

Tauri picks the window URL at compile time. A **debug** build loads
`http://localhost:5273`; a **release** build with the `custom-protocol` feature
loads the frontend embedded from `ui/dist`.

So the debug binary on its own shows `ERR_CONNECTION_REFUSED`. Either:

```powershell
cd ui; pnpm tauri dev          # development, with hot reload
```

or build the release binary as above. Only the release path applies the real
CSP from `crates/app/tauri.conf.json`, so run it before trusting the CSP.

### Where the library lives

One SQLite file holds everything:

| | |
|---|---|
| Default | `%APPDATA%\xitter-dl\library.sqlite` |
| Override | `XITTER_DL_DB` |

The app and the CLI both resolve it through `xdl_core::default_library_path`, so
they cannot disagree about which file holds your bookmarks. `bridge.json` sits
beside it, which is why pairing carries between the two, and so does `media/`,
which is where thumbnails are cached. Nothing is stored inside a build, so the
installed exe and a dev build share one library and one cache.

Keep two libraries, and never point the app at the wrong one:

- **Yours** — the default path. Launch the app normally and this is what opens.
  Real bookmarks, kept indefinitely; this is the file to back up.
- **Throwaway** — `.scratch\library.sqlite`, reached by setting `XITTER_DL_DB`
  for one command. Fixtures, import tests and smoke runs go here, so a
  half-parsed payload never lands in the real thing.

```powershell
$env:XITTER_DL_DB = '.scratch\library.sqlite'   # this shell only
cargo run -p xitter-dl-cli -- stats
```

`cargo test` cannot touch the real library: no test resolves the default path,
and they work in `std::env::temp_dir()` instead.

---

## Capturing your bookmarks

The userscript files away bookmarks X **already sent** your browser. It
originates no requests, holds no X credential, and never touches the timeline
DOM.

1. Start the app. The left rail shows the connection state — **Connect browser**
   when nothing is paired, and a code that rotates every two minutes once you
   ask for one. When a browser is delivering, the same spot reads *Connected*
   with how long ago the last batch arrived.
2. Install `userscript/xitter-dl-capture.user.js` in Tampermonkey or
   Violentmonkey. Install it from the raw GitHub URL rather than pasting the
   file: Tampermonkey reads `@updateURL` from the copy it already has, so a
   pasted script has no URL to poll and will never update itself. Installed from
   the URL, it keeps itself current whenever `@version` moves.
3. Open `https://x.com/i/history` — X's bookmarks page. Click the pill in the
   bottom-right corner and type the code.

After that it works across restarts. You re-pair only if you revoke, reinstall
the script, clear its storage, or want to add a second browser — the app's
**Add another browser** issues a fresh code without disturbing the browsers
that already work.

Imported bookmarks appear in the app as they arrive.

### The pill is on every page; capture is not

The pill is attached on every x.com route. It is where capture status and pairing
state are visible, and a pill that exists on only one route cannot tell you why
it is missing on another. Capture is gated to X's bookmarks routes — `/i/history`,
which is where X serves the bookmarks timeline now, and the older
`/i/bookmarks`. On any other route the script patches nothing and writes nothing
to storage. What still runs is the Tampermonkey menu registration, the pill, and
a poll that reads `location.pathname`.

That list is a guess about X's routing, and it has been wrong twice — the second
time for weeks, because `/i/history` fetches `/graphql/Bookmarks` without being
called bookmarks anywhere. A wrong list does not error; it silently captures
nothing. The pill is the antidote: it reports the path it read and its verdict,
so the disagreement is visible on screen instead of being inferred from an empty
library.

Gating the pill as well was an overcorrection, and it is what made the pill
disappear. It widened the route check's blast radius from "the hooks" to "the
entire script", so a single answer of *no* — for any reason — produced no pill,
no capture, and nothing in the console. That is indistinguishable from a script
that was never installed, which is exactly how it got misread.

The route check still decides what is **captured**. It no longer decides what is
**visible**.

What the gate was originally for: boot ran everywhere and installed a
`subtree: true` `MutationObserver` over the document, disconnected only once the
pill had been built. A pill that could not be built left that observer firing on
every DOM change for the life of the tab — which is what "any page that is not
the bookmarks page fails to load" turned out to be. The observer is gone;
waiting for `<body>` is a bounded poll that always stops.

Hooks are armed on arrival at the timeline, removed on leaving it, and armed
again on return. Arming used to be one-time while disarming was not, so capture
died at the first navigation away and never came back.

Four rules fall out of all that, and every one of them was learned from the
console:

- **Nothing structured is handed to the userscript manager.** Values crossing
  that boundary are primitives; the queue and the dedupe memory are stored as
  JSON strings. Passing an array produced
  `Permission denied to access property "clonable"` on Firefox, which took the
  whole boot down with it.
- **No event-listener options object.** Firefox reads `capture` off that
  dictionary in the *page's* compartment and refuses one built in the script's:
  `Permission denied to access property "capture"`.
- **A failure is announced.** The script prints `[xitter-dl] <version> loaded`
  and every route change through `console.log`, whether or not debug logging is
  on, and anything that stops it working goes to `console.error` with the same
  prefix. Silence from a capture script is otherwise indistinguishable from a
  capture script that is working.
- **`console.log`, never `console.info`.** Firefox files `info` behind its own
  filter chip, so a beacon sent there is invisible to anyone with the console
  filtered to "Logs" — which is everyone. It was invisible here for two rounds
  of debugging.

The pill's panel repeats the last of these in the UI: it names the route the
script is reading and whether it counts as one it captures on, so a route check
that disagrees with the address bar can be seen without opening the console at
all.

On Firefox the hooks are installed only when `exportFunction` is available to
carry them across compartments; without it the script declines to hook and says
so, rather than handing page code a function it cannot call.

Two switches are reachable from the Tampermonkey menu, so they work even if the
page is broken: network hooks on/off, and debug logging on/off.

### Importing a single tweet without the script

The app's **Import** button takes a pasted payload, and `xdl import` takes a
file, a directory, NDJSON, or a HAR:

1. Open `x.com/i/history`.
2. DevTools → Network → filter `graphql` → find a `Bookmarks` request.
3. Right-click → Copy → Copy response.
4. Paste into the app's Import box.

Nothing is sent anywhere. The payload is parsed and stored locally.

### Running the userscript tests

```powershell
node userscript/test/capture.test.mjs
node userscript/test/pill.test.mjs
```

`node --test userscript/test/*.test.mjs` runs both files in one go. A directory
argument is not expanded by Node 26 — it is resolved as a module path and fails
with `MODULE_NOT_FOUND`, so name the files. The harnesses build
a stubbed browser, evaluate the **real** `.user.js` in it, and drive it through
the fetch hook, so they fail if the hooks stop installing, if `response.clone()`
stops being called, or if the wire format drifts from the Rust parser.

---

## How the two sides find each other

The app binds `127.0.0.1:8737`, or the next free port up to `8757`. The port is
not persisted.

`GET /v1/hello` answers an unauthenticated **pure echo** — it returns the random
`probe` value the caller invented, and nothing else. The script probes the port
range and sends its install secret only to a port that echoed its own nonce.

Without that echo, the script would have to present its secret to every port in
the range to discover which one is the app, handing it to whatever unrelated
process happened to be squatting on the wrong one.

Pairing is a one-time exchange: a short code, valid for two minutes, redeemed
for a 256-bit secret. The secret authenticates every capture afterwards and
never crosses the IPC boundary into the webview.

`xdl bridge` runs the same receiver in the foreground and prints the code as it
rotates — useful for diagnosing a script that will not connect, without a GUI:

```powershell
# terminal 1
cargo run -p xitter-dl-cli -- --db .scratch\e2e.sqlite bridge

# terminal 2 — pairs, captures a generated page, checks every refusal
node scripts/smoke-bridge.mjs A4KY-P8N4
```

---

## Layout

```
crates/core/     Schema, X payload parsing, import, search, the loopback bridge.
crates/cli/      `xdl` — the whole pipeline without a GUI.
crates/app/      Tauri v2 shell: window, typed IPC commands, receiver lifecycle.
ui/              Svelte 5 frontend.
userscript/      The capture script, plus its Node test harnesses.
fixtures/        Test payloads shared by the Rust tests, JS tests and the UI.
scripts/         Build helpers, fixture generator, bridge smoke test.
```

`crates/core` has no Tauri dependency, which is what makes the CLI, the test
suite and the importers usable without a GUI.

---

## Design decisions worth knowing

**The Chirp typeface is not in this repo.** X renders in Chirp, and matching it
is most of the difference between "looks like X" and "looks like X in the wrong
font". Chirp is also not redistributable — it is a commercial typeface from
[Grilli Type](https://www.grillitype.com) licensed to X, and its own metadata
says `All rights reserved` with no license field. The `chirp-font` repositories
on GitHub are unauthorised redistributions: not malware, but not licensed
either, and committing one would put all-rights-reserved software in a public
repo.

So the `@font-face` rules in `ui/src/styles/x.css` are committed and the files
are not (`ui/public/fonts/` is gitignored). Without them the stack falls through
to a system font — no error, no layout shift. Drop legitimate `.woff2` files in
and you get the real thing with no code change. For redistribution, pick a face
you are allowed to ship; the fallback list ends at Inter, the closest
freely-licensed relative.

**The script ships X's raw envelopes.** It does not build a `BookmarkRecord` in
JavaScript. It sends the envelope exactly as X sent it and lets the Rust parser
— the one with the tests — interpret it. Two consequences: the parser can be
fixed retroactively against bytes already captured, and the script never becomes
a second, untested implementation of the same logic. It parses only enough to
count posts for its pill and read the bottom cursor.

**The install secret is a file, not the OS keyring.** It is written to
`bridge.json` beside the library, mode `0600` on Unix, inside `%APPDATA%` on
Windows. The secret authenticates a loopback-only endpoint that exists only
while the app is open. A keyring would add a dependency tree and a failure mode
(a locked or unavailable credential store) for a marginal gain, given an
attacker who can read that file already has the user's session.

**Snippet delimiters are control characters (`U+0001`/`U+0002`), not `<mark>`.**
The frontend turns match positions into DOM nodes. Emitting markup from Rust and
`innerHTML`-ing it would turn any tweet containing markup into script execution.
Control characters cannot appear in tweet text or be typed into a search box, so
the frontend splits on them and builds elements. There is no `{@html}` anywhere
in the app. The CLI opts into printable delimiters, since a terminal cannot show
`U+0001`.

**Conversation modules are captured, but carry no `sortIndex`.** A
`TimelineTimelineModule` orders its tweets as a thread, not as bookmarks, so
letting that ordering into the bookmark ordering would scramble the library.
Skipping them would discard real posts the user can see.

**`sortIndex` is read from the timeline entry, not from `legacy`.** It is a
sibling of `content`. Reading `legacy.sort_index` yields `null` on every record
forever, and presents as "bookmark ordering isn't available" rather than as a
bug. There is a test named after this.

**Entity indices are verified, not trusted.** X's `[start, end]` indices may
count UTF-16 code units or Unicode code points. Get it wrong and every link in a
tweet containing an emoji shifts by one character. `TweetText.svelte` checks
every slice against the entity's own literal text, retries with code-point
indices, and falls back to plain text — an unstyled link is cosmetic, a
misplaced one is corruption.

**The bridge decodes chunked bodies.** The first version refused
`Transfer-Encoding` outright, on the assumption that the only client is our own
script. Node's `http.request` switches to chunked the moment `Content-Length` is
omitted and browsers streaming a body do the same, so that produced a 400
nothing on the client side could explain. The genuine smuggling risk is a
request declaring its length **two** ways, and that is what gets refused.
`Expect: 100-continue` is answered for the same reason — .NET sends it by
default for POSTs and will otherwise wait out its own timeout.

**Reads open their own SQLite connection.** A single mutex-guarded `Library`
would serialise every search behind every import and defeat the WAL pragma.
Writes take a mutex; reads do not.

**The userscript is plain JavaScript with no build step.** Greasy Fork requires
readable source rather than minified output, and a reviewer should be reading
the exact bytes that run.

**It requests `unsafeWindow`.** Patching `window.fetch` from a sandboxed
manager script is unreliable without reaching the page's real window, and a hook
that silently does not install looks exactly like "X stopped sending bookmarks".
The grant is used for one thing: assigning the two hooks.

**`tauri-specta` is not in use yet.** The TypeScript types in
`ui/src/lib/types.ts` are hand-written mirrors of `crates/core/src/model.rs`. A
Rust model change must be mirrored there by hand.

**`rusqlite` has no `fts5` feature.** FTS5 comes from the `bundled` feature,
whose build script passes `-DSQLITE_ENABLE_FTS5` to the C compiler. The
`trigram` tokenizer needs SQLite ≥ 3.34, and the bundle is far newer.

---

## Categories and classification

Folders are the tab strip under the page title, where X keeps its tabs: the
open one is bold with the accent bar, long names scroll sideways instead of
being cut off. Clicking one filters the feed with a recursive query, so a parent
shows everything inside its children. A child does not get its own tab (the
parent already includes it) unless you are in it.

The right rail is the editor: drag one folder onto another to nest it, onto a
top or bottom edge to reorder, onto the header to pull it back out. They are not
X's interest tabs. Every folder is a name and a sentence saying what belongs in
it, and both are yours — the same word means different things to different
people, so the definition travels with the name.

Filling them is the classifier's job. **Settings → Classification** takes a
provider (TypeSafe, OpenCode Zen, or any host serving the same API), a model and
an API key. The key goes to Windows Credential Manager: it is never written to
`settings.json`, never sent to the webview, and cannot be read back after
saving — only "saved ••••1234".

A run sends one structured request per post: the post as `state` (with any
quoted post attached), a `choice` question whose criteria are your folders, and
a `score` question for how central the post is to whichever folder it lands in.
Each choice is described by its path and its sentence — `Finance / Crypto —
chains, tokens, exchanges` — because a name alone is what the API falls back to,
and `Finance` beside `Trading` beside `VC` needs the boundary stated. A folder
with no sentence contributes only its path, so the panel says how many are in
that state before a run starts.

**Unsorted** is a folder the classifier creates the first time it runs, and it
exists so the question has a way out. Without it every post must land somewhere,
and a post that is a bare link gets filed at 1.00 confidence because there is
nothing to be unsure about — the question simply had no "none". Posts that fit
nothing else go there; it is an ordinary folder, so it can be renamed, nested or
emptied. What is stored is the verdict, its
confidence, the score and the model that said so. An image post — a photo with a
link and nothing else — is sent as an image, with the
`TypeSafe-Danger-Zone-Images` header the endpoint requires; a post with a picture
and a paragraph is sent as text. Turn images off in Settings for a model without
vision.

Nothing is filed until you press the button in the categories panel, and that
button sends a capped run by default — 25, 100, 250 or 500 posts, newest first,
with `all` as a deliberate choice — because a library-wide run is a bill and the
first one should be a sample. Requests are paced to roughly 190 a minute across
all workers: a free tier's window closes after about that many, and four workers
with no gate spend it in twenty seconds, after which every retry lands inside
the closed window and the run looks like it died. A rate limit now pauses the
run for thirty seconds and carries on, and every wave is appended to
`classify.log` beside the library, so a run that stops has a reason on disk
instead of in a webview that the next click replaced. Runs are resumable: a post that failed has no verdict, so the next run retries it, and
editing one folder's sentence re-runs exactly the posts whose verdict was
reached under the old wording. Deleting a folder takes its subfolders and their
verdicts with it, and never touches the posts.

---

## Media is thumbnails only

A bookmark shows the preview X's own timeline draws at that slot, and nothing
larger. No originals, no video, no audio — by design for now, because a
bookmarks feed contains videos of hundreds of megabytes and pulling those down
implicitly is a bandwidth and disk decision the user never made.

**What is fetched.** `?name=small` on the payload's own `media_url_https`: 680 px
wide, the size the timeline renders, 47 kB, aspect kept. Measured on real media:
`thumb` is 150×150 (a square crop) at 6.3 kB, `120x120` is 3.3 kB, `small` 47 kB,
`medium` 136 kB, `orig` 800 kB. A video's URL is its poster, and X ignores the
variant name there — one poster at 1200×675, ~59 kB, which is what a video
bookmark shows. The CLI prints which items are cached and which are not.

**Profile pictures** ride the same store. The userscript fetches the account's
`avatar.image_url` as the payload named it — `profile_images` serves no
`?name=small` — and the read layer attaches it to `author.avatarBlob` by that
URL. One file per account, reused by every post it wrote; without one the
avatar component draws a monogram. Accounts X marks `Square` in
`profile_image_shape` on the user result (business and brand accounts that
uploaded a logo) are drawn as a slightly rounded square rather than cropped into
a circle; the field is read from the stored raw payload, so it applies to posts
already in the library.

**Who fetches it.** The userscript, never the app. The app links exactly one
HTTP client — the classifier's, in `xdl_core::inference`, which reaches the
endpoint configured in Settings and nothing else — and holds no X credential
(PRD §4.3); the browser is the only place an image request can legitimately
originate. Note this is not the "cache hit" an
earlier draft described: the payload carries a bare URL and the variant the
page rendered lives only in the DOM the script refuses to read, so every
thumbnail is one deliberate request.

**Where it lives.** `media/<sha256>.<ext>` beside the database — for the default
library, `%APPDATA%\xitter-dl\media\`. Content-addressed and idempotent: the
same picture on twenty bookmarks is one file, and a re-scroll rewrites nothing.
Because it sits beside the library rather than inside a build, the installed
exe and a dev build share one cache.

**How it is attached.** `media_blobs` maps the *media URL* to that hash. Not the
`media` row: `replace_media` deletes and rewrites a post's media rows on every
re-import, so a thumbnail hanging off a row would disappear on the next scroll
of the bookmarks page. Keying on the URL also means a thumbnail may arrive
before the post it belongs to, which is a real ordering on the wire.

**How it is drawn.** The `media_thumb` command returns a `data:` URL for one
hash. The CSP stays `img-src 'self' data:` — no `asset:` scope, no file-read
capability handed to the webview, and the hash is validated against 64 hex
characters before any path is built.

**What a gap looks like.** An item without bytes draws the "not archived yet"
frame. Bookmarks captured before this landed stay in that state until the
bookmarks page is scrolled again; that pass collects previews from pages the
page-hash dedupe already rejects, which is exactly what makes backfill work.

**Switching it off.** The Tampermonkey menu has a "thumbnails" switch, separate
from the network hooks. Off means this script requests nothing from X's CDN;
capture is unaffected.

---

## What this will never do

No X API. No OAuth. No headless browser, Playwright, Puppeteer or Selenium. No
`auth_token` or `ct0` anywhere near the Rust code. No backend-issued GraphQL. No
third-party scraping API, proxies or residential IPs. No cloud sync, telemetry
or phone-home.

The app holds no X credential and cannot generate X traffic. One HTTP client is
linked in, and it exists for exactly one caller: the classifier, pointed at the
provider you configured. It knows no X host, and what it sends is your post text
and the previews you allowed it to send. That exception is enforced at the
dependency boundary — the module, not a convention — and it is the only one.
