/**
 * Tests for the capture userscript.
 *
 * ## Why this runs the real file
 *
 * The script is an IIFE with no exports, and that is deliberate — a userscript
 * you can read in one sitting should not grow an export surface for the benefit
 * of its tests. So this harness builds a stubbed browser, evaluates the actual
 * `xitter-dl-capture.user.js` inside it, and drives it through the public thing
 * it actually does: hooking `fetch`.
 *
 * That means these tests fail if the hooks stop being installed, if `clone()`
 * stops being called, or if the wire format drifts from the app's parser —
 * which is exactly the set of things worth protecting.
 *
 * Run:  node --test userscript/test/
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');

const SCRIPT = readFileSync(join(root, 'userscript', 'xitter-dl-capture.user.js'), 'utf8');
const PAGE = readFileSync(join(root, 'fixtures', 'bookmarks-page.json'), 'utf8');
const EXPECTED = JSON.parse(
  readFileSync(join(root, 'fixtures', 'bookmarks-page.expected.json'), 'utf8')
);

/** A URL for the fixture page, with a rotating doc_id that must not matter. */
const BOOKMARKS_URL = (hash) =>
  `https://x.com/i/api/graphql/${hash}/Bookmarks?variables=%7B%7D&features=%7B%7D`;

const settle = () => new Promise((r) => setTimeout(r, 20));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Build a world containing the script and just enough browser to run it.
 *
 * `fetchImpl` is installed *before* the script evaluates, because the script
 * captures `window.fetch` at hook time — as it must, to run at document-start.
 */
function makeWorld({ fetchImpl, xhrImpl, media } = {}) {
  const values = new Map();
  const requests = [];

  const win = {
    crypto: {
      getRandomValues(a) {
        for (let i = 0; i < a.length; i++) a[i] = (i * 37 + 11) & 255;
        return a;
      },
    },
    // No real DOM. The script must survive that, because the pill is a
    // convenience and capture is the product.
    document: {
      documentElement: {},
      body: {},
      addEventListener() {},
      createElement() {
        throw new Error('this stub has no DOM');
      },
    },
    // The hooks are scoped to the bookmarks timeline, so this world has to say
    // it is on it. See `watchRoute` in the script, and the route tests in
    // pill.test.mjs for the other half of that behaviour.
    location: { pathname: '/i/bookmarks' },
    MutationObserver: class {
      observe() {}
      disconnect() {}
    },
    fetch: fetchImpl,
    XMLHttpRequest: function () {},
  };
  win.XMLHttpRequest.prototype = { open() {}, send() {} };

  const ctx = vm.createContext({
    unsafeWindow: win,
    GM_getValue: (k, d) => (values.has(k) ? values.get(k) : d),
    GM_setValue: (k, v) => void values.set(k, v),
    GM_xmlhttpRequest(opts) {
      requests.push({
        method: opts.method,
        url: opts.url,
        headers: opts.headers || {},
        data: opts.data,
        responseType: opts.responseType,
      });
      setTimeout(() => {
        try {
          // A CDN answer, keyed by the URL as requested (query and all), so a
          // test can assert which variant was asked for.
          if (media && opts.url.indexOf('pbs.twimg.com') !== -1) {
            const answer = media[opts.url];
            if (!answer) {
              opts.onload({ status: 404, responseHeaders: 'content-type: text/plain' });
              return;
            }
            opts.onload({
              status: 200,
              response: answer.bytes ? Uint8Array.from(answer.bytes).buffer : answer,
              responseHeaders: 'content-type: ' + (answer.mime || 'image/jpeg'),
            });
            return;
          }
          if (opts.url.includes('/v1/hello')) {
            const probe = new URL(opts.url).searchParams.get('probe');
            opts.onload({
              status: 200,
              responseText: JSON.stringify({ app: 'xitter-dl', v: 1, probe }),
            });
          } else if (opts.url.includes('/v1/capture')) {
            opts.onload({
              status: 200,
              responseText: JSON.stringify({ ok: true, seen: 1, new: 1, updated: 0 }),
            });
          } else if (opts.url.includes('/v1/pair')) {
            opts.onload({ status: 401, responseText: JSON.stringify({ error: 'nope' }) });
          } else {
            opts.onload({ status: 404, responseText: '{}' });
          }
        } catch (e) {
          opts.onerror && opts.onerror(e);
        }
      }, 0);
    },
    console: { info() {}, warn() {}, log() {}, error() {} },
    setTimeout,
    clearTimeout,
    setInterval: () => 0,
    clearInterval: () => {},
    URL,
    // A browser has these; a bare VM context does not, and the thumbnail
    // encoder uses one. Missing here looked exactly like a fetch failure.
    btoa,
    atob,
  });

  vm.runInContext(SCRIPT, ctx);
  return { win, values, requests, ctx };
}

