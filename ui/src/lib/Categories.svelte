<script lang="ts">
  /**
   * Folders, in the right rail.
   *
   * ## Why a tree and not tabs
   *
   * A tab strip answers "show me one thing"; a folder tree answers "where did I
   * put it", and lets a folder hold folders. The window draws the flattened
   * tree the backend returns — one indent per level, drag order preserved.
   *
   * ## The goo
   *
   * `liquid-gooey` is React, so this is the same effect built on the primitive
   * it wraps: a blurred layer whose alpha is pushed through a steep contrast
   * matrix, which turns overlapping shapes into one silhouette with a neck
   * between them. The SVG filter lives here and is applied only while dragging;
   * at rest the rows are ordinary rects, and the labels never pass through the
   * filter, so text stays crisp.
   *
   * ## Drag semantics
   *
   * - middle band of a row → the folder becomes a child of that row
   * - top / bottom edge → reorder among that row's siblings
   * - the panel header, or nothing → back to the root
   *
   * A move that would put a folder inside itself is refused by the backend,
   * which is where the cycle check lives; this side just shows the error.
   */
  import type { Category, ClassifyProgress, InferenceView } from './types';
  import * as api from './api';

  interface Props {
    /** The folder being shown, or null for the whole library. */
    active: string | null;
    /** How many bookmarks exist in total, for the "All" row. */
    total: number;
    /** `label` is passed alongside so the header can name the open folder. */
    onpick: (id: string | null, label: string | null) => void;
    /** Opens the classifier settings, which live above this panel. */
    onsettings: () => void;
    /** Changes when the settings were edited, so the panel re-reads the key. */
    revision: number;
    /** Fired after any edit, so the strip at the top of the feed re-reads. */
    onchanged: () => void;
    /** The strip's `+`: opens this panel's form instead of a second one. */
    addRequest: number;
  }

  let { active, total, onpick, onsettings, revision, onchanged, addRequest }: Props = $props();

  let cats = $state<Category[]>([]);
  let error = $state<string | null>(null);

  /** id → element, for hit testing during a drag. Not reactive on purpose. */
  const rowEl: Record<string, HTMLElement> = {};
  let panelEl = $state<HTMLElement | null>(null);

  // ── editing ───────────────────────────────────────────────────────────────
  /** The category being edited inline, or `undefined` when nothing is. */
  let editing = $state<string | undefined>(undefined);
  let editLabel = $state('');
  let editCriteria = $state('');
  /** Parent for a new category: `undefined` = closed, `null` = root. */
  let creating = $state<string | null | undefined>(undefined);
  let draft = $state('');
  let confirmDelete = $state<string | null>(null);

  const flat = $derived(tree(cats));

  /** Children first, then a stable id tiebreak, so a redraw never reorders. */
  function tree(list: Category[]): Array<{ cat: Category; depth: number }> {
    const byParent = new Map<string, Category[]>();
    for (const c of list) {
      const key = c.parentId ?? '';
      const bucket = byParent.get(key) ?? [];
      bucket.push(c);
      byParent.set(key, bucket);
    }
    for (const bucket of byParent.values()) {
      bucket.sort((a, b) => a.position - b.position || a.label.localeCompare(b.label));
    }
    const out: Array<{ cat: Category; depth: number }> = [];
    const walk = (parent: string, depth: number) => {
      for (const cat of byParent.get(parent) ?? []) {
        out.push({ cat, depth });
        walk(cat.id, depth + 1);
      }
    };
    walk('', 0);
    return out;
  }

  function labelOf(id: string | null): string {
    if (!id) return 'the library';
    return cats.find((c) => c.id === id)?.label ?? '…';
  }

  function apply(next: Category[]) {
    cats = next;
    error = null;
  }

  /** A mutation: the panel and the strip both follow the backend's answer. */
  function mutated(next: Category[]) {
    apply(next);
    onchanged();
  }

  function fail(e: unknown) {
    error = String(e);
  }

  function reload() {
    api
      .categoriesList()
      .then(mutated)
      .catch(fail);
  }

  $effect(() => {
    reload();
  });

  // ── classifier ────────────────────────────────────────────────────────────
  //
  // The run lives in the backend; this panel only starts it and draws what
  // comes back. Progress arrives as events so the window stays live during a
  // long run — a promise that resolves in eleven minutes would look frozen.

  let infer = $state<InferenceView | null>(null);
  let running = $state(false);
  let progress = $state<ClassifyProgress | null>(null);
  let note = $state<string | null>(null);
  /**
   * How many posts one run may send. The user pays per token, so the first run
   * is a sample by default rather than the whole library — `all` is there, but
   * it is a choice, not the only button.
   */
  let cap = $state<number | null>(100);

  async function loadInference() {
    try {
      infer = await api.inferenceConfig();
    } catch (e) {
      // A missing key or an unreachable backend is not a reason to hide the
      // panel; the folders still work, they are just empty.
      note = String(e);
    }
  }

  $effect(() => {
    // Reading `revision` here is the point: saving a key in Settings has to
    // flip this panel's button from "add a key" to "file N posts".
    void revision;
    void loadInference();
  });

  // The strip's `+` and this panel's `+` are one form, so a folder created from
  // the top of the feed is created where the open folder says it should be.
  $effect(() => {
    if (addRequest > 0) startCreate();
  });

  $effect(() => {
    const stops = [
      api.onClassifyProgress((p) => {
        running = true;
        progress = p;
      }),
      api.onClassifyDone((d) => {
        running = false;
        progress = null;
        infer = infer ? { ...infer, status: d.status } : infer;
        const parts = [`${d.report.filed} filed`];
        if (d.report.unsorted > 0) parts.push(`${d.report.unsorted} unsorted`);
        if (d.report.failed > 0) parts.push(`${d.report.failed} failed`);
        if (d.report.rateLimited) parts.push('waited out a rate limit');
        if (d.report.cancelled) parts.push('stopped early');
        note = parts.join(' · ') + (d.report.firstError ? ` — ${d.report.firstError}` : '');
        reload();
        void loadInference();
      }),
      api.onClassifyError((message) => {
        running = false;
        progress = null;
        note = message;
        void loadInference();
      }),
    ];
    return () => {
      for (const stop of stops) void stop.then((off) => off());
    };
  });

  async function startClassify(force: boolean) {
    note = null;
    try {
      await api.classifyStart(force, force ? null : cap);
      running = true;
      progress = { done: 0, considered: infer?.status.pending ?? 0, failed: 0 };
    } catch (e) {
      note = String(e);
      running = false;
    }
  }

  async function stopClassify() {
    try {
      await api.classifyCancel();
    } catch (e) {
      note = String(e);
    }
  }

  /**
   * Folders the model has only a name for.
   *
   * The API documents that a choice without a description "is interpreted by
   * its name alone" — which, for overlapping names like Finance/Trading/VC, is
   * the difference between filing and guessing. Saying so is cheaper than
   * letting a bad run look like a bad model. "Unsorted" ships with its own.
   */
  const undefinedFolders = $derived(
    cats.filter((c) => c.id !== 'unsorted' && !c.criteria.trim()).length,
  );

  /** What this run would actually send: the cap, or what is left. */
  const take = $derived(
    infer ? Math.min(cap ?? infer.status.pending, infer.status.pending) : 0,
  );

  const pct = $derived(
    progress && progress.considered > 0
      ? Math.min(100, Math.round((progress.done / progress.considered) * 100))
      : 0,
  );

  // ── create / rename / delete ──────────────────────────────────────────────

  /** New folders land inside the open folder, which is where you are looking. */
  function startCreate() {
    creating = active;
    draft = '';
    editing = undefined;
    error = null;
    queueMicrotask(() => {
      const el = document.querySelector<HTMLInputElement>('.cat-new input');
      el?.focus();
    });
  }

  async function submitCreate() {
    const label = draft.trim();
    if (!label) return;
    try {
      mutated(await api.categoryCreate(creating ?? null, label));
      creating = undefined;
      draft = '';
    } catch (e) {
      fail(e);
    }
  }

  function startEdit(cat: Category) {
    editing = cat.id;
    creating = undefined;
    editLabel = cat.label;
    editCriteria = cat.criteria;
    confirmDelete = null;
  }

  async function submitEdit() {
    if (!editing) return;
    const label = editLabel.trim();
    if (!label) {
      editing = undefined;
      return;
    }
    try {
      mutated(await api.categoryUpdate(editing, label, editCriteria));
      editing = undefined;
    } catch (e) {
      fail(e);
    }
  }

  async function remove(id: string) {
    if (confirmDelete !== id) {
      confirmDelete = id;
      return;
    }
    confirmDelete = null;
    try {
      mutated(await api.categoryDelete(id));
      if (active === id) onpick(null, null);
    } catch (e) {
      fail(e);
    }
  }

  // ── drag ──────────────────────────────────────────────────────────────────

  interface Box {
    id: string;
    top: number;
    left: number;
    width: number;
    height: number;
  }

  interface Drag {
    id: string;
    label: string;
    /** Pointer, in client coordinates. */
    x: number;
    y: number;
    /** Panel origin at the moment the drag started. */
    ox: number;
    oy: number;
    /** Client rects of every row at drag start. */
    boxes: Box[];
    width: number;
    height: number;
  }

  let pending = $state<{ id: string; x: number; y: number } | null>(null);
  let drag = $state<Drag | null>(null);
  /** The dragged row's own size, captured before it leaves the list. */
  let drag0: { width: number; height: number } | null = null;
  let hover = $state<{ id: string; mode: 'into' | 'before' | 'after' } | null>(null);
  let suppressClick = false;

  function onDown(event: PointerEvent, cat: Category) {
    if (event.button !== 0) return;
    // The row's own controls handle their own pointers.
    if ((event.target as HTMLElement).closest('button, input')) return;
    pending = { id: cat.id, x: event.clientX, y: event.clientY };
  }

  function onMove(event: PointerEvent) {
    if (pending && !drag) {
      const moved = Math.hypot(event.clientX - pending.x, event.clientY - pending.y);
      if (moved < 5) return;
      startDrag(event.clientX, event.clientY);
      return;
    }
    if (!drag) return;
    drag.x = event.clientX;
    drag.y = event.clientY;

    const box = drag.boxes.find(
      (b) =>
        event.clientY >= b.top &&
        event.clientY <= b.top + b.height &&
        event.clientX >= b.left &&
        event.clientX <= b.left + b.width,
    );
    if (!box || box.id === drag.id) {
      hover = null;
      return;
    }
    const within = (event.clientY - box.top) / box.height;
    // The middle of a row means "put it inside"; the edges mean "reorder".
    // 0.3/0.7 is wide enough to hit with a mouse and narrow enough that the
    // edges are still reachable.
    hover = {
      id: box.id,
      mode: within < 0.3 ? 'before' : within > 0.7 ? 'after' : 'into',
    };
  }

  function startDrag(x: number, y: number) {
    if (!pending || !panelEl) return;
    const id = pending.id;
    const cat = cats.find((c) => c.id === id);
    if (!cat) return;
    const panel = panelEl.getBoundingClientRect();
    const boxes: Box[] = [];
    for (const [key, el] of Object.entries(rowEl)) {
      const r = el.getBoundingClientRect();
      if (key === id) {
        // The floating pill stands in for this row.
        drag0 = { width: r.width, height: r.height };
        continue;
      }
      boxes.push({ id: key, top: r.top, left: r.left, width: r.width, height: r.height });
    }
    drag = {
      id,
      label: cat.label,
      x,
      y,
      ox: panel.left,
      oy: panel.top,
      boxes,
      width: drag0?.width ?? 200,
      height: drag0?.height ?? 32,
    };
    drag0 = null;
    hover = null;
    pending = null;
    suppressClick = true;
  }

  function onUp() {
    if (!drag) {
      pending = null;
      return;
    }
    const moved = drag;
    const target = hover;
    drag = null;
    hover = null;
    // A drop that ends outside any row produces no click, and a flag left set
    // would swallow the next one.
    setTimeout(() => (suppressClick = false), 250);
    if (!target) return;

    const targetCat = cats.find((c) => c.id === target.id);
    if (!targetCat) return;

    let parent: string | null;
    let position: number | null;
    if (target.mode === 'into') {
      parent = targetCat.id;
      position = null;
    } else {
      parent = targetCat.parentId;
      const siblings = cats
        .filter((c) => c.parentId === parent && c.id !== moved.id)
        .sort((a, b) => a.position - b.position);
      const at = siblings.findIndex((c) => c.id === targetCat.id);
      position = target.mode === 'before' ? at : at + 1;
    }

    void api
      .categoryMove(moved.id, parent, position)
      .then(mutated)
      .catch(fail);
  }

  /** Dropping on the header puts a folder back at the top level. */
  function onHeaderPointerUp() {
    if (!drag) return;
    const id = drag.id;
    drag = null;
    hover = null;
    void api.categoryMove(id, null, null).then(mutated).catch(fail);
  }

  /** Where to draw the "insert here" line, if the drop is a reorder. */
  const dropLine = $derived.by(() => {
    if (!drag || !hover || hover.mode === 'into') return null;
    const box = drag.boxes.find((b) => b.id === hover!.id);
    if (!box) return null;
    return {
      left: box.left - drag.ox,
      width: box.width,
      top: box.top - drag.oy + (hover.mode === 'after' ? box.height : -1),
    };
  });

  function pick(id: string | null, label: string | null = null) {
    // The click that ends a drag is not a request to open anything.
    if (suppressClick) {
      suppressClick = false;
      return;
    }
    onpick(id, label);
  }
