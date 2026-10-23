<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import type { LibraryStats, PostView, SearchHit } from './lib/types';
  import Tweet from './lib/Tweet.svelte';
  import Icon from './lib/Icon.svelte';
  import Snippet from './lib/Snippet.svelte';
  import Pairing from './lib/Pairing.svelte';
  import Categories from './lib/Categories.svelte';
  import Settings from './lib/Settings.svelte';
  import CategoryTabs from './lib/CategoryTabs.svelte';
  import * as api from './lib/api';
  import {
    applyTheme,
    loadThemeChoice,
    saveThemeChoice,
    THEME_CHOICES,
    type ThemeChoice,
  } from './lib/theme';
  import { formatCount } from './lib/format';

  let posts = $state<PostView[]>([]);
  let hits = $state<SearchHit[] | null>(null);
  let query = $state('');
  let libraryStats = $state<LibraryStats | null>(null);
  let themeChoice = $state<ThemeChoice>(loadThemeChoice());
  let error = $state<string | null>(null);
  let loading = $state(true);
  /**
   * The folder the feed is showing, or null for the whole library. The filter
   * runs in SQL rather than over the loaded list, so opening a folder is one
   * query, not a client-side pass over five hundred rows.
   */
  let activeCategory = $state<string | null>(null);
  let activeLabel = $state<string | null>(null);
  /** Bumped when the classifier settings change, so the panel re-reads them. */
  let inferenceRevision = $state(0);
  /** Bumped by the rail panel after a folder edit, so the strip re-reads. */
  let categoriesRevision = $state(0);
  /** The strip's `+` opens the rail panel's form rather than a second one. */
  let addFolderSignal = $state(0);

  // Paste-to-import. The quickest route to "one tweet in the library" and it
  // needs no file dialog, no clipboard and no browser involvement.
  let showSettings = $state(false);
  let showImport = $state(false);
  let importText = $state('');
  let importBusy = $state(false);
  let importNote = $state<string | null>(null);

  // Any control outside the rail can ask the connection panel to open its code;
  // bumping this counter is the whole protocol.
  let connectSignal = $state(0);

  // The window-spanning scroller, so the "Bookmarks" rail item can double as
  // scroll-to-top the way X's own nav items do.
  let scroller = $state<HTMLElement | null>(null);

  const list = $derived(hits ? hits.map((h) => h.post) : posts);
  const snippetFor = $derived((id: string) => hits?.find((h) => h.post.id === id)?.snippet ?? null);

  // ── theme ────────────────────────────────────────────────────────────────
  $effect(() => {
    applyTheme(themeChoice);
    saveThemeChoice(themeChoice);
  });

  // Follow the OS live while "system" is selected, rather than only on boot.
  $effect(() => {
    if (themeChoice !== 'system' || typeof matchMedia !== 'function') return;
    const mq = matchMedia('(prefers-color-scheme: dark)');
    const onChange = () => applyTheme('system');
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  });

  // ── data ─────────────────────────────────────────────────────────────────
  /** Open a folder: it is a different query, and search does not apply to it. */
  function pickCategory(id: string | null, label: string | null) {
    activeCategory = id;
    activeLabel = label;
    if (query) query = '';
    hits = null;
    // The effect above notices `activeCategory`; calling refresh here as well
    // would run two reads of the same list.
  }

  async function refresh() {
    // Only a window with nothing on screen gets the skeleton. A live capture
    // arrives while you are reading, and swapping the feed for placeholder
    // tiles on every batch reads as the app going black and redrawing.
    if (list.length === 0) loading = true;
    error = null;
    try {
      if (api.isTauri()) {
        posts = await api.listBookmarks(500, 0, activeCategory);
        libraryStats = await api.stats();
      } else {
        error =
          'Not running inside the app shell. Start it with `pnpm tauri dev` — ' +
          'the frontend has no built-in mock data on purpose, so that the ' +
          'Rust model stays the single source of truth.';
      }
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  // Refresh when the open folder changes — and only then.
  //
  // `untrack` is the whole point: `refresh()` reads the list to decide whether
  // to raise the skeleton, so an effect that calls it directly both reads and
  // writes `posts`, re-runs itself, and spins. With a non-empty list the spin
  // was invisible (same rows, keyed, redrawn in place); on an empty folder it
  // alternated skeleton and empty state at frame rate, and the window never
  // got a quiet moment. Reading `activeCategory` and running the body
  // untracked leaves one dependency: the folder.
  $effect(() => {
    void activeCategory;
    untrack(() => void refresh());
  });

  // A batch names the posts it touched, so the feed re-reads those rows
  // instead of reloading five hundred of them — the reload was what made the
  // window stutter every time a preview arrived. `changed` is empty when there
  // were too many to list, and only then is a full refresh the right answer.
  const pendingIds = new Set<string>();
  let patchTimer: ReturnType<typeof setTimeout> | undefined;

  function queuePatch(ids: string[]) {
    if (!ids.length || hits) return;
    for (const id of ids) pendingIds.add(id);
    clearTimeout(patchTimer);
    patchTimer = setTimeout(async () => {
      const batch = [...pendingIds];
      pendingIds.clear();
      try {
        const views = await api.postsByIds(batch);
        if (!views.length) return;
        const byId = new Map(views.map((v) => [v.id, v]));
        // Patch only what is on screen. A batch about a post further down the
        // bookmarks list is not a reason to reload the window: the list it
        // would reload is the same list, and reloading it on every batch was
        // the stutter this whole path exists to remove.
        posts = posts.map((p) => byId.get(p.id) ?? p);
      } catch (e) {
        error = String(e);
      }
    }, 220);
  }

  // Live capture. Without this the list would be stale exactly when it matters
  // most: you scroll x.com, the receiver files twenty bookmarks, and the window
  // you are looking at shows none of them.
  onMount(() => {
    // A finished run changes both the folder counts and, when a folder is
    // open, which posts the feed should show.
    let offClassify: (() => void) | undefined;
    void api.onClassifyDone(() => {
      void refresh();
    }).then((off) => (offClassify = off));

    let off: (() => void) | undefined;
    let alive = true;
    void api
      .onBridgeEvent((event) => {
        if (!alive) return;
        if (event.type !== 'captured') return;
        // New bookmarks change the list itself — a page of them has to be
        // merged, not patched — so those still take a full read. Everything
        // else re-reads by id: that is what stops a scroll filing twenty pages
        // from re-parsing five hundred posts every couple of seconds.
        if (event.new > 0) void refresh();
        else if (event.changed.length) queuePatch(event.changed);
        // Changed ids are dropped past the cap, and media can arrive without
        // one; both cases mean "too much to describe", so reload.
        else if (event.media > 0) void refresh();
      })
      .then((f) => {
        if (alive) off = f;
        else f();
      })
      .catch(() => {
        // Not running in the shell; nothing to subscribe to.
      });
    return () => {
      alive = false;
      off?.();
    };
  });

  // Debounce: search runs on every keystroke in Rust, but there is no reason
  // to cross the IPC boundary mid-word more than necessary.
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const q = query.trim();
    clearTimeout(searchTimer);
    if (!q) {
      hits = null;
      return;
    }
    searchTimer = setTimeout(async () => {
      try {
        hits = await api.search(q, 200);
      } catch (e) {
        error = String(e);
      }
    }, 150);
    return () => clearTimeout(searchTimer);
  });

  async function doImport() {
    if (!importText.trim()) return;
    importBusy = true;
    importNote = null;
    try {
      const result = await api.importJson(importText);
      importNote =
        result.new > 0
          ? `Imported ${result.new} new post${result.new === 1 ? '' : 's'}.`
          : result.seen > 0
            ? 'Already in your library.'
            : 'No tweets found in that payload.';
      if (result.new > 0) {
        importText = '';
        await refresh();
      }
    } catch (e) {
      importNote = `Could not import: ${e}`;
    } finally {
      importBusy = false;
    }
  }

  function openLink(url: string) {
    void api.openExternal(url).catch((e) => (error = String(e)));
  }

  function goHome() {
    query = '';
    scroller?.scrollTo({ top: 0, behavior: 'smooth' });
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      if (showImport) showImport = false;
      else if (query) query = '';
    }
    // "/" focuses search, exactly as it does on X.
    if (event.key === '/' && document.activeElement?.tagName !== 'INPUT' &&
        document.activeElement?.tagName !== 'TEXTAREA') {
      event.preventDefault();
      document.getElementById('xdl-search')?.focus();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!--
  The whole frame scrolls as one, so the scrollbar lands on the window edge
  where X puts it, and the rails pin themselves with `position: sticky` the
  way X's nav and sidebar do.
-->
<div class="shell scroll" bind:this={scroller}>
  <div class="frame">
    <!--
      Left rail: X's nav column, put to work. This used to be empty gutter;
      X fills the same space with navigation and its primary action, so the
      brand, the one "page" this app has, and the two commands live here.
    -->
    <aside class="rail">
      <div class="pin left-pin">
        <div class="brand">
          <span class="mark">xdl</span>
          <span class="wordmark">xitter-dl</span>
        </div>

        <nav class="nav">
          <!-- 20px text, bold when current: X's nav-item scale exactly. -->
          <button class="navitem" class:active={!query} onclick={goHome} title="Bookmarks">
            <Icon name="bookmark" size={24} />
            <span class="navlabel">Bookmarks</span>
          </button>
        </nav>

        <div class="rail-actions">
          <!-- Connection lives here, not in the right rail: it is a state of the
               app, not a sidebar module, and it changes on its own. -->
          <Pairing openSignal={connectSignal} />
          <button class="ghost wide" onclick={() => (showImport = !showImport)}>
            Import JSON
          </button>
          <button class="ghost wide" onclick={() => (showSettings = true)}>Settings</button>
        </div>

        <!-- Theme lives at the foot of the rail as a segmented control: the
             native <select> it replaces was the last piece of default browser
             chrome in the window. -->
        <div class="rail-foot">
          <span class="foot-label">Theme</span>
          <div class="theme-picker" role="radiogroup" aria-label="Theme">
            {#each THEME_CHOICES as t (t.value)}
              <button
                class="theme-opt"
                class:active={themeChoice === t.value}
                role="radio"
                aria-checked={themeChoice === t.value}
                onclick={() => (themeChoice = t.value)}
              >
                {t.value === 'system' ? 'Auto' : t.label}
              </button>
            {/each}
          </div>
        </div>
      </div>
    </aside>

    <!--
      600px is X's own timeline width, and this column *is* the app. The side
      borders are X's too, and they are not decoration — they are what makes
      the rails read as gutter furniture and the column read as the feed.
    -->
    <main class="column">
      <!-- X's column header: 20px title, 13px meta line, blurred and sticky. -->
      <!--
        The title block and the folder strip share one sticky pane. Two sticky
        elements would need the header's height hard-coded as the strip's
        offset, and that number changes with the meta line.
      -->
      <div class="feedhead">
      <header class="listhead" class:hairline={!!query}>
        <h1>{query ? 'Search' : (activeLabel ?? 'Bookmarks')}</h1>
        <p class="meta">
          {#if query}
            {list.length} result{list.length === 1 ? '' : 's'} for “{query}”
          {:else if activeCategory}
            {list.length}{list.length === 500 ? '+' : ''} in this folder
          {:else if libraryStats}
            {formatCount(libraryStats.bookmarks)} bookmark{libraryStats.bookmarks === 1 ? '' : 's'}
            {#if libraryStats.removed > 0}
              · {libraryStats.removed} no longer present
            {/if}
          {:else}
            Your local archive
          {/if}
        </p>
      </header>

      {#if !query}
        <CategoryTabs
          active={activeCategory}
          revision={categoriesRevision}
          onpick={pickCategory}
          onadd={() => (addFolderSignal += 1)}
        />
      {/if}
      </div>

      {#if error}
        <div class="banner error">
          <span class="banner-text">{error}</span>
          <button class="banner-x" onclick={() => (error = null)} title="Dismiss">×</button>
        </div>
      {/if}

      {#if loading}
        <!-- Skeleton rows shaped like the tweet they stand in for, so nothing
             jumps when the real list lands. -->
        {#each [0, 1, 2] as i (i)}
          <div class="skeleton" aria-hidden="true">
            <div class="sk-avatar"></div>
            <div class="sk-lines">
              <div class="sk-line w45"></div>
              <div class="sk-line"></div>
              <div class="sk-line w70"></div>
            </div>
          </div>
        {/each}
      {:else if list.length === 0}
        <!-- 31px/36px at weight 800 is X's own empty-state headline scale. -->
        <div class="empty">
          {#if query}
            <h2>No results for “{query}”</h2>
            <p>
              Search covers the full text of every saved post, including long-form
              notes. Try another word, or press Esc to go back to the list.
            </p>
          {:else if activeCategory}
            <h2>Nothing filed here yet</h2>
            <p>
              “{activeLabel}” has no posts. File them from the categories panel on the
              right — the whole library, or a capped run to see what it does first.
            </p>
            <div class="empty-actions">
              <button class="ghost" onclick={() => (activeCategory = null)}>
                Show all bookmarks
              </button>
            </div>
          {:else}
            <h2>Nothing saved here yet</h2>
            <p>
              Scroll your bookmarks on x.com with the capture script running, or
              paste a captured payload to file your first post.
            </p>
            <div class="empty-actions">
              <button class="primary" onclick={() => (connectSignal += 1)}>Connect browser</button>
              <button class="ghost" onclick={() => (showImport = true)}>Import JSON</button>
            </div>
          {/if}
        </div>
      {:else}
        {#each list as post (post.id)}
          <div class="row">
            {#if snippetFor(post.id)}
              <Snippet snippet={snippetFor(post.id)!} />
            {/if}
            <Tweet {post} variant="timeline" onOpenLink={openLink} />
          </div>
        {/each}
      {/if}
    </main>

    <!--
      Right rail: X's sidebar, and where X keeps "Search Bookmarks" on its own
      bookmarks page — so search lives here too. The import and pairing panels
      open here as cards rather than shoving the feed down, which is the other
      thing the old full-width sections got wrong.
    -->
    <aside class="rail">
      <div class="pin right-pin scroll">
        <div class="searchwrap">
          <Icon name="search" size={16} />
          <input
            id="xdl-search"
            class="search"
            type="search"
            placeholder="Search Bookmarks"
            bind:value={query}
            autocomplete="off"
            spellcheck="false"
          />
          {#if query}<button class="clear" onclick={() => (query = '')} title="Clear">×</button>{/if}
        </div>

        {#if showImport}
          <section class="panel">
            <header class="panel-head">
              <h3>Import a payload</h3>
              <button class="panel-x" onclick={() => { showImport = false; importNote = null; }} title="Close">×</button>
            </header>
            <p class="hint">
              Paste the JSON of a captured GraphQL response — a whole bookmarks
              page, a <code>TweetDetail</code>, or a single
              <code>tweet_results.result</code>. Nothing is sent anywhere; this
              is parsed and stored locally.
            </p>
            <textarea
              bind:value={importText}
              placeholder={'{"data":{"tweetResult":{"result":{ … }}}}'}
              spellcheck="false"
            ></textarea>
            <div class="import-actions">
              <button class="primary" onclick={doImport} disabled={importBusy || !importText.trim()}>
                {importBusy ? 'Importing…' : 'Import'}
              </button>
              {#if importNote}<span class="note">{importNote}</span>{/if}
            </div>
          </section>
        {/if}

        {#if libraryStats}
          <section class="panel">
            <h3>Library</h3>
            <dl class="stats">
              <div class="stat"><dt>Bookmarks</dt><dd>{formatCount(libraryStats.bookmarks)}</dd></div>
              <div class="stat"><dt>Posts</dt><dd>{formatCount(libraryStats.posts)}</dd></div>
              <div class="stat"><dt>Authors</dt><dd>{formatCount(libraryStats.authors)}</dd></div>
              <div class="stat"><dt>Media items</dt><dd>{formatCount(libraryStats.media)}</dd></div>
              {#if libraryStats.removed > 0}
                <div class="stat"><dt>No longer present</dt><dd>{formatCount(libraryStats.removed)}</dd></div>
              {/if}
            </dl>
          </section>
        {/if}

        <section class="panel">
          <h3>Shortcuts</h3>
          <div class="keys">
            <span class="keyrow"><kbd>/</kbd> Focus search</span>
            <span class="keyrow"><kbd>Esc</kbd> Clear search, close panels</span>
          </div>
        </section>

        <Categories
          active={activeCategory}
          total={libraryStats?.bookmarks ?? 0}
          onpick={pickCategory}
          onsettings={() => (showSettings = true)}
          revision={inferenceRevision}
          onchanged={() => (categoriesRevision += 1)}
          addRequest={addFolderSignal}
        />

        <p class="colophon">Local-first. Nothing leaves this machine.</p>
      </div>
    </aside>
  </div>

  <Settings
    open={showSettings}
    onclose={() => (showSettings = false)}
    onsaved={() => {
      inferenceRevision += 1;
      void refresh();
    }}
  />
</div>

<style>
  .shell {
    height: 100vh;
    background: var(--bg);
    color: var(--text-primary);
  }

  /*
   * X's proportions: ~275px nav, 600px feed, ~350px sidebar, the lot centred.
   * The window ships at 1320×880, where this fits with room to breathe; on a
   * narrower window the grid degrades by shrinking the rails via minmax()
   * rather than reflowing, because a fixed desktop window does not need a
   * responsive relayout, just a graceful squeeze.
   */
  .frame {
    display: grid;
    grid-template-columns: minmax(200px, 275px) minmax(320px, 600px) minmax(280px, 360px);
    justify-content: center;
    min-height: 100vh;
  }

  /* ── rails ──────────────────────────────────────────────────────────── */

  .pin {
    position: sticky;
    top: 0;
  }

  .left-pin {
    height: 100vh;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 24px 20px 16px;
  }

  .right-pin {
    max-height: 100vh;
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 8px 16px 24px 24px;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 12px 12px;
  }

  .mark {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    font-size: 13px;
    font-weight: 800;
    letter-spacing: 0.02em;
  }

  .wordmark {
    font-weight: 800;
    font-size: 17px;
  }

  .nav {
    display: flex;
    flex-direction: column;
  }

  .navitem {
    display: flex;
    align-items: center;
    gap: 16px;
    width: fit-content;
    padding: 10px 16px 10px 12px;
    border-radius: 9999px;
    font-size: 20px;
    line-height: 24px;
    color: var(--text-primary);
  }

  .navitem:hover {
    background: var(--bg-hover);
  }

  .navitem.active .navlabel {
    font-weight: 700;
  }

  .rail-actions {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 12px;
    padding: 0 4px;
  }

  .wide {
    width: 100%;
  }

  .rail-foot {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 0 8px;
  }

  .foot-label {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-secondary);
  }

  .theme-picker {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .theme-opt {
    font-size: 12px;
    font-weight: 600;
    padding: 4px 10px;
    border-radius: 9999px;
    border: 1px solid var(--border);
    color: var(--text-secondary);
    background: transparent;
  }

  .theme-opt:hover {
    background: var(--bg-hover);
  }

  .theme-opt.active {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  /* ── buttons ────────────────────────────────────────────────────────── */

  .primary {
    background: var(--accent);
    color: #fff;
    font-weight: 700;
    font-size: 14px;
    border-radius: 9999px;
    padding: 8px 16px;
  }

  .primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .ghost {
    border: 1px solid var(--border);
    color: var(--text-primary);
    font-weight: 700;
    font-size: 14px;
    border-radius: 9999px;
    padding: 7px 16px;
    background: transparent;
  }

  .ghost:hover {
    background: var(--bg-hover);
  }

  /* ── centre column ──────────────────────────────────────────────────── */

  .column {
    border-left: 1px solid var(--border);
    border-right: 1px solid var(--border);
    min-width: 0;
  }

  .feedhead {
    position: sticky;
    top: 0;
    z-index: 2;
    background: color-mix(in srgb, var(--bg) 85%, transparent);
    backdrop-filter: blur(12px);
  }

  .listhead {
    padding: 8px 16px 10px;
  }

  .listhead h1 {
    margin: 0;
    font-size: 20px;
    line-height: 24px;
    font-weight: 800;
  }

  .listhead .meta {
    margin: 2px 0 0;
    font-size: 13px;
    line-height: 16px;
    color: var(--text-secondary);
  }

  .banner.error {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 12px 16px;
    padding: 10px 12px;
    border-radius: 8px;
    font-size: 13px;
    line-height: 18px;
    background: rgba(244, 33, 46, 0.1);
    color: #f4212e;
  }

  .banner-text {
    flex: 1;
    min-width: 0;
    overflow-wrap: break-word;
  }

  .banner-x {
    color: inherit;
    font-size: 16px;
    line-height: 1;
    opacity: 0.7;
  }

  .banner-x:hover {
    opacity: 1;
  }

  /* ── skeletons ──────────────────────────────────────────────────────── */

  .skeleton {
    display: flex;
    gap: 12px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    animation: sk-pulse 1.5s ease-in-out infinite;
  }

  .sk-avatar {
    width: 40px;
    height: 40px;
    border-radius: 9999px;
    background: var(--bg-active);
    flex-shrink: 0;
  }

  .sk-lines {
    flex: 1;
    padding-top: 2px;
  }

  .sk-line {
    height: 12px;
    border-radius: 6px;
    background: var(--bg-active);
    margin-bottom: 8px;
  }

  .sk-line.w45 {
    width: 45%;
  }

  .sk-line.w70 {
    width: 70%;
  }

  @keyframes sk-pulse {
    50% {
      opacity: 0.45;
    }
  }

  /* ── empty states ───────────────────────────────────────────────────── */

  .empty {
    max-width: 380px;
    margin: 0 auto;
    padding: 48px 32px;
  }

  .empty h2 {
    margin: 0 0 8px;
    font-size: 31px;
    line-height: 36px;
    font-weight: 800;
    overflow-wrap: break-word;
  }

  .empty p {
    margin: 0 0 20px;
    font-size: 15px;
    line-height: 20px;
    color: var(--text-secondary);
  }

  .empty-actions {
    display: flex;
    gap: 12px;
  }

  /* ── search ─────────────────────────────────────────────────────────── */

  .searchwrap {
    display: flex;
    align-items: center;
    gap: 10px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 9999px;
    padding: 0 16px;
    height: 44px;
    color: var(--text-secondary);
    flex-shrink: 0;
  }

  .searchwrap:focus-within {
    border-color: var(--accent);
    background: var(--bg);
    color: var(--accent);
  }

  .search {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    outline: none;
    color: var(--text-primary);
    font-size: 15px;
    height: 100%;
  }

  .search::placeholder {
    color: var(--text-secondary);
  }

  .search::-webkit-search-cancel-button {
    display: none;
  }

  .clear {
    color: var(--text-secondary);
    font-size: 18px;
    line-height: 1;
  }

  .clear:hover {
    color: var(--text-primary);
  }

  /* ── sidebar cards ──────────────────────────────────────────────────── */

  .panel {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 12px 16px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .panel h3 {
    margin: 0;
    font-size: 17px;
    line-height: 20px;
    font-weight: 800;
  }

  .panel-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .panel-x {
    margin-left: auto;
    color: var(--text-secondary);
    font-size: 18px;
    line-height: 1;
  }

  .panel-x:hover {
    color: var(--text-primary);
  }

  .hint {
    margin: 0;
    font-size: 13px;
    line-height: 17px;
    color: var(--text-secondary);
  }

  code {
    font-family: var(--font-mono);
    font-size: 12px;
    background: var(--bg-active);
    padding: 1px 4px;
    border-radius: 4px;
  }

  textarea {
    width: 100%;
    min-height: 96px;
    resize: vertical;
    background: var(--bg);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 10px 12px;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 18px;
    outline: none;
  }

  textarea:focus {
    border-color: var(--accent);
  }

  .import-actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .note {
    font-size: 13px;
    color: var(--text-secondary);
  }

  .stats {
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .stat {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
    line-height: 16px;
  }

  .stat dt {
    color: var(--text-secondary);
  }

  .stat dd {
    margin: 0;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .keys {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .keyrow {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--text-secondary);
  }

  kbd {
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1;
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: 4px;
    padding: 2px 5px;
  }

  .colophon {
    margin: 0;
    padding: 0 8px;
    font-size: 12px;
    color: var(--text-secondary);
  }
</style>