/** A stand-in for a fetch Response that records whether it was cloned. */
function fakeResponse(body, { ok = true } = {}) {
  const res = {
    ok,
    cloned: 0,
    clone() {
      res.cloned++;
      return { text: async () => body };
    },
  };
  return res;
}

function queued(values) {
  // The queue is persisted as a JSON string, not as an array. Handing the
  // manager a structured value is what produced
  // `Permission denied to access property "clonable"` on Firefox, so the
  // stored form is a string and the decoding happens here.
  const raw = values.get('queue');
  if (typeof raw !== 'string' || raw === '') return [];
  return JSON.parse(raw).map((l) => JSON.parse(l));
}

// ── the wire format ──────────────────────────────────────────────────────────

test('captures a bookmarks page through the fetch hook', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  const lines = queued(world.values);
  assert.equal(lines.length, 1, 'exactly one page record should be queued');
  assert.equal(lines[0].kind, 'page');
  assert.equal(lines[0].op, EXPECTED.op);
  assert.equal(lines[0].v, 1);
  assert.equal(typeof lines[0].captured_at, 'number');
});

test("ships X's bytes unchanged, so a parser fix applies retroactively", async () => {
  // The script deliberately does NOT re-implement the envelope parser. The
  // app's Rust parser is the tested one, and storing the raw envelope is what
  // lets it be improved later without re-capturing anything.
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  const [line] = queued(world.values);
  assert.deepEqual(line.raw, JSON.parse(PAGE), 'the envelope must round-trip exactly');
});

test('clones the response before reading, so the page still gets its body', async () => {
  // The single most important line in the script. Reading the original would
  // break x.com, and a capture script that breaks the page is worse than none.
  const response = fakeResponse(PAGE);
  const world = makeWorld({ fetchImpl: async () => response });

  const got = await world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  assert.equal(response.cloned, 1, 'must clone exactly once');
  assert.equal(got, response, 'the page must still receive its own response object');
});

// ── operation matching ───────────────────────────────────────────────────────

test('survives doc_id rotation, because it matches on the operation name', async () => {
  // X rotates the hash every 2-4 weeks. Both hashes must be captured; a matcher
  // built on the hash would break on the second one.
  const second = PAGE.replace('1843712994563928064', '1843712994563929999');
  assert.notEqual(second, PAGE, 'the two pages must differ, or dedupe would hide the test');

  const bodies = [PAGE, second];
  let i = 0;
  const world = makeWorld({ fetchImpl: async () => fakeResponse(bodies[i++]) });

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();
  world.win.fetch(BOOKMARKS_URL('ZZZ999'));
  await settle();

  assert.equal(queued(world.values).length, 2, 'both hashes must be captured');
});

test('ignores operations that are not bookmarks', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });

  for (const op of ['HomeTimeline', 'TweetDetail', 'SearchTimeline', 'UserTweets']) {
    world.win.fetch(`https://x.com/i/api/graphql/HASH/${op}?variables=%7B%7D`);
  }
  // And traffic that is not GraphQL at all.
  world.win.fetch('https://x.com/i/api/1.1/dm/inbox.json');
  world.win.fetch('https://pbs.twimg.com/media/abc.jpg');
  await settle();

  assert.equal(queued(world.values).length, 0, 'nothing should have been captured');
});

test('captures every bookmark operation, not just the main timeline', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });

  for (const op of ['Bookmarks', 'BookmarkSearch', 'BookmarkFolderTimeline', 'BookmarkFoldersSlice']) {
    world.win.fetch(`https://x.com/i/api/graphql/HASH/${op}?variables=%7B%7D`);
  }
  await settle();

  assert.equal(queued(world.values).length, 1, 'same body, so deduped to one');
});

// ── robustness: the script must never break the page ─────────────────────────

test('a repeated page is not sent twice', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();
  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  assert.equal(queued(world.values).length, 1);
});

test('nothing structured is ever handed to the manager', async () => {
  // `Permission denied to access property "clonable"` on Firefox came from
  // passing an array to GM storage: the manager reaches into the value from its
  // own compartment, the Xray wrapper refuses, and the call throws — which took
  // the whole boot down with it. So storage only ever sees a primitive, and the
  // two lists this script persists are serialised first.
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  assert.equal(world.values.size, 2, 'this capture should have persisted two keys');
  for (const [key, value] of world.values) {
    const kind = value === null ? 'null' : typeof value;
    assert.ok(
      value === null || ['string', 'number', 'boolean'].includes(kind),
      `${key} was stored as ${kind}; only primitives may cross this boundary`
    );
  }
  assert.equal(typeof world.values.get('queue'), 'string', 'the queue must persist as a string');
  assert.equal(
    typeof world.values.get('recentHashes'),
    'string',
    'the dedupe memory must persist as a string'
  );
});