</script>

<svelte:window onpointermove={onMove} onpointerup={onUp} />

<!-- The filter: blur, then a steep alpha ramp. The result is a single
     silhouette wherever two shapes come close, which is the whole trick. -->
<svg class="defs" aria-hidden="true">
  <defs>
    <filter id="cat-goo">
      <feGaussianBlur in="SourceGraphic" stdDeviation="5" result="blur" />
      <feColorMatrix
        in="blur"
        mode="matrix"
        values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 19 -9"
        result="goo"
      />
      <!-- Only the goo result: blending the source back over it would hide
           the neck that forms where two shapes meet. -->
    </filter>
  </defs>
</svg>

<section class="panel" bind:this={panelEl}>
  <header class="head" onpointerup={onHeaderPointerUp} role="presentation">
    <h3>Categories</h3>
    <button class="add" onclick={startCreate} title="New category inside {labelOf(active)}">+</button>
  </header>

  {#if error}
    <p class="err">{error}</p>
  {/if}

  {#if creating !== undefined}
    <form
      class="cat-new"
      onsubmit={(e) => {
        e.preventDefault();
        void submitCreate();
      }}
    >
      <input
        bind:value={draft}
        placeholder="Name this folder"
        maxlength="40"
        onkeydown={(e) => e.key === 'Escape' && (creating = undefined)}
      />
      <button class="ok" type="submit" disabled={!draft.trim()}>Add</button>
    </form>
  {/if}

  <div class="rows">
    <div
      class="row all"
      class:on={active === null}
      role="button"
      tabindex="0"
      onclick={() => pick(null)}
      onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && pick(null)}
    >
      <span class="name">All bookmarks</span>
      <span class="n">{total}</span>
    </div>

    {#each flat as r (r.cat.id)}
      {#if editing === r.cat.id}
        <form
          class="cat-edit"
          style="margin-left: {r.depth * 14}px"
          onsubmit={(e) => {
            e.preventDefault();
            void submitEdit();
          }}
        >
          <input
            class="edit-name"
            bind:value={editLabel}
            maxlength="40"
            placeholder="Name"
            onkeydown={(e) => e.key === 'Escape' && (editing = undefined)}
          />
          <input
            class="edit-criteria"
            bind:value={editCriteria}
            maxlength="200"
            placeholder="What belongs here (read by the classifier)"
            onkeydown={(e) => e.key === 'Escape' && (editing = undefined)}
          />
          <div class="edit-actions">
            <button class="ok" type="submit">Save</button>
            <button class="quiet" type="button" onclick={() => (editing = undefined)}>Cancel</button>
          </div>
        </form>
      {:else}
        <div
          class="row"
          class:on={active === r.cat.id}
          class:into={hover?.id === r.cat.id && hover.mode === 'into'}
          class:source={drag?.id === r.cat.id}
          bind:this={rowEl[r.cat.id]}
          style="margin-left: {r.depth * 14}px"
          role="button"
          tabindex="0"
          title={r.cat.criteria || 'No description yet — double-click to add one'}
          onpointerdown={(e) => onDown(e, r.cat)}
          onclick={() => pick(r.cat.id, r.cat.label)}
          ondblclick={() => startEdit(r.cat)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') pick(r.cat.id, r.cat.label);
            if (e.key === 'F2') startEdit(r.cat);
            if (e.key === 'Delete') void remove(r.cat.id);
          }}
        >
          {#if r.depth > 0}<span class="twig" aria-hidden="true"></span>{/if}
          <span class="name">{r.cat.label}</span>
          {#if r.cat.own > 0 && r.cat.total !== r.cat.own}
            <span class="n sub" title="{r.cat.own} here, {r.cat.total} including subfolders">
              {r.cat.own}/{r.cat.total}
            </span>
          {:else}
            <span class="n">{r.cat.total}</span>
          {/if}
          <button
            class="kill"
            class:armed={confirmDelete === r.cat.id}
            title={confirmDelete === r.cat.id ? 'Click again to delete, with its subfolders' : 'Delete'}
            onclick={(e) => {
              e.stopPropagation();
              void remove(r.cat.id);
            }}
          >
            {confirmDelete === r.cat.id ? 'Sure?' : '×'}
          </button>
        </div>
      {/if}
    {/each}
  </div>

  {#if cats.length >= 2 && !cats.some((c) => c.parentId)}
    <p class="hint">Drag one folder onto another to nest it.</p>
  {/if}

  <!-- Filing is the panel's job, so the control that fills the folders lives
       here rather than in Settings: you look at the empty folders, then press
       the button next to them. -->
  <div class="classify">
    {#if !infer}
      <p class="hint">No classifier configured.</p>
    {:else if !infer.hasKey}
      <button class="run" onclick={onsettings}>Add a key to file posts</button>
    {:else if running}
      <div class="bar" aria-hidden="true"><span style="width: {pct}%"></span></div>
      <div class="runrow">
        <span class="count">
          {progress ? `${progress.done} of ${progress.considered}` : 'starting…'}
          {#if progress && progress.failed > 0}· {progress.failed} failed{/if}
        </span>
        <button class="stop" onclick={stopClassify}>Stop</button>
      </div>
    {:else}
      <div class="runrow">
        {#if infer.status.pending > 0}
          <button class="run" onclick={() => startClassify(false)}>
            File {take} post{take === 1 ? '' : 's'}
          </button>
          <!-- The cap is deliberately a control, not a constant: the first run
               should cost pennies, and "all" stays available once it is trusted. -->
          <select
            class="cap"
            bind:value={cap}
            title="How many posts this run may send — newest first"
          >
            <option value={25}>25</option>
            <option value={100}>100</option>
            <option value={250}>250</option>
            <option value={500}>500</option>
            <option value={null}>all</option>
          </select>
        {:else}
          <button class="run quiet" onclick={() => startClassify(true)}>Re-file everything</button>
        {/if}
      </div>
      {#if infer.status.pending > 0 && undefinedFolders > 0}
        <p class="hint">
          {undefinedFolders} folder{undefinedFolders === 1 ? ' has' : 's have'} no definition — JEV
          sees only the name. Double-click one to describe what belongs in it.
        </p>
      {/if}
      {#if infer.status.filed > 0 && infer.status.pending > 0}
        <button class="again" onclick={() => startClassify(true)} title="Re-run every post, not only the waiting ones">
          or re-file everything
        </button>
      {/if}
    {/if}
    {#if note}<p class="note">{note}</p>{/if}
  </div>

  {#if drag}
    <div class="goo" style="filter: url(#cat-goo)">
      {#each drag.boxes as b (b.id)}
        <div
          class="blob"
          class:absorb={hover?.id === b.id && hover.mode === 'into'}
          style="left: {b.left - drag.ox}px; top: {b.top - drag.oy}px; width: {b.width}px; height: {b.height}px"
        ></div>
      {/each}
      <div
        class="blob floating"
        style="width: {drag.width}px; height: {drag.height}px; transform: translate3d({drag.x -
          drag.ox -
          drag.width / 2}px, {drag.y - drag.oy - drag.height / 2}px, 0)"
      ></div>
      {#if dropLine}
        <div
          class="ins"
          style="left: {dropLine.left}px; top: {dropLine.top}px; width: {dropLine.width}px"
        ></div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .defs {
    position: absolute;
    width: 0;
    height: 0;
    overflow: hidden;
  }

  .panel {
    position: relative;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 12px 16px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .head h3 {
    margin: 0;
    font-size: 17px;
    line-height: 20px;
    font-weight: 800;
    flex: 1;
  }

  .add {
    border: 0;
    background: none;
    color: var(--text-secondary);
    font-size: 20px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 9999px;
    cursor: pointer;
  }

  .add:hover {
    color: var(--text-primary);
    background: var(--bg-hover);
  }

  .err {
    margin: 0;
    color: #f4212e;
    font-size: 12px;
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0 6px 0 10px;
    border-radius: 10px;
    color: var(--text-primary);
    font-size: 14px;
    cursor: default;
    user-select: none;
    touch-action: none;
  }

  .row:hover {
    background: var(--bg-hover);
  }

  .row.on {
    background: var(--bg-active);
    font-weight: 700;
  }

  /* While a row is being dragged, its resting copy recedes: the pill under the
     cursor is the real one. */
  .row.source {
    opacity: 0.4;
  }

  .row.into {
    background: var(--bg-active);
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .n {
    color: var(--text-secondary);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }

  .n.sub {
    font-size: 11px;
  }

  .twig {
    width: 8px;
    height: 1px;
    background: var(--border);
    flex: none;
    margin-left: -2px;
  }

  /* The delete affordance stays out of the way until the row is hovered, and
     the second click is the confirmation — same two-step as Unpair. */
  .kill {
    border: 0;
    background: none;
    color: var(--text-secondary);
    font-size: 14px;
    line-height: 1;
    padding: 2px 4px;
    border-radius: 6px;
    cursor: pointer;
    opacity: 0;
  }

  .row:hover .kill,
  .kill.armed {
    opacity: 1;
  }

  .kill.armed {
    color: #f4212e;
    font-size: 11px;
  }

  .cat-new,
  .cat-edit {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .cat-new input,
  .cat-edit input {
    background: transparent;
    border: 1px solid var(--border);
    border-radius: 8px;
    color: var(--text-primary);
    padding: 6px 9px;
    font: inherit;
    font-size: 13px;
    min-width: 0;
  }

  .cat-new input {
    flex: 1;
  }

  .cat-edit {
    flex-direction: column;
    padding: 2px 0 4px;
  }

  .cat-edit .edit-criteria {
    font-size: 12px;
    color: var(--text-secondary);
  }

  .edit-actions {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  button.ok {
    border: 1px solid var(--border);
    background: transparent;
    color: inherit;
    border-radius: 9999px;
    padding: 5px 12px;
    font-size: 12px;
    font-weight: 700;
    cursor: pointer;
  }

  button.ok:hover {
    background: var(--bg-hover);
  }

  button.ok:disabled {
    opacity: 0.4;
    cursor: default;
  }

  button.quiet {
    border: 0;
    background: none;
    color: var(--text-secondary);
    font-size: 12px;
    cursor: pointer;
    padding: 4px 0;
  }

  .hint {
    margin: 0;
    color: var(--text-secondary);
    font-size: 11px;
    line-height: 1.4;
  }

  /* ── the goo layer ────────────────────────────────────────────────────── */

  .goo {
    position: absolute;
    inset: 0;
    pointer-events: none;
    border-radius: 16px;
    overflow: hidden;
  }

  .blob {
    position: absolute;
    border-radius: 10px;
    /* Opaque, because the filter works on alpha: a translucent fill would be
       ramped to nothing by the contrast matrix and the shape would vanish.
       Mixed from the text colour so it reads on every theme. */
    background: color-mix(in srgb, var(--text-primary) 10%, var(--bg-elevated));
    transition: transform 120ms ease-out;
  }

  .blob.absorb {
    transform: scale(1.04);
  }

  .blob.floating {
    opacity: 0.92;
  }

  .ins {
    position: absolute;
    height: 2px;
    background: var(--accent);
    border-radius: 2px;
  }

  /* ── classifier controls ───────────────────────────────────────────────── */

  .classify {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 2px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }

  .runrow {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .run {
    flex: 1;
    border: 1px solid var(--border);
    background: transparent;
    color: var(--text-primary);
    border-radius: 9999px;
    padding: 8px 12px;
    font-size: 13px;
    font-weight: 700;
    cursor: pointer;
  }

  .run:hover {
    background: var(--bg-hover);
  }

  .cap {
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    border-radius: 9999px;
    padding: 7px 6px;
    font: inherit;
    font-size: 12px;
    width: 66px;
    text-align: center;
  }

  .run.quiet {
    border-color: transparent;
    color: var(--text-secondary);
    font-weight: 400;
  }

  .again,
  .stop,
  .count {
    border: 0;
    background: none;
    color: var(--text-secondary);
    font-size: 12px;
    padding: 0;
  }

  .again {
    text-align: left;
    text-decoration: underline;
    cursor: pointer;
    align-self: flex-start;
  }

  .stop {
    margin-left: auto;
    cursor: pointer;
    text-decoration: underline;
  }

  .count {
    font-variant-numeric: tabular-nums;
  }

  .bar {
    height: 2px;
    background: var(--border);
    border-radius: 2px;
    overflow: hidden;
  }

  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 220ms linear;
  }

  .note {
    margin: 0;
    color: var(--text-secondary);
    font-size: 11px;
    line-height: 1.4;
  }

  @media (prefers-reduced-motion: reduce) {
    .goo {
      filter: none !important;
    }
    .blob {
      transition: none;
    }
  }
</style>
