<script lang="ts">
  /**
   * One stored thumbnail, drawn from the local store.
   *
   * The bytes come back through `media_thumb` as a `data:` URL. The app's CSP
   * allows `data:` and not `asset:`, so this needs no scope entry and no
   * capability: the file read happens in Rust, behind a hash guard.
   *
   * The cache lives in `thumbs.ts` rather than here, so an avatar and a media
   * grid showing the same file cost one IPC round trip between them.
   */
  import { onMount } from 'svelte';
  import type { MediaBlob } from './types';
  import { thumbFor } from './thumbs';

  interface Props {
    blob: MediaBlob;
    alt: string;
  }

  let { blob, alt }: Props = $props();

  let url = $state<string | null>(null);

  onMount(() => {
    let live = true;
    thumbFor(blob.hash).then((value) => {
      if (live) url = value;
    });
    return () => {
      live = false;
    };
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