test('a malformed body queues nothing and throws nothing', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse('{"data": { truncated…') });

  await world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  assert.equal(queued(world.values).length, 0);
});

test('a body with no timeline instructions queues nothing', async () => {
  // An error envelope, a rate-limit notice, an empty response — all normal.
  const world = makeWorld({
    fetchImpl: async () => fakeResponse(JSON.stringify({ errors: [{ message: 'nope' }] })),
  });

  await world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  assert.equal(queued(world.values).length, 0);
});

test('a failed response is ignored rather than parsed', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse('{}', { ok: false }) });

  await world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();

  assert.equal(queued(world.values).length, 0);
});

test('a rejected fetch still returns a rejecting promise to the page', async () => {
  // Swallowing the rejection would change the page's control flow.
  const world = makeWorld({
    fetchImpl: async () => {
      throw new Error('network down');
    },
  });

  await assert.rejects(() => world.win.fetch(BOOKMARKS_URL('AAA111')));
});

test('an empty body is ignored', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse('') });
  await world.win.fetch(BOOKMARKS_URL('AAA111'));
  await settle();
  assert.equal(queued(world.values).length, 0);
});

// ── the loopback contract ────────────────────────────────────────────────────

test('every request it makes goes to loopback, and only loopback', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });
  // A secret means the flush path will actually run.
  world.values.set('secret', 'a'.repeat(64));

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  // The flush is deliberately delayed so a fast scroll coalesces.
  await new Promise((r) => setTimeout(r, 1400));

  assert.ok(world.requests.length >= 2, `expected a hello and a capture, got ${world.requests.length}`);
  for (const r of world.requests) {
    assert.match(
      r.url,
      /^http:\/\/127\.0\.0\.1:\d+\//,
      `the script must never contact anything but loopback, saw ${r.url}`
    );
  }
});

test('discovers the port by echo rather than by broadcasting the secret', async () => {
  // The app answers /v1/hello with the probe we invented. Without that echo the
  // script would have to send the install secret to every port in the range to
  // find the app, handing it to whatever happened to be squatting on the wrong
  // one.
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });
  const secret = 'b'.repeat(64);
  world.values.set('secret', secret);

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await new Promise((r) => setTimeout(r, 1400));

  const hello = world.requests.find((r) => r.url.includes('/v1/hello'));
  assert.ok(hello, 'should have probed /v1/hello');
  assert.equal(hello.method, 'GET');
  assert.doesNotMatch(hello.url, new RegExp(secret), 'the secret must not appear in a probe');

  const capture = world.requests.find((r) => r.url.includes('/v1/capture'));
  assert.ok(capture, 'should have posted the capture');
  assert.equal(capture.headers.Authorization, `Bearer ${secret}`);
  assert.equal(capture.method, 'POST');
});

test('the capture body is NDJSON, one record per line', async () => {
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });
  world.values.set('secret', 'c'.repeat(64));

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await new Promise((r) => setTimeout(r, 1400));

  const capture = world.requests.find((r) => r.url.includes('/v1/capture'));
  assert.ok(capture, 'expected a capture POST');
  assert.ok(capture.data.endsWith('\n'), 'NDJSON is newline-terminated');
  const lines = capture.data.trim().split('\n');
  assert.equal(lines.length, 1, 'one page is one record');
  assert.equal(JSON.parse(lines[0]).kind, 'page');
});

test('captures are buffered, not dropped, while the app is closed', async () => {
  // No secret and no app: the record must survive for a later flush.
  const world = makeWorld({ fetchImpl: async () => fakeResponse(PAGE) });

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await new Promise((r) => setTimeout(r, 1400));

  assert.equal(world.requests.length, 0, 'nothing should be sent without a secret');
  assert.equal(queued(world.values).length, 1, 'and it must still be queued');
});

// ── thumbnails ───────────────────────────────────────────────────────────────

const PHOTO = 'https://pbs.twimg.com/media/AAA.jpg';
const POSTER = 'https://pbs.twimg.com/amplify_video_thumb/1/img/BBB.jpg';

