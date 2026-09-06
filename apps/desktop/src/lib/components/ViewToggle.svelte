<!--
  The switch between a list of full-width cards and a grid of square tiles.

  Two radio-shaped buttons rather than one that flips: which shape is showing
  is then readable without having to know what the control would do next.
-->
<script lang="ts">
  import type { ViewMode } from '$lib/view-mode.svelte';

  const {
    mode,
    onchange,
    label = 'Layout',
  }: { mode: ViewMode; onchange: (mode: ViewMode) => void; label?: string } = $props();
</script>

<div class="toggle" role="group" aria-label={label}>
  <button
    type="button"
    class:on={mode === 'list'}
    aria-pressed={mode === 'list'}
    title="Show as a list"
    onclick={() => onchange('list')}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true"
      ><rect x="1" y="2.5" width="14" height="3" rx="1" /><rect
        x="1"
        y="10.5"
        width="14"
        height="3"
        rx="1"
      /></svg
    >
    <span class="sr">List</span>
  </button>
  <button
    type="button"
    class:on={mode === 'tiles'}
    aria-pressed={mode === 'tiles'}
    title="Show as tiles"
    onclick={() => onchange('tiles')}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true"
      ><rect x="1.5" y="1.5" width="5.5" height="5.5" rx="1.2" /><rect
        x="9"
        y="1.5"
        width="5.5"
        height="5.5"
        rx="1.2"
      /><rect x="1.5" y="9" width="5.5" height="5.5" rx="1.2" /><rect
        x="9"
        y="9"
        width="5.5"
        height="5.5"
        rx="1.2"
      /></svg
    >
    <span class="sr">Tiles</span>
  </button>
</div>

<style>
  .toggle {
    display: inline-flex;
    padding: 2px;
    gap: 2px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--surface-sunken);
  }
  button {
    display: grid;
    place-items: center;
    width: 30px;
    height: 26px;
    padding: 0;
    border: 0;
    border-radius: 8px;
    background: transparent;
    color: var(--faint);
  }
  button:hover {
    color: var(--text);
    background: #ffffff0a;
  }
  button.on {
    color: var(--accent-ink);
    background: var(--accent);
  }
  svg {
    width: 15px;
    height: 15px;
    fill: currentColor;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
