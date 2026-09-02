<script lang="ts">
  import { commands } from '$lib/bridge';
  import type { InterruptedOperation } from '$lib/types';
  import { onMount } from 'svelte';

  let operations = $state<InterruptedOperation[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let notice = $state<string | null>(null);

  async function refresh() {
    try {
      operations = await commands.interruptedOperations();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function rollBack(operationId: string) {
    error = null;
    try {
      await commands.rollBack(operationId);
      notice = 'Rollback completed. The previously active profile remains active.';
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(refresh);
</script>

<h1>Interrupted operations</h1>
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}
{#if notice !== null}<p class="severity-neutral" role="status">{notice}</p>{/if}

{#if loading}
  <p class="muted">Checking the operation journal…</p>
{:else if operations.length === 0}
  <p class="muted">Nothing was interrupted. Every operation completed or was rolled back.</p>
{:else}
  <p class="muted">
    Onera found operations that did not finish. Each one's journal records exactly which files were
    already written, so undoing them is safe.
  </p>
  {#each operations as operation (operation.operation_id)}
    <div class="panel">
      <p><strong>{operation.kind}</strong> — {operation.state}</p>
      {#if operation.kind === 'reconcile'}
        <p class="severity-warning">
          This was a profile switch. The target profile is not active until its filesystem changes
          finish and pass verification.
        </p>
      {/if}
      <p class="muted">
        {operation.committed_files} file(s) written, {operation.staged_files} staged, started {operation.created_at}
      </p>
      {#if operation.recovery !== 'None' && operation.recovery !== 'none'}
        <button class="danger" onclick={() => rollBack(operation.operation_id)}>Roll back</button>
      {:else if operation.state === 'failed'}
        <p class="severity-danger">
          Automatic recovery is unavailable. Inspect diagnostics and the game directory before
          launching the game.
        </p>
      {/if}
    </div>
  {/each}
{/if}
