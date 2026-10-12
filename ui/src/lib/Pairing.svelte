<script lang="ts">
  /**
   * Connect a browser: show a pairing code, and report what arrives.
   *
   * Rendered as a card in the right rail, in the slot X reserves for exactly
   * this kind of sidebar module.
   *
   * ## Why this component is mounted rather than toggled
   *
   * It is rendered only while the panel is open, so mounting *is* "show the
   * code" and unmounting *is* "hide the code". That mapping is deliberate: the
   * backend invalidates the code when the panel closes, because a code that
   * stays valid while off screen is a code nobody is watching. A CSS `hidden`
   * toggle would quietly break that guarantee.
   *
   * ## Why a paired bridge can still show a code
   *
   * One secret serves every browser that redeemed a code, so being paired is
   * not a reason to hide the code — it is the reason someone might want a
   * second one. Opening this panel on an already-paired bridge used to show a
   * dead end and nothing else, which made "connect my laptop too" look
   * identical to "something is wrong". Both states now lead somewhere.
   */

  import { onMount } from 'svelte';
  import * as api from './api';

  let { onclose }: { onclose: () => void } = $props();

  let info = $state<api.PairingInfo | null>(null);
  let error = $state<string | null>(null);
  let secondsLeft = $state(0);
  let lastCapture = $state<api.CaptureReport | null>(null);
  let note = $state<string | null>(null);

  // Whether the code block is on screen. Starts true only once we know the
  // bridge is unpaired — showing a code to someone who is already connected
  // answers a question they did not ask.
  let showCode = $state(false);

  // The TTL the current code started with, so the drain bar below it has a
  // denominator. A panel opened mid-rotation sees a slightly generous bar for
  // one cycle, which is harmless; the next rotation corrects it.
  let codeTtl = $state(1);

  let codeCopied = $state(false);

  // "Unpair every browser" is the most destructive thing in the app, so it
  // takes two clicks: the first arms the button, the second fires, and four
  // seconds of inaction disarms it again.
  let confirmUnpair = $state(false);
  let confirmTimer: ReturnType<typeof setTimeout> | undefined;

  const ttlPct = $derived(
    codeTtl > 0 ? Math.max(0, Math.min(100, (secondsLeft / codeTtl) * 100)) : 0,
  );

  onMount(() => {
    let alive = true;
    let timer: ReturnType<typeof setInterval> | undefined;

    const apply = (next: api.PairingInfo) => {
      info = next;
      secondsLeft = next.secondsRemaining;
      codeTtl = Math.max(next.secondsRemaining, 1);
      if (!next.paired) showCode = true;
    };

    const rotate = async () => {
      try {
        const next = await api.pairingShow();
        if (alive) apply(next);
      } catch (e) {
        if (alive) error = String(e);
      }
    };

    void rotate();

    timer = setInterval(() => {
      if (!alive || !showCode) return;
      secondsLeft -= 1;
      if (secondsLeft <= 0) void rotate();
    }, 1000);

    let off: (() => void) | undefined;
    void api
      .onBridgeEvent((event) => {
        if (!alive) return;
        if (event.type === 'paired') {
          note = event.label ? `Paired with ${event.label}.` : 'Paired.';
          // A successful exchange means the code did its job; get it off the
          // screen rather than leaving a spent code sitting there.
          showCode = false;
          void api.pairingStatus().then(apply).catch(() => {});
        } else if (event.type === 'captured') {
          lastCapture = event;
          note = null;
        } else if (event.type === 'rejected') {
          note = `A request was refused: ${event.reason}`;
        }
      })
      .then((f) => {
        if (alive) off = f;
        else f();
      })
      .catch((e) => {
        if (alive) error = String(e);
      });

    return () => {
      alive = false;
      clearInterval(timer);
      clearTimeout(confirmTimer);
      off?.();
      // Invalidate the code the moment the panel goes away.
      void api.pairingHide().catch(() => {});
    };
  });

  async function addBrowser() {
    showCode = true;
    error = null;
    try {
      const next = await api.pairingShow();
      info = next;
      secondsLeft = next.secondsRemaining;
      codeTtl = Math.max(next.secondsRemaining, 1);
    } catch (e) {
      error = String(e);
    }
  }

  async function copyCode() {
    if (!info?.code) return;
    try {
      await navigator.clipboard.writeText(info.code);
      codeCopied = true;
      setTimeout(() => (codeCopied = false), 1600);
    } catch {
      // Clipboard denied: the code is still on screen and selectable, so the
      // button failing quietly costs nothing.
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
      info = await api.pairingRevoke();
      secondsLeft = info.secondsRemaining;
      codeTtl = Math.max(info.secondsRemaining, 1);
      lastCapture = null;
      showCode = true;
      note = 'Every paired browser must pair again before it can send anything.';
    } catch (e) {
      error = String(e);
    }
  }
