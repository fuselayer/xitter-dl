<script lang="ts">
  /**
   * The media grid, laid out as X lays it out.
   *
   * ## Two states per frame, and they must stay distinguishable
   *
   * An item with a `blob` draws the stored thumbnail. An item without one draws
   * a placeholder that says so. "Not archived" and "broken" are different
   * facts, and only one of them is worth fixing.
   *
   * What lands here is the smallest variant X serves: `?name=thumb`, which is a
   * 150×150 crop for a photo and the only poster there is for a video. So a
   * frame that is large will look soft. That is the trade this build makes —
   * a few kilobytes per bookmark rather than the full image — and it is why the
   * grid stays cheap at five hundred posts.
   *
   * Reserving space from `width`/`height` is worth doing regardless — it is
   * what stops X's timeline from jumping as images load, and it is the part of
   * the layout most likely to be got wrong later.
   */
  import type { Media } from './types';
  import Thumb from './Thumb.svelte';

  interface Props {
    media: Media[];
  }

  let { media }: Props = $props();

  /** X caps a single image at 510px tall. */
  const MAX_SINGLE_HEIGHT = 510;

  function aspect(m: Media): string {
    if (m.width && m.height && m.width > 0 && m.height > 0) {
      return `${m.width} / ${m.height}`;
    }
    return '16 / 9'; // X's fallback when dimensions are absent
  }

  /** Clamp a single image's frame so a very tall photo does not dominate. */
  function singleStyle(m: Media): string {
    const a = aspect(m);
    if (m.width && m.height && m.height > 0) {
      const ratio = m.width / m.height;
      const heightAtFullWidth = 1 / ratio;
      if (heightAtFullWidth > 1) {
        // Portrait: cap by height, let width shrink.
        return `aspect-ratio: ${a}; max-height: ${MAX_SINGLE_HEIGHT}px; max-width: ${Math.round(
          MAX_SINGLE_HEIGHT * ratio,
        )}px;`;
      }
    }
    return `aspect-ratio: ${a}; max-height: ${MAX_SINGLE_HEIGHT}px;`;
  }

  const label = $derived((m: Media) => (m.kind === 'photo' ? 'Photo' : m.kind === 'gif' ? 'GIF' : 'Video'));

  /** X's duration pill sits bottom-right on any video or GIF. */
  function duration(m: Media): string | null {
    if (m.kind === 'photo' || !m.durationMs) return null;
    const total = Math.round(m.durationMs / 1000);
    const mins = Math.floor(total / 60);
    const secs = total % 60;
    return `${mins}:${String(secs).padStart(2, '0')}`;
  }
</script>

<div class="media" class:one={media.length === 1} class:two={media.length === 2} class:three={media.length === 3} class:four={media.length >= 4}>
  {#each media as m, i (i)}
    <figure class="cell" style={media.length === 1 ? singleStyle(m) : ''}>
      {#if m.blob}
        <!--
          A stored thumbnail. It is the smallest variant X serves — 150×150 for
          a photo, the poster for a video — so it is soft when the frame is
          large. That is the deliberate trade: a few kilobytes per bookmark
          instead of the full image.
        -->
        <Thumb blob={m.blob} alt={m.altText ?? `${label(m)} (no alt text)`} />
        {#if duration(m)}
          <span class="dur-badge">{duration(m)}</span>
        {/if}
      {:else}
        <div class="placeholder" role="img" aria-label={m.altText ?? `${label(m)} (no alt text)`}>
          <span class="kind">{label(m)}{#if m.durationMs}<span class="dur"> · {Math.round(m.durationMs / 1000)}s</span>{/if}</span>
          <span class="state">not archived yet</span>
        </div>
      {/if}
      {#if m.altText}
        <!-- X shows a small ALT badge on any image that carries alt text. -->
        <span class="alt-badge" title={m.altText}>ALT</span>
      {/if}
    </figure>
  {/each}
</div>

<style>
  .media {
    display: grid;
    gap: 2px; /* X's grid gutter */
    margin-top: 12px;
    border-radius: 16px; /* X's rounded-2xl */
    overflow: hidden;
    border: 1px solid var(--border);
    background: var(--border);
  }

  .media.one {
    display: block;
    border: none;
    background: none;
    width: fit-content;
  }

  .media.two,
  .media.four {
    grid-template-columns: 1fr 1fr;
  }

  .media.three {
    grid-template-columns: 1fr 1fr;
    grid-template-rows: 1fr 1fr;
  }

  /* X's three-up: one tall image on the left, two stacked on the right. */
  .media.three .cell:first-child {
    grid-row: span 2;
  }

  .cell {
    position: relative;
    margin: 0;
    aspect-ratio: 1 / 1;
    background: var(--bg-elevated);
    overflow: hidden;
  }

  .media.one .cell {
    border-radius: 16px;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
  }

  .placeholder {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    color: var(--text-secondary);
    font-size: 13px;
    line-height: 16px;
    text-align: center;
    padding: 8px;
  }

  .kind {
    font-weight: 700;
  }

  .state {
    font-size: 12px;
    opacity: 0.75;
  }

  .alt-badge {
    position: absolute;
    left: 8px;
    bottom: 8px;
    background: rgba(0, 0, 0, 0.75);
    color: #fff;
    font-size: 11px;
    font-weight: 700;
    line-height: 1;
    padding: 3px 5px;
    border-radius: 4px;
    letter-spacing: 0.02em;
  }

  /* X's own treatment for a video or GIF: the running time, bottom right. */
  .dur-badge {
    position: absolute;
    right: 8px;
    bottom: 8px;
    background: rgba(0, 0, 0, 0.75);
    color: #fff;
    font-size: 12px;
    font-weight: 700;
    line-height: 1;
    padding: 3px 5px;
    border-radius: 4px;
    font-variant-numeric: tabular-nums;
  }
</style>
