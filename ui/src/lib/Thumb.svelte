<script lang="ts">
  /**
   * One stored thumbnail, drawn from the local store.
   *
   * The bytes come back through `media_thumb` as a `data:` URL. The app's CSP
   * allows `data:` and not `asset:`, so this needs no scope entry and no
   * capability: the file read happens in Rust, behind a hash guard.
   *
   * The cache is module-level because a re-render is not a reason to re-encode
   * the same JPEG through IPC, and because the same picture on several posts is
   * one hash. A `null` in the cache means the file was gone when we asked, and
   * is remembered too — asking again on every mount would be a read per frame.
   */
  import { onMount } from 'svelte';
  import type { MediaBlob } from './types';
  import { mediaThumb } from './api';

  interface Props {
    blob: MediaBlob;
    alt: string;
  }

  let { blob, alt }: Props = $props();

  const cache = new Map<string, string | null>();
  let url = $state<string | null>(null);

  onMount(() => {
    const hit = cache.get(blob.hash);
    if (hit !== undefined) {
      url = hit;
      return;
    }
    mediaThumb(blob.hash)
      .then((value) => {
        cache.set(blob.hash, value);
        url = value;
      })
      .catch(() => cache.set(blob.hash, null));
  });
</script>

{#if url}
  <img src={url} {alt} draggable="false" />
{:else}
  <!--
    The frame is already reserved by the cell's aspect ratio, so this costs no
    layout — and it is what a blob whose file was deleted by hand looks like.
  -->
  <div class="pending"></div>
{/if}

<style>
  img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .pending {
    position: absolute;
    inset: 0;
  }
</style>