</script>

<section class="pairing">
  <header>
    <h3>Connect your browser</h3>
    <button class="x" onclick={onclose} title="Close">×</button>
  </header>

  {#if error}
    <p class="bad">{error}</p>
  {/if}

  {#if info && info.port === 0}
    <p class="bad">
      The capture receiver is not running, so no browser can connect. Something
      stopped it binding a loopback port at startup.
    </p>
  {:else if info}
    {#if info.paired}
      <p class="state ok">
        <span class="dot ok"></span>
        A secret was issued{#if info.pairedLabel} to {info.pairedLabel}{/if}{#if info.pairedAt}
          on {new Date(info.pairedAt * 1000).toLocaleDateString()}{/if}.
      </p>
      {#if !showCode}
        <p class="hint">
          Pairing is per browser: the secret lives in that browser's userscript
          storage, and this panel can only know that one was handed out — not
          that a browser still holds it. If the pill on x.com says it is not
          paired, use <strong>Add another browser</strong> rather than unpairing.
          That issues a fresh code and leaves the browsers that already work alone.
        </p>
        <div class="row">
          <button class="ghost" onclick={addBrowser}>Add another browser</button>
          <button class="danger" class:armed={confirmUnpair} onclick={unpairClick}>
            {confirmUnpair ? 'Click again to confirm' : 'Unpair every browser'}
          </button>
        </div>
      {/if}
    {:else}
      <p class="state">
        <span class="dot warn"></span>
        Waiting for a browser to pair.
      </p>
    {/if}

    {#if showCode}
      {#if info.paired}
        <p class="hint">
          This code connects an <strong>additional</strong> browser. Browsers
          already paired keep working.
        </p>
      {:else}
        <p class="hint">
          On <code>x.com/i/bookmarks</code>, click the pill in the bottom-right
          corner of the page and type this code. You only do this once per browser.
        </p>
      {/if}
      <div class="code-row">
        <div class="code">{info.formattedCode ?? '————-————'}</div>
        {#if info.code}
          <button class="ghost small" onclick={copyCode}>{codeCopied ? 'Copied' : 'Copy'}</button>
        {/if}
      </div>
      <div class="ttl" aria-hidden="true">
        <div class="ttl-fill" style="width: {ttlPct}%"></div>
      </div>
      <p class="count">
        {#if secondsLeft > 0}
          New code in {secondsLeft}s. It rotates on its own — read the current one.
        {:else}
          Rotating…
        {/if}
      </p>
    {/if}

    <p class="addr">
      Receiver <code>{info.address}</code>
    </p>
  {:else}
    <p class="hint">Asking the backend…</p>
  {/if}

  {#if lastCapture}
    <p class="capture">
      Last batch: {lastCapture.seen} seen, {lastCapture.new} new,
      {lastCapture.updated} already held.
      {#if lastCapture.problems.length}
        <span class="bad">{lastCapture.problems.length} could not be read.</span>
      {/if}
    </p>
  {/if}

  {#if note}
    <p class="hint">{note}</p>
  {/if}
</section>

<style>
  /* A right-rail card, matching the other panels in the sidebar. */
  .pairing {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 12px 16px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  h3 {
    margin: 0;
    font-size: 17px;
    line-height: 20px;
    font-weight: 800;
  }
  .x {
    margin-left: auto;
    border: 0;
    background: transparent;
    color: var(--text-secondary);
    font-size: 20px;
    line-height: 1;
    cursor: pointer;
  }
  .x:hover {
    color: var(--text-primary);
  }
  p {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
  }
  .hint,
  .count,
  .addr,
  .capture {
    color: var(--text-secondary);
    font-size: 12px;
  }
  .state {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
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
  }
  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .code-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  /* Large, monospace and widely tracked: this is read off the screen and typed
     into a different window, which is exactly the job letter-spacing is for. */
  .code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 30px;
    font-weight: 700;
    letter-spacing: 0.14em;
    padding: 8px 0 4px;
    user-select: all;
  }
  /* The code's remaining lifetime, draining left to right. The countdown text
     below says the same thing; the bar just says it at a glance. */
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
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.95em;
    opacity: 0.85;
  }
  button.danger,
  button.ghost {
    border-radius: 9999px;
    padding: 6px 14px;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
    background: transparent;
  }
  button.ghost.small {
    padding: 4px 12px;
    font-size: 12px;
  }
  button.danger {
    border: 1px solid #f4212e;
    color: #f4212e;
  }
  button.danger:hover,
  button.danger.armed {
    background: rgba(244, 33, 46, 0.1);
  }
  button.ghost {
    border: 1px solid var(--border);
    color: inherit;
  }
  button.ghost:hover {
    background: var(--bg-hover);
  }
</style>
