<!--
  A list of cards in one of its two shapes.

  The cards themselves are written by the page — every list has different
  content — so this only carries the layout and the accessible name. The card
  styles are global (`app.css`) for the same reason: a scoped rule here would
  not reach content the page rendered.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { ViewMode } from '$lib/view-mode.svelte';

  const { mode, label, children }: { mode: ViewMode; label?: string; children: Snippet } = $props();
</script>

<ul class="cards" class:rows={mode === 'list'} class:tiles={mode === 'tiles'} aria-label={label}>
  {@render children()}
</ul>
