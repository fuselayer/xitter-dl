<script lang="ts">
  /**
   * A profile picture.
   *
   * ## Where the pixels come from
   *
   * `avatarBlob` is filled by the read layer from `media_blobs`, keyed by the
   * account's `avatarUrl` — one file per account, reused by every post it
   * wrote. The bytes arrive as a `data:` URL behind a hash guard, so the webview
   * never opens a connection to an X-owned host and the CSP stays as it is.
   *
   * Without a blob this shows the display-name initial, which is what X itself
   * shows when an avatar fails to load — so the fallback is one X already uses,
   * not an invention. The tooltip says which state you are looking at.
   */
  import { onMount } from 'svelte';
  import type { MediaBlob } from './types';
  import { thumbFor } from './thumbs';

  interface Props {
    name: string;
    handle: string;
    /** X uses 40px in rows and 48px on the focused post. */
    size?: number;
    /** The stored profile picture, when the library holds one. */
    blob?: MediaBlob | null;
  }

  let { name, handle, size = 40, blob = null }: Props = $props();

  let src = $state<string | null>(null);

  onMount(() => {
    if (!blob) return;
    let live = true;
    thumbFor(blob.hash).then((value) => {
      if (live) src = value;
    });
    return () => {
      live = false;
    };
  });

  // X picks the colour from the handle, so it is stable per account rather
  // than changing on every render.
  const PALETTE = [
    '#1d9bf0', '#7856ff', '#ff7a00', '#f91880',
    '#00ba7c', '#e0245e', '#536471', '#ffd400',
  ];

  const initial = $derived((name.trim()[0] ?? handle.trim()[0] ?? '?').toUpperCase());
  const colour = $derived(
    PALETTE[[...handle].reduce((acc, ch) => acc + ch.codePointAt(0)!, 0) % PALETTE.length],
  );
</script>

<div
  class="avatar"
  style="width: {size}px; height: {size}px; background: {colour}; font-size: {Math.round(size * 0.42)}px;"
  title={src ? `@${handle}` : `@${handle} — avatar not archived yet`}
  aria-hidden="true"
>
  {#if src}
    <img src={src} alt="" draggable="false" />
  {:else}
    {initial}
  {/if}
</div>

<style>
  .avatar {
    border-radius: 9999px;
    display: grid;
    place-items: center;
    color: #fff;
    font-weight: 700;
    flex-shrink: 0;
    user-select: none;
    overflow: hidden;
  }

  img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
</style>
