<script lang="ts">
  import { page } from '$app/state';
  import { commands, onProgress } from '$lib/bridge';
  import {
    CONFLICT_CHOICES,
    DECISION_SCOPES,
    canApply,
    describe,
    formatBytes,
    summarise,
    unresolved,
  } from '$lib/plan-view';
  import { fraction, initial, reduce } from '$lib/progress.svelte';
  import type { InstallPlanView } from '$lib/types';
  import { onMount } from 'svelte';

  let plan = $state<InstallPlanView | null>(null);
  let progress = $state(initial());
  let error = $state<string | null>(null);
  let applied = $state(false);
  let scope = $state<string>('this_file');

  // `onMount` must return its cleanup synchronously; the subscription and the
  // preparation both run in the background and are torn down through a handle.
  onMount(() => {
    let stop: (() => void) | undefined;
    void onProgress((event) => {
      progress = reduce(progress, event);
    }).then((unlisten) => {
      stop = unlisten;
    });

    void (async () => {
      try {
        plan = await commands.prepareInstall({
          gameId: page.url.searchParams.get('game') ?? '',
          gameDomain: page.url.searchParams.get('domain') ?? '',
          modId: page.url.searchParams.get('mod') ?? '',
          fileId: page.url.searchParams.get('file') ?? '',
        });
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      }
    })();

    return () => stop?.();
  });

  async function decide(target: string, choice: string) {
    if (plan === null) return;
    plan = await commands.decide(plan.operation_id, target, choice, scope);
  }

  async function apply() {
    if (plan === null) return;
    try {
      await commands.applyPlan(plan.operation_id);
      applied = true;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function cancel() {
    if (plan !== null) await commands.cancelOperation(plan.operation_id);
  }
</script>

<h1>Installation preview</h1>
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

{#if plan === null}
  <div class="panel">
    <p>{progress.stage}{progress.detail === null ? '' : `: ${progress.detail}`}</p>
    {#if fraction(progress) === null}
      <progress></progress>
    {:else}
      <progress value={fraction(progress)}></progress>
    {/if}
    <p><button onclick={cancel}>Cancel</button></p>
  </div>
{:else if applied}
  <div class="panel"><p>Installed. <a href="/mods">Back to installed mods</a></p></div>
{:else}
  <div class="panel">
    <p><strong>{plan.mod_name}</strong> — {plan.layout_rationale}</p>
    <p class="muted">
      {plan.files.length} file(s), {formatBytes(plan.bytes_to_write)} to write,
      {plan.ignored} ignored.
    </p>
    {#each summarise(plan) as row (row.classification)}
      <span class={`severity-${describe(row.classification).severity}`}>
        {describe(row.classification).label}: {row.count}
      </span>
    {/each}
    {#if plan.rejected.length > 0}
      <h2>Rejected archive entries</h2>
      <ul>
        {#each plan.rejected as entry (entry.raw_path)}<li class="severity-warning">
            {entry.raw_path}: {entry.reason}
          </li>{/each}
      </ul>
    {/if}
  </div>

  {#if unresolved(plan).length > 0}
    <h2>Decisions needed ({unresolved(plan).length})</h2>
    <p>
      <label for="scope">Apply my choice to</label>
      <select id="scope" bind:value={scope}>
        {#each DECISION_SCOPES as option (option.id)}<option value={option.id}
            >{option.label}</option
          >{/each}
      </select>
    </p>
    {#each unresolved(plan) as file (file.target)}
      <div class="panel">
        <p><code>{file.target}</code></p>
        <p class={`severity-${describe(file.classification).severity}`}>
          {describe(file.classification).label} — {describe(file.classification).detail}
        </p>
        {#each file.notes as note (note)}<p class="muted">{note}</p>{/each}
        {#each CONFLICT_CHOICES as choice (choice.id)}
          <button
            class={choice.destructive ? 'danger' : ''}
            onclick={() => decide(file.target, choice.id)}
          >
            {choice.label}
          </button>
        {/each}
      </div>
    {/each}
  {/if}

  <h2>Files</h2>
  <table>
    <thead><tr><th>Target</th><th>What happens</th><th>Source</th></tr></thead>
    <tbody>
      {#each plan.files as file (file.target)}
        <tr>
          <td><code>{file.target}</code></td>
          <td class={`severity-${describe(file.classification).severity}`}
            >{describe(file.classification).label}</td
          >
          <td class="muted">{file.source}</td>
        </tr>
      {/each}
    </tbody>
  </table>

  <p>
    <button class="primary" onclick={apply} disabled={!canApply(plan)}>Install</button>
    <a href="/mods">Cancel</a>
  </p>
{/if}
