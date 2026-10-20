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
  import type { AvatarShape, MediaBlob } from './types';
  import { thumbFor } from './thumbs';

  interface Props {
    name: string;
    handle: string;
    /** X uses 40px in rows and 48px on the focused post. */
    size?: number;
    /** The stored profile picture, when the library holds one. */
    blob?: MediaBlob | null;
    /** Business and brand accounts upload a square logo; X does not round it. */
    shape?: AvatarShape;
  }

  let { name, handle, size = 40, blob = null, shape = 'circle' }: Props = $props();

  let src = $state<string | null>(null);

  // An effect, not a mount hook: a capture can hand this account a picture
  // while the row is already on screen, and the row is patched rather than
  // rebuilt. On `onMount` the newest avatar would wait for a restart.
  $effect(() => {
    const hash = blob?.hash;
    if (!hash) {
      src = null;
      return;
    }
    let live = true;
    thumbFor(hash).then((value) => {
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
  class:square={shape === 'square'}
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

  /* A business account's logo is square because it was uploaded square, so it
     is not cropped into a circle. Percentage radius keeps the corner
     proportional at both 40 and 20 px, and matches X's own rounding (about a
     tenth of the side). */
  .avatar.square {
    border-radius: 12%;
  }

  img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
</style>