/** A bookmarks page carrying one tweet with the media given. */
function pageWithMedia(media) {
  return JSON.stringify({
    data: {
      bookmark_timeline_v2: {
        timeline: {
          instructions: [
            {
              type: 'TimelineAddEntries',
              entries: [
                {
                  entryId: 'tweet-900001',
                  content: {
                    entryType: 'TimelineTimelineItem',
                    itemContent: {
                      tweet_results: {
                        result: {
                          __typename: 'Tweet',
                          rest_id: '900001',
                          core: {
                            user_results: {
                              result: { rest_id: '42', core: { name: 'Ada', screen_name: 'ada' } },
                            },
                          },
                          legacy: {
                            id_str: '900001',
                            full_text: 'a post with media',
                            entities: { urls: [], media: media },
                            extended_entities: { media: media },
                          },
                        },
                      },
                    },
                  },
                },
              ],
            },
          ],
        },
      },
    },
  });
}

const TWO_ITEMS = [
  { type: 'photo', media_url_https: PHOTO },
  { type: 'video', media_url_https: POSTER },
];

function mediaWorld(answers = {}) {
  return makeWorld({
    fetchImpl: async () => fakeResponse(pageWithMedia(TWO_ITEMS)),
    media: Object.assign(
      {
        [PHOTO + '?name=small']: { bytes: [0xff, 0xd8, 0xff, 0x00, 0x11], mime: 'image/jpeg' },
        [POSTER + '?name=small']: { bytes: [0xff, 0xd8, 0xff, 0x01], mime: 'image/jpeg' },
      },
      answers
    ),
  });
}

const thumbsRequested = (world) =>
  world.requests.filter((r) => r.url.indexOf('pbs.twimg.com') !== -1);

test('fetches a preview for every media item and queues a media record', async () => {
  const world = mediaWorld();
  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await wait(900);

  const lines = queued(world.values);
  assert.equal(lines[0].kind, 'page', 'the page record goes first');

  const media = lines.filter((l) => l.kind === 'media');
  assert.equal(media.length, 2, `expected two thumbnails, got ${media.length}`);

  // Keyed on the payload's own bare URL, so the app can match it to the row.
  assert.deepEqual(
    media.map((m) => m.url).sort(),
    [PHOTO, POSTER].sort()
  );
  assert.equal(media[0].post_id, '900001');
  assert.equal(media[0].mime, 'image/jpeg');

  const photo = media.find((m) => m.url === PHOTO);
  assert.equal(
    Buffer.from(photo.data, 'base64').toString('hex'),
    'ffd8ff0011',
    'the bytes must survive the base64 round trip'
  );
});

test('asks for ?name=small, and reads the body as binary', async () => {
  const world = mediaWorld();
  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await wait(900);

  const thumbs = thumbsRequested(world);
  assert.equal(thumbs.length, 2);
  for (const r of thumbs) {
    assert.match(r.url, /\?name=small$/, 'must ask for the timeline-size variant');
    // Without this a text body mangles the image before it is encoded.
    assert.equal(r.responseType, 'arraybuffer');
  }
});

test('does not fetch a preview it has already sent', async () => {
  const world = mediaWorld();
  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await wait(900);

  const before = thumbsRequested(world).length;
  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await wait(600);

  assert.equal(thumbsRequested(world).length, before, 'a re-scroll must cost no requests');
  assert.equal(queued(world.values).filter((l) => l.kind === 'media').length, 2);
});

test('runs the preview pass on a page whose bytes are already captured', async () => {
  // The backfill path. Re-opening the bookmarks page delivers the same page
  // bodies, which `firstSight` rejects — and those posts are exactly the ones
  // that still need previews. With the URL memory gone (a fresh browser), the
  // pass must run anyway.
  const world = mediaWorld();
  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await wait(900);

  world.values.delete('mediaSeen');
  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await wait(900);

  const lines = queued(world.values);
  assert.equal(lines.filter((l) => l.kind === 'page').length, 1, 'the page is a repeat');
  assert.equal(
    lines.filter((l) => l.kind === 'media').length,
    4,
    'but both previews were offered again'
  );
});

test('drops a preview that comes back as an error page', async () => {
  const world = makeWorld({
    fetchImpl: async () => fakeResponse(pageWithMedia([TWO_ITEMS[0]])),
    media: { [PHOTO + '?name=small']: { bytes: [0x3c, 0x68, 0x74], mime: 'text/html' } },
  });

  world.win.fetch(BOOKMARKS_URL('AAA111'));
  await wait(900);

  assert.equal(
    queued(world.values).filter((l) => l.kind === 'media').length,
    0,
    'an HTML body is not a thumbnail'
  );
});
