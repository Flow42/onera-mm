<!--
  One provider file's declared requirements, as the provider stated them.

  Rendering rules this component exists to hold:

  * an empty requirement list means "requires nothing" only when the answer is
    authoritative — otherwise it is a missing answer and says so;
  * `position` is an opaque ordering key and is never rendered;
  * an empty `game_slug` is a real value meaning "the provider did not say", so
    the unknown status is shown rather than an empty game name;
  * the fingerprint is displayed, because it is the thing an ignore decision is
    scoped to and the user has to have seen it.
-->
<script lang="ts">
  import {
    availabilityCopy,
    candidateLabel,
    candidateStatusCopy,
    candidateTargetCopy,
    dlcCopy,
    groupState,
    isCandidateSelectable,
    requirementKindCopy,
    snapshotSummary,
  } from '$lib/dependency-view';
  import type { DependencyGroup, DependencySnapshot } from '$lib/types';

  interface Props {
    snapshot: DependencySnapshot;
    gameSlug: string;
    /** Offered per blocking group when the caller can record an ignore for it. */
    onIgnore?: (group: DependencyGroup, fingerprint: string) => void;
    /** Offered per blocking group when an accepted risk can be withdrawn. */
    onClearIgnore?: (group: DependencyGroup, fingerprint: string) => void;
    busy?: boolean;
  }

  const { snapshot, gameSlug, onIgnore, onClearIgnore, busy = false }: Props = $props();

  const availability = $derived(availabilityCopy(snapshot.availability));
  const summary = $derived(snapshotSummary(snapshot));
</script>

<section class="panel" data-testid="dependency-detail">
  <h3>Declared requirements</h3>
  <p class={`severity-${availability.severity}`} data-testid="dependency-availability">
    <strong>{availability.label}</strong> — {availability.detail}
  </p>
  <p class={`severity-${summary.severity}`} data-testid="dependency-summary">
    <strong>{summary.label}</strong> — {summary.detail}
  </p>
  <p class="muted">
    Definition fingerprint <code data-testid="dependency-fingerprint">{snapshot.fingerprint}</code>.
    An accepted risk is scoped to this exact definition and stops applying when it changes.
  </p>

  {#each snapshot.groups as group (group.id)}
    {@const kind = requirementKindCopy(group.kind)}
    {@const state = groupState(group, gameSlug)}
    <article class="requirement" data-testid="dependency-group">
      <div class="requirement-heading">
        <strong>{group.label ?? `Requirement ${group.id}`}</strong>
        <span class={`severity-${kind.severity}`}>{kind.label}</span>
        <span class={`severity-${state.severity}`}>{state.label}</span>
      </div>
      <p class="muted">{kind.detail}</p>
      <p class={`severity-${state.severity}`}>{state.detail}</p>
      {#if group.candidates.length > 0}
        <div class="table-scroll">
          <table>
            <thead>
              <tr><th>Candidate</th><th>Status</th><th>Target game</th><th>Selectable</th></tr>
            </thead>
            <tbody>
              {#each group.candidates as candidate, index (`${candidate.provider_file_id ?? 'none'}-${index}`)}
                {@const status = candidateStatusCopy(candidate.status)}
                {@const target = candidateTargetCopy(candidate, gameSlug)}
                <tr>
                  <td>{candidateLabel(candidate)}</td>
                  <td class={`severity-${status.severity}`} title={status.detail}>{status.label}</td
                  >
                  <td class={`severity-${target.severity}`} title={target.detail}>{target.label}</td
                  >
                  <td
                    class={isCandidateSelectable(candidate, gameSlug)
                      ? 'severity-neutral'
                      : 'severity-warning'}
                  >
                    {isCandidateSelectable(candidate, gameSlug) ? 'Yes' : 'No'}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
      {#if kind.blocks && (onIgnore !== undefined || onClearIgnore !== undefined)}
        <div class="group-actions">
          {#if onIgnore !== undefined}
            <button
              class="danger"
              disabled={busy}
              onclick={() => onIgnore(group, snapshot.fingerprint)}
            >
              Ignore this requirement
            </button>
          {/if}
          {#if onClearIgnore !== undefined}
            <button disabled={busy} onclick={() => onClearIgnore(group, snapshot.fingerprint)}>
              Clear accepted risk
            </button>
          {/if}
        </div>
      {/if}
    </article>
  {/each}

  {#if snapshot.dlc.length > 0}
    <h4>Store extras</h4>
    {#each snapshot.dlc as dlc (dlc.id)}
      {@const copy = dlcCopy(dlc)}
      <p class={`severity-${copy.severity}`} data-testid="dependency-dlc">
        <strong>{dlc.label ?? `DLC ${dlc.id}`}</strong> — {copy.label}: {copy.detail}
      </p>
    {/each}
  {/if}
</section>

<style>
  .requirement {
    border-top: 1px solid var(--border, #3a3a3a);
    padding-top: 0.75rem;
    margin-top: 0.75rem;
  }
  .requirement-heading {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    align-items: baseline;
  }
  .group-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .table-scroll {
    overflow-x: auto;
  }
</style>
