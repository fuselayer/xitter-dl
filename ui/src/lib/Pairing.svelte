<script lang="ts">
  /**
   * The connection state, in the left rail.
   *
   * It answers one question — *is a browser sending me things right now* — and
   * it used to answer it wrongly, because "a secret was issued on 12 October" is
   * history, not presence. `lastCaptureAt` from the backend is the presence
   * signal: the bridge stamps it every time a batch lands.
   *
   * ## When the code shows
   *
   * `showCode` *is* the request: opening it calls `pairingShow`, closing it calls
   * `pairingHide`, and the backend invalidates the code on hide. A code that
   * stayed valid while off screen would be a code nobody is watching. That is
   * why this is state and not a CSS toggle.
   *
   * It is only offered when it is useful: someone unpaired needs it, someone
   * connected and active does not, and a paired bridge that has gone quiet is
   * usually a browser that was closed — which also wants a code.
   */

  import { onMount } from 'svelte';
  import * as api from './api';

  /** A counter the app bumps to ask for the code from elsewhere in the window. */
  let { openSignal = 0 }: { openSignal?: number } = $props();

  let info = $state<api.PairingInfo | null>(null);
  let error = $state<string | null>(null);
  let secondsLeft = $state(0);
  let showCode = $state(false);
  let codeCopied = $state(false);
  let codeTtl = $state(1);
  let now = $state(Math.floor(Date.now() / 1000));

  // Two clicks for unpair: the first arms it, four seconds of inaction disarms.
  let confirmUnpair = $state(false);
  let confirmTimer: ReturnType<typeof setTimeout> | undefined;

  /** Active means bytes arrived recently, not that a secret exists. */
  const ACTIVE_WINDOW = 180;

  const active = $derived(
    info?.lastCaptureAt != null && now - info.lastCaptureAt < ACTIVE_WINDOW,
  );
  const paired = $derived(info?.paired ?? false);
  const ttlPct = $derived(
    codeTtl > 0 ? Math.max(0, Math.min(100, (secondsLeft / codeTtl) * 100)) : 0,
  );

  /** "12s ago", "4m ago", "3h ago" — the shift from presence to history. */
  function ago(at: number | null): string {
    if (at == null) return 'never';
    const d = Math.max(0, now - at);
    if (d < 60) return `${d}s ago`;
    if (d < 3600) return `${Math.floor(d / 60)}m ago`;
    return `${Math.floor(d / 3600)}h ago`;
  }

  function apply(next: api.PairingInfo) {
    info = next;
    secondsLeft = next.secondsRemaining;
    codeTtl = Math.max(next.secondsRemaining, 1);
  }

  onMount(() => {
    let alive = true;
    let off: (() => void) | undefined;

    void api
      .pairingStatus()
      .then((next) => alive && apply(next))
      .catch((e) => alive && (error = String(e)));

    // One poll covers everything that changes without an event: the code's
    // remaining life, the "ago" label, and a capture that landed before the
    // event subscription finished.
    const timer = setInterval(() => {
      if (!alive) return;
      now = Math.floor(Date.now() / 1000);
      if (!showCode) {
        void api
          .pairingStatus()
          .then((next) => alive && apply(next))
          .catch(() => {});
        return;
      }
      secondsLeft -= 1;
      if (secondsLeft <= 0) void openCode();
    }, 1000);

    void api
      .onBridgeEvent((event) => {
        if (!alive) return;
        if (event.type === 'paired') {
          // The code did its job; get it off the screen.
          showCode = false;
          void api
            .pairingStatus()
            .then((next) => alive && apply(next))
            .catch(() => {});
        } else if (event.type === 'captured') {
          now = Math.floor(Date.now() / 1000);
          if (info) info = { ...info, lastCaptureAt: now };
        }
      })
      .then((f) => {
        if (alive) off = f;
        else f();
      })
      .catch(() => {});

    return () => {
      alive = false;
      clearInterval(timer);
      clearTimeout(confirmTimer);
      off?.();
      // A code left valid behind a collapsed panel is a code nobody is watching.
      if (showCode) void api.pairingHide().catch(() => {});
    };
  });

  async function openCode() {
    showCode = true;
    error = null;
    try {
      apply(await api.pairingShow());
    } catch (e) {
      error = String(e);
    }
  }

  // The empty state and any future affordance open the code by bumping a
  // counter, so they do not need to reach inside this component.
  let seenSignal = 0;
  $effect(() => {
    if (openSignal === seenSignal) return;
    seenSignal = openSignal;
    void openCode();
  });

  async function closeCode() {
    showCode = false;
    try {
      apply(await api.pairingHide());
    } catch {
      /* the panel is closing; a failure here changes nothing visible */
    }
  }

  async function copyCode() {
    if (!info?.code) return;
    try {
      await navigator.clipboard.writeText(info.code);
      codeCopied = true;
      setTimeout(() => (codeCopied = false), 1600);
    } catch {
      // Clipboard denied: the code is on screen and selectable, so this failing
      // quietly costs nothing.
    }
  }

  function unpairClick() {
    if (!confirmUnpair) {
      confirmUnpair = true;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirmUnpair = false), 4000);
      return;
    }
    confirmUnpair = false;
    clearTimeout(confirmTimer);
    void unpair();
  }

  async function unpair() {
    try {
      apply(await api.pairingRevoke());
      void openCode();
    } catch (e) {
      error = String(e);
    }
  }
