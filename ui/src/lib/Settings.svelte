<script lang="ts">
  /**
   * Where the classifier is configured.
   *
   * A modal rather than another rail panel because there is nothing live about
   * it: you set it once, test it, and close it. The categories panel is the
   * surface you return to; this is the plumbing behind it.
   *
   * The key field is write-only. Once saved, the backend will only ever say
   * *that* a key exists and its last four characters, so re-opening this panel
   * cannot reveal one — that is the point, not an omission.
   */
  import * as api from './api';
  import type { InferenceView, TestResult } from './types';

  interface Props {
    open: boolean;
    onclose: () => void;
    /** Fired after a save so the panel behind can refresh its counts. */
    onsaved: (view: InferenceView) => void;
  }

  let { open, onclose, onsaved }: Props = $props();

  let view = $state<InferenceView | null>(null);
  let providerId = $state('');
  let baseUrl = $state('');
  let model = $state('');
  /** Requests per minute this account may make. Paced against, not guessed at. */
  let rpm = $state(60);
  let keyDraft = $state('');
  /**
   * Key state of the *selected* provider.
   *
   * `view` describes what is saved; the dropdown can point somewhere else. Two
   * different providers, two different answers, and showing the saved one next
   * to the selected one is how "BeatAPI" came to display the OpenCode hint.
   */
  let keyState = $state<{ hasKey: boolean; hint: string | null } | null>(null);
  const keyInfo = $derived(keyState ?? { hasKey: false, hint: null });
  let clearKey = $state(false);
  let sendImages = $state(true);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let test = $state<TestResult | null>(null);
  let testError = $state<string | null>(null);

  // Load each time it opens: the settings file is the source of truth, and a
  // second window may have changed it.
  $effect(() => {
    if (!open) return;
    error = null;
    test = null;
    testError = null;
    keyDraft = '';
    clearKey = false;
    let alive = true;
    api
      .inferenceConfig()
      .then((v) => {
        if (!alive) return;
        keyState = { hasKey: v.hasKey, hint: v.keyHint };
        view = v;
        providerId = v.provider.id;
        baseUrl = v.provider.baseUrl;
        model = v.provider.model;
        rpm = v.provider.requestsPerMinute;
        sendImages = v.sendImages;
      })
      .catch((e) => alive && (error = String(e)));
    return () => {
      alive = false;
    };
  });

  const preset = $derived(view?.presets.find((p) => p.id === providerId) ?? null);

  function chooseProvider(id: string) {
    providerId = id;
    // A key typed for one provider must never be saved under another: the
    // dropdown is a switch of account, not just of endpoint.
    keyDraft = '';
    clearKey = false;
    keyState = null;
    const chosen = id;
    void api
      .inferenceKeyStatus(chosen)
      .then((status) => {
        if (providerId === chosen) keyState = status;
      })
      .catch(() => {
        /* the panel can still save a key; the hint is only decoration */
      });
    const p = view?.presets.find((x) => x.id === id);
    if (!p) return;
    // Switching providers is a switch of *host and model*, not just a label:
    // leaving the previous model behind is how you test a key against the
    // wrong endpoint and conclude the key is broken.
    if (p.baseUrl) baseUrl = p.baseUrl;
    if (p.model) model = p.model;
    rpm = p.requestsPerMinute;
    test = null;
    testError = null;
  }

  async function save() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const key = clearKey ? '' : keyDraft.trim() ? keyDraft.trim() : null;
      const next = await api.inferenceSave(providerId, baseUrl, model, rpm, key, sendImages);
      view = next;
      keyState = { hasKey: next.hasKey, hint: next.keyHint };
      keyDraft = '';
      clearKey = false;
      onsaved(next);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function runTest() {
    if (busy) return;
    busy = true;
    test = null;
    testError = null;
    error = null;
    try {
      // Test what is *saved*: typing a key and pressing Test without saving
      // would test the previous key and report a false failure.
      if (keyDraft.trim() || clearKey) await save();
      test = await api.inferenceTest();
    } catch (e) {
      testError = String(e);
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window onkeydown={open ? onKeydown : undefined} />

{#if open}
  <!-- The scrim closes on click; the card stops propagation so a click on a
       field does not dismiss the panel mid-edit. -->
  <div
    class="scrim"
    role="presentation"
    onclick={onclose}
  >
    <div
      class="card"
      role="dialog"
      aria-modal="true"
      aria-labelledby="settings-title"
      onclick={(e) => e.stopPropagation()}
    >
      <header>
        <h2 id="settings-title">Classification</h2>
        <button class="x" onclick={onclose} title="Close">×</button>
      </header>

      {#if !view}
        <p class="muted">{error ?? 'Loading…'}</p>
      {:else}
        <p class="lede">
          Posts are filed by asking a JEV model which folder each one belongs in.
          Your key is kept in Windows Credential Manager.
        </p>

        <label class="field">
          <span>Provider</span>
          <select value={providerId} onchange={(e) => chooseProvider(e.currentTarget.value)}>
            {#each view.presets as p (p.id)}
              <option value={p.id}>{p.label}</option>
            {/each}
          </select>
        </label>

        <label class="field">
          <span>Model</span>
          <input bind:value={model} spellcheck="false" placeholder="jev-latest" />
        </label>

        <label class="field">
          <span>Requests per minute</span>
          <input type="number" min="1" max="6000" step="1" bind:value={rpm} />
          <span class="muted">
            {preset?.note ?? 'The run waits between requests to stay under this.'}
          </span>
        </label>

        <label class="field">
          <span>Base URL</span>
          <input bind:value={baseUrl} spellcheck="false" placeholder="https://api.typesafe.ai" />
        </label>

        <label class="field">
          <span>API key</span>
          <input
            type="password"
            bind:value={keyDraft}
            spellcheck="false"
            autocomplete="off"
            placeholder={keyInfo.hasKey && !clearKey
              ? `Saved ${keyInfo.hint ?? ''} — type to replace`
              : 'Paste the key from your provider'}
          />
        </label>

        <div class="keyline">
          {#if clearKey}
            <span class="warn">The key will be removed when you save.</span>
            <button class="link" onclick={() => (clearKey = false)}>Undo</button>
          {:else if keyInfo.hasKey}
            <span class="ok">Key saved {keyInfo.hint ?? ''}</span>
            <button class="link" onclick={() => (clearKey = true)}>Forget key</button>
          {:else}
            <span class="warn">No key saved yet.</span>
          {/if}
        </div>

        <label class="check">
          <input type="checkbox" bind:checked={sendImages} />
          <span>
            Send image posts as images
            <em> — turn off if the model cannot read images; those posts go as text.</em>
          </span>
        </label>

        {#if error}<p class="err">{error}</p>{/if}

        {#if test}
          <p class="ok">
            {test.model} answered in {test.latencyMs} ms
            {test.yes === null ? '' : `(P(test message) = ${test.yes.toFixed(2)})`}
          </p>
        {:else if testError}
          <p class="err">{testError}</p>
        {/if}

        <footer>
          <button class="ghost" onclick={runTest} disabled={busy || (!keyInfo.hasKey && !keyDraft.trim())}>
            Test
          </button>
          <span class="spacer"></span>
          <button class="ghost" onclick={onclose}>Close</button>
          <button class="primary" onclick={save} disabled={busy}>
            {busy ? 'Saving…' : 'Save'}
          </button>
        </footer>

        {@const pending = view.status.pending}
        <p class="foot">
          {view.status.filed} filed · {pending} waiting
          {#if preset?.id === 'custom' && !baseUrl.startsWith('https://')}
            · a custom host must be https unless it is local
          {/if}
        </p>
      {/if}
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 40;
    padding: 24px;
  }

  .card {
    width: min(430px, 100%);
    max-height: 90vh;
    overflow-y: auto;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 20px;
    padding: 18px 20px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.5);
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  h2 {
    margin: 0;
    font-size: 19px;
    line-height: 22px;
    font-weight: 800;
    flex: 1;
  }

  .x {
    border: 0;
    background: none;
    color: var(--text-secondary);
    font-size: 20px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 9999px;
    cursor: pointer;
  }

  .x:hover {
    color: var(--text-primary);
    background: var(--bg-hover);
  }

  .lede {
    margin: 0;
    color: var(--text-secondary);
    font-size: 13px;
    line-height: 1.45;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .field span {
    font-size: 12px;
    color: var(--text-secondary);
  }

  input,
  select {
    background: transparent;
    border: 1px solid var(--border);
    border-radius: 9px;
    color: var(--text-primary);
    padding: 8px 10px;
    font: inherit;
    font-size: 13px;
    min-width: 0;
  }

  input:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
  }

  select {
    background: var(--bg-elevated);
  }

  .keyline {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 12px;
    margin-top: -4px;
  }

  .ok {
    color: var(--accent);
    margin: 0;
    font-size: 12px;
  }

  .warn {
    color: var(--text-secondary);
  }

  .err {
    color: #f4212e;
    font-size: 12px;
    margin: 0;
    line-height: 1.4;
  }

  .link {
    border: 0;
    background: none;
    color: var(--text-secondary);
    font-size: 12px;
    text-decoration: underline;
    padding: 0;
    cursor: pointer;
  }

  .check {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: 13px;
    line-height: 1.4;
  }

  .check input {
    margin-top: 2px;
    accent-color: var(--accent);
  }

  .check em {
    color: var(--text-secondary);
    font-style: normal;
    font-size: 12px;
  }

  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }

  .spacer {
    flex: 1;
  }

  button.ghost,
  button.primary {
    border-radius: 9999px;
    padding: 7px 14px;
    font-size: 13px;
    font-weight: 700;
    cursor: pointer;
    border: 1px solid var(--border);
    background: transparent;
    color: var(--text-primary);
  }

  button.primary {
    background: var(--text-primary);
    color: var(--bg);
    border-color: var(--text-primary);
  }

  button.ghost:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  button:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .foot {
    margin: 0;
    color: var(--text-secondary);
    font-size: 11px;
  }

  .muted {
    color: var(--text-secondary);
    font-size: 13px;
  }
</style>
