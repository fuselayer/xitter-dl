<script lang="ts">
  /**
   * A search snippet with its matches highlighted.
   *
   * The snippet arrives from Rust's FTS5 `snippet()` as a single string with
   * matches wrapped in `U+0001` / `U+0002`. Those are control characters
   * precisely so that this component can split on them and build real DOM
   * nodes — rather than the alternative, which is to have Rust emit `<mark>`
   * tags and have the frontend `innerHTML` them. That alternative turns any
   * tweet containing markup into script execution.
   *
   * So: no `{@html}` anywhere in this app, and no HTML parsing near user data.
   */
  import { SNIPPET_CLOSE, SNIPPET_OPEN } from './snippet';

  interface Props {
    snippet: string;
  }

  let { snippet }: Props = $props();

  type Part = { text: string; match: boolean };

  const parts = $derived.by((): Part[] => {
    const out: Part[] = [];
    let rest = snippet;
    while (rest.length > 0) {
      const open = rest.indexOf(SNIPPET_OPEN);
      if (open < 0) {
        out.push({ text: rest, match: false });
        break;
      }
      if (open > 0) out.push({ text: rest.slice(0, open), match: false });

      const close = rest.indexOf(SNIPPET_CLOSE, open + 1);
      if (close < 0) {
        // Unterminated marker — render the remainder plainly rather than
        // swallowing it.
        out.push({ text: rest.slice(open + 1), match: false });
        break;
      }
      out.push({ text: rest.slice(open + 1, close), match: true });
      rest = rest.slice(close + 1);
    }
    return out;
  });
</script>

<div class="snippet">
  <span class="tag">Match</span>
  <span class="context">
    {#each parts as part, i (i)}{#if part.match}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}
  </span>
</div>

<style>
  .snippet {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    /* 68px = the tweet's 16px padding + 40px avatar + 12px gap, so the match
       context sits in the same text column as the tweet it belongs to rather
       than floating against the card's left edge. */
    padding: 12px 16px 0 68px;
    font-size: 13px;
    line-height: 17px;
    color: var(--text-secondary);
  }

  .tag {
    flex: none;
    margin-top: 1px;
    font-size: 11px;
    line-height: 1;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--accent);
    background: rgba(29, 155, 240, 0.1);
    padding: 3px 6px;
    border-radius: 4px;
  }

  /* Two lines of context, then an ellipsis — a hit should orient the eye,
     not pre-empt the tweet it sits above. */
  .context {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: break-word;
  }

  mark {
    background: rgba(29, 155, 240, 0.18);
    color: var(--accent);
    border-radius: 2px;
    padding: 0 1px;
  }
</style>
