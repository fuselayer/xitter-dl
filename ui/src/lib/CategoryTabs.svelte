<script lang="ts">
  /**
   * The folder strip, where X puts its tabs.
   *
   * Folders are a place you *are*, not a command you run: the strip is always
   * visible above the feed, the open one is bold with the accent underline, and
   * clicking a name is a filter change. The right-rail panel keeps the editing
   * — nesting, renaming, deleting, filing — because that is the part that needs
   * a tree and a drag target.
   *
   * ## Top-level only, plus wherever you are
   *
   * A tab strip cannot indent, so it shows top-level folders; a parent already
   * includes everything filed in its children. When a child is open (chosen in
   * the rail), it gets a tab of its own at the end — otherwise the strip would
   * claim nothing is selected while the feed shows a folder.
   */
  import type { Category } from './types';
  import * as api from './api';

  interface Props {
    active: string | null;
    /** Bumped by the rail panel after any edit, so the strip re-reads. */
    revision: number;
    onpick: (id: string | null, label: string | null) => void;
    /** "New folder" — the strip's `+` opens the same form as the rail's. */
    onadd: () => void;
  }

  let { active, revision, onpick, onadd }: Props = $props();

  let cats = $state<Category[]>([]);
  let strip = $state<HTMLElement | null>(null);
  const tabs: Record<string, HTMLButtonElement> = {};

  $effect(() => {
    void revision;
    let alive = true;
    api
      .categoriesList()
      .then((list) => alive && (cats = list))
      .catch(() => {
        /* a strip is not worth an error banner; the panel reports */
      });
    return () => {
      alive = false;
    };
  });

  const roots = $derived(
    cats.filter((c) => !c.parentId).sort((a, b) => a.position - b.position),
  );
  const current = $derived(active ? (cats.find((c) => c.id === active) ?? null) : null);
  /** A nested folder, shown as a trailing tab while it is the open one. */
  const nested = $derived(current && current.parentId ? current : null);

  // Keep the open tab reachable without touching vertical scroll: a plain
  // `scrollIntoView` would also scroll the feed, which is not what changed.
  $effect(() => {
    const id = active;
    if (!id || !strip) return;
    const el = tabs[id];
    if (!el) return;
    const box = strip.getBoundingClientRect();
    const me = el.getBoundingClientRect();
    if (me.left < box.left) strip.scrollLeft += me.left - box.left - 16;
    else if (me.right > box.right) strip.scrollLeft += me.right - box.right + 16;
  });

  function choose(id: string | null, label: string | null) {
    onpick(id, label);
  }
</script>

<nav class="tabs" bind:this={strip} aria-label="Folders">
  <button
    class="tab"
    class:on={active === null}
    aria-current={active === null ? 'page' : undefined}
    onclick={() => choose(null, null)}
  >
    All bookmarks
  </button>

  {#each roots as cat (cat.id)}
    <button
      class="tab"
      class:on={active === cat.id}
      aria-current={active === cat.id ? 'page' : undefined}
      bind:this={tabs[cat.id]}
      onclick={() => choose(cat.id, cat.label)}
    >
      {cat.label}
    </button>
  {/each}

  {#if nested}
    <button
      class="tab"
      class:on={true}
      aria-current="page"
      bind:this={tabs[nested.id]}
      onclick={() => choose(nested.id, nested.label)}
    >
      {nested.label}
    </button>
  {/if}

  <button class="add" onclick={onadd} title="New folder" aria-label="New folder">+</button>
</nav>

<style>
  .tabs {
    display: flex;
    align-items: stretch;
    gap: 28px;
    padding: 0 16px;
    overflow-x: auto;
    overscroll-behavior-x: contain;
    scrollbar-width: none;
    border-bottom: 1px solid var(--border);
  }

  .tabs::-webkit-scrollbar {
    display: none;
  }

  .tab {
    position: relative;
    border: 0;
    background: none;
    font: inherit;
    font-size: 15px;
    line-height: 20px;
    font-weight: 500;
    color: var(--text-secondary);
    padding: 15px 0 14px;
    white-space: nowrap;
    cursor: pointer;
    flex: none;
  }

  .tab:hover {
    color: var(--text-primary);
  }

  .tab.on {
    color: var(--text-primary);
    font-weight: 800;
  }

  /* X's indicator: a short rounded bar under the word, not a full-width rule. */
  .tab.on::after {
    content: '';
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 4px;
    border-radius: 9999px;
    background: var(--accent);
  }

  .add {
    border: 0;
    background: none;
    color: var(--text-secondary);
    font: inherit;
    font-size: 19px;
    line-height: 1;
    padding: 0 2px;
    margin-left: -14px;
    cursor: pointer;
    flex: none;
    align-self: center;
  }

  .add:hover {
    color: var(--text-primary);
  }
</style>
