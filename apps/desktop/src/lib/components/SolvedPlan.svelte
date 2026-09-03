<!--
  The confirmation view for one dependency result.

  Used for an uncommitted desired-state edit, for a whole-profile compatible
  update, and for the dependency section of an activation preview, so all three
  say the same things in the same order.

  It offers only the actions the backend actually solved for, and it deals
  strictly in dependency risk: a filesystem path conflict is a different kind of
  decision, is rendered by its own section, and is never resolved by anything
  clicked here.
-->
<script lang="ts">
  import {
    dependencyHealthCopy,
    dependencyOutcomeCopy,
    evidenceIsCurrent,
    evidenceNotices,
    outcomeActions,
    pinsBlockingSolution,
    proposalChanges,
    requirementExplanation,
  } from '$lib/dependency-view';
  import type { DependencyAction, DependencyActionId } from '$lib/dependency-view';
  import type { ProfileMember, ResolutionResult } from '$lib/types';

  interface Props {
    heading: string;
    result: ResolutionResult;
    members?: ProfileMember[];
    busy?: boolean;
    onAction: (action: DependencyActionId) => void;
    /** Overrides the outcome-derived buttons, e.g. for an uncommitted edit. */
    actions?: DependencyAction[];
    testId?: string;
  }

  const {
    heading,
    result,
    members = [],
    busy = false,
    onAction,
    actions: supplied,
    testId = 'solved-plan',
  }: Props = $props();

  const outcome = $derived(dependencyOutcomeCopy(result));
  const notices = $derived(evidenceNotices(result.evidence));
  const changes = $derived(proposalChanges(result));
  const pinned = $derived(pinsBlockingSolution(result, members));
  const actions = $derived(supplied ?? outcomeActions(result));
  const memberName = (id: string | null) =>
    members.find((member) => member.id === id)?.selection.provider_mod_id ?? id ?? 'a new member';
</script>

<section class="panel" data-testid={testId}>
  <h3>{heading}</h3>
  <p class={`severity-${outcome.severity}`} data-testid={`${testId}-outcome`}>
    <strong>{outcome.label}</strong> — {outcome.detail}
  </p>

  {#if notices.length === 0}
    <p class="muted">Every answer was fetched from the provider just now.</p>
  {:else}
    <div data-testid={`${testId}-evidence`}>
      {#each notices as notice (notice.label)}
        <p class={`severity-${notice.severity}`}>{notice.label}: {notice.detail}</p>
      {/each}
      {#if !evidenceIsCurrent(result.evidence)}
        <p class="severity-warning">
          This answer rests on incomplete data. Cached results are shown as they were stored and are
          not current.
        </p>
      {/if}
    </div>
  {/if}

  {#if changes.length > 0}
    <h4>Proposed changes</h4>
    <p class="muted">
      These were solved together as one set. Accepting them changes desired state only; no file is
      written until the profile is activated.
    </p>
    <div class="table-scroll">
      <table>
        <thead><tr><th>Change</th><th>Mod</th><th>Why</th></tr></thead>
        <tbody>
          {#each changes as change (change.key)}
            <tr data-testid={`${testId}-change`}>
              <td class={`severity-${change.copy.severity}`}>{change.copy.label}</td>
              <td>
                {change.selection === null ? memberName(change.profileMemberId) : change.label}
              </td>
              <td>
                <span class="muted">{change.copy.detail}</span>
                {#each change.because as requirement (requirement.group_id)}
                  {@const explanation = requirementExplanation(requirement)}
                  <span class="because">{explanation.label}: {explanation.detail}</span>
                {/each}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}

  {#if result.health.length > 0}
    <h4>Member health</h4>
    <ul data-testid={`${testId}-health`}>
      {#each result.health as row (row.profile_member_id)}
        {@const copy = dependencyHealthCopy(row.health)}
        <li class={`severity-${copy.severity}`}>
          <strong>{memberName(row.profile_member_id)}</strong> — {copy.label}: {copy.detail}
          {#each row.unsatisfied as requirement (`${requirement.group_id}-${requirement.source.provider_mod_id}`)}
            {@const explanation = requirementExplanation(requirement)}
            <span class="because">{explanation.label}: {explanation.detail}</span>
          {/each}
        </li>
      {/each}
    </ul>
  {/if}

  {#if pinned.length > 0}
    <h4>Pins preventing a solution</h4>
    <ul class="severity-warning" data-testid={`${testId}-pins`}>
      {#each pinned as entry (entry.member.id)}
        <li>
          <strong>{entry.member.selection.provider_mod_id}</strong> is pinned
          {entry.member.pin.kind === 'pinned' && entry.member.pin.reason !== null
            ? `(${entry.member.pin.reason})`
            : ''}, so its version cannot change. Unpin it and solve again to widen the search.
        </li>
      {/each}
    </ul>
  {/if}

  <p class="muted">
    Dependency risk only. A file-path conflict between two mods is a separate decision and is never
    settled by anything on this panel.
  </p>

  <div class="actions" data-testid={`${testId}-actions`}>
    {#each actions as action (action.id)}
      <button
        class={action.primary ? 'primary' : action.destructive ? 'danger' : ''}
        title={action.detail}
        disabled={busy && action.id !== 'cancel'}
        onclick={() => onAction(action.id)}>{action.label}</button
      >
    {/each}
  </div>
</section>

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 1rem;
  }
  .because {
    display: block;
    font-size: 0.8rem;
  }
  .table-scroll {
    overflow-x: auto;
  }
</style>