</script>

<section class="conn">
  {#if error}
    <p class="bad">{error}</p>
  {/if}

  {#if info && info.port === 0}
    <p class="bad">The capture receiver is not running — no browser can connect.</p>
  {:else if !info}
    <p class="meta">Asking the backend…</p>
  {:else}
    {#if active}
      <p class="state ok">
        <span class="dot ok"></span>
        Connected
      </p>
      <p class="meta">{info.pairedLabel ?? 'a browser'} · {ago(info.lastCaptureAt)}</p>
    {:else if paired}
      <p class="state">
        <span class="dot warn"></span>
        No browser active
      </p>
      <p class="meta">Paired {ago(info.pairedAt)} · nothing arriving</p>
    {:else}
      <p class="state">
        <span class="dot"></span>
        Not connected
      </p>
    {/if}

    {#if showCode}
      <div class="code-row">
        <div class="code">{info.formattedCode ?? '——'}</div>
        {#if info.code}
          <button class="ghost small" onclick={copyCode}>{codeCopied ? 'Copied' : 'Copy'}</button>
        {/if}
      </div>
      <div class="ttl" aria-hidden="true">
        <div class="ttl-fill" style="width: {ttlPct}%"></div>
      </div>
      <p class="meta">
        {#if secondsLeft > 0}New code in {secondsLeft}s{:else}Rotating…{/if} · type it
        into the pill on x.com
      </p>
      <p class="meta">Receiver {info.address}</p>
      <button class="ghost small" onclick={closeCode}>Done</button>
    {:else if active}
      <div class="row">
        <button class="ghost small" onclick={openCode}>Add browser</button>
        <button class="quiet" class:armed={confirmUnpair} onclick={unpairClick}>
          {confirmUnpair ? 'Sure?' : 'Unpair'}
        </button>
      </div>
    {:else}
      <button class="primary wide" onclick={openCode}>Connect browser</button>
      {#if paired}
        <button class="quiet" class:armed={confirmUnpair} onclick={unpairClick}>
          {confirmUnpair ? 'Sure?' : 'Unpair'}
        </button>
      {/if}
    {/if}
  {/if}
</section>

<style>
  .conn {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  p {
    margin: 0;
  }
  .state {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 14px;
    font-weight: 700;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex: none;
    background: var(--text-secondary);
  }
  .dot.ok {
    background: var(--repost);
  }
  .dot.warn {
    background: #ffd400;
  }
  .ok {
    color: var(--repost);
  }
  .bad {
    color: #f4212e;
    font-size: 12px;
    line-height: 1.4;
  }
  .meta {
    color: var(--text-secondary);
    font-size: 12px;
    line-height: 1.4;
    overflow-wrap: anywhere;
  }
  .code-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  /* Read off this screen and typed into another window, so tracking earns its
     keep even at rail width. */
  .code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 19px;
    font-weight: 700;
    letter-spacing: 0.1em;
    user-select: all;
  }
  .ttl {
    height: 3px;
    border-radius: 2px;
    background: var(--bg-active);
    overflow: hidden;
  }
  .ttl-fill {
    height: 100%;
    background: var(--accent);
    transition: width 1s linear;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  button.primary.wide {
    width: 100%;
  }
  button.ghost {
    border: 1px solid var(--border);
    background: transparent;
    color: inherit;
    border-radius: 9999px;
    padding: 6px 14px;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }
  button.ghost.small {
    padding: 4px 11px;
    font-size: 12px;
  }
  button.ghost:hover {
    background: var(--bg-hover);
  }
  /* Unpair is a text link, not a button: the destructive action should not
     compete with the one thing this panel is for. */
  button.quiet {
    border: 0;
    background: none;
    padding: 4px 0;
    color: var(--text-secondary);
    font-size: 12px;
    cursor: pointer;
    text-align: left;
  }
  button.quiet:hover,
  button.quiet.armed {
    color: #f4212e;
  }
</style>
