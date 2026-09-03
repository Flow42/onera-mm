<!--
  Whole-profile compatible updates.

  "Update all compatible" solves the entire enabled profile once. It is not a
  list of independently newest per-mod versions, and there is deliberately no
  per-row accept: the solved set is taken as a whole or not at all.

  Loading, empty, incomplete-evidence and failure are four different states with
  four different renderings. None of them is allowed to look like "everything is
  up to date".
-->
<script lang="ts">
  import { BridgeError, commands } from '$lib/bridge';
  import SolvedPlan from '$lib/components/SolvedPlan.svelte';
  import {
    canApplyCompatibleUpdate,
    compatibleUpdateCopy,
    compatibleUpdateReportCopy,
    evidenceIsCurrent,
    isStalePlan,
    stalePlanCopy,
  } from '$lib/dependency-view';
  import type { DependencyActionId } from '$lib/dependency-view';
  import { formatBytes } from '$lib/plan-view';
  import { formatTarget, mutationStepCopy } from '$lib/profile-view';
  import type {
    CompatibleUpdatePreview,
    CompatibleUpdateReport,
    LocalGame,
    Profile,
    ProfileMember,
  } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let profiles = $state<Profile[]>([]);
  let selectedGame = $state<string | null>(null);
  let selectedProfile = $state<string | null>(null);
  let members = $state<ProfileMember[]>([]);
  let preview = $state<CompatibleUpdatePreview | null>(null);
  let report = $state<CompatibleUpdateReport | null>(null);

  let loadingGames = $state(true);
  let checking = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let errorCode = $state<string | null>(null);
  let stalePlan = $state(false);

  const headline = $derived(preview === null ? null : compatibleUpdateCopy(preview));
  const reportCopy = $derived(report === null ? null : compatibleUpdateReportCopy(report.state));

  function showError(value: unknown) {
    errorCode = value instanceof BridgeError ? value.code : 'internal';
    error = value instanceof Error ? value.message : String(value);
  }

  async function selectGame(gameId: string) {
    selectedGame = gameId;
    selectedProfile = null;
    profiles = [];
    members = [];
    preview = null;
    report = null;
    error = null;
    errorCode = null;
    try {
      profiles = await commands.profiles(gameId);
      const active = profiles.find((profile) => profile.is_active) ?? profiles[0];
      if (active !== undefined) await selectProfile(active.id);
    } catch (value) {
      showError(value);
    }
  }

  async function selectProfile(profileId: string) {
    selectedProfile = profileId;
    preview = null;
    report = null;
    try {
      members = await commands.profileMembers(profileId);
    } catch {
      // The member list is only used to name rows; failing to load it must not
      // hide the update result itself.
      members = [];
    }
    await check();
  }

  async function check() {
    if (selectedProfile === null) return;
    checking = true;
    error = null;
    errorCode = null;
    stalePlan = false;
    report = null;
    try {
      preview = await commands.planCompatibleUpdates(selectedProfile);
    } catch (value) {
      preview = null;
      showError(value);
    } finally {
      checking = false;
    }
  }

  async function apply() {
    if (selectedProfile === null || preview === null || !canApplyCompatibleUpdate(preview)) return;
    busy = 'Applying the compatible set';
    error = null;
    errorCode = null;
    stalePlan = false;
    try {
      report = await commands.applyCompatibleUpdates(selectedProfile, preview.fingerprint);
    } catch (value) {
      if (value instanceof BridgeError && isStalePlan(value.code)) {
        // Re-solve first: `check` clears the flag, so it is raised afterwards.
        await check();
        stalePlan = true;
      } else {
        showError(value);
      }
    } finally {
      busy = null;
    }
  }

  function handleAction(action: DependencyActionId) {
    switch (action) {
      case 'install_missing':
      case 'apply_update_set':
      case 'apply_disable_set':
        void apply();
        break;
      case 'replan':
        void check();
        break;
      case 'change_pins':
      case 'ignore_requirement':
        error = null;
        errorCode = null;
        break;
      case 'cancel':
        preview = null;
        break;
      default:
        break;
    }
  }

  onMount(async () => {
    try {
      games = await commands.localGames();
      loadingGames = false;
      const first = games[0];
      if (first !== undefined) await selectGame(first.id);
    } catch (value) {
      showError(value);
    } finally {
      loadingGames = false;
    }
  });
</script>

<header class="page-heading">
  <div>
    <h1>Updates</h1>
    <p class="muted">
      One compatible set solved for the whole enabled profile, not a newest version chosen per mod.
    </p>
  </div>
  {#if games.length > 0}
    <label>
      Installation
      <select
        aria-label="Installation"
        value={selectedGame}
        onchange={(event) => selectGame((event.currentTarget as HTMLSelectElement).value)}
        disabled={busy !== null || checking}
      >
        {#each games as game (game.id)}
          <option value={game.id}>{game.adapter_id} — {game.install_root}</option>
        {/each}
      </select>
    </label>
  {/if}
  {#if profiles.length > 0}
    <label>
      Profile
      <select
        aria-label="Profile"
        value={selectedProfile}
        onchange={(event) => selectProfile((event.currentTarget as HTMLSelectElement).value)}
        disabled={busy !== null || checking}
      >
        {#each profiles as profile (profile.id)}
          <option value={profile.id}>{profile.name}{profile.is_active ? ' (active)' : ''}</option>
        {/each}
      </select>
    </label>
  {/if}
</header>

<p>
  <button onclick={check} disabled={checking || busy !== null || selectedProfile === null}
    >{checking ? 'Checking…' : 'Check again'}</button
  >
</p>

{#if error !== null}
  <p class="error" role="alert" data-error-code={errorCode} data-testid="updates-error">
    {error} The profile was not changed. You can retry when the provider is reachable.
  </p>
{/if}

{#if stalePlan}
  {@const copy = stalePlanCopy()}
  <p class="severity-warning" role="status" data-testid="updates-stale-plan">
    <strong>{copy.label}</strong> — {copy.detail}
  </p>
{/if}

{#if busy !== null}<p aria-live="polite">{busy}…</p>{/if}

{#if loadingGames}
  <p class="muted">Loading game installations…</p>
{:else if games.length === 0}
  <div class="panel empty-state">
    <h2>No game installations</h2>
    <p>Register a game before checking for compatible updates.</p>
    <a href="/games">Go to Games</a>
  </div>
{:else if profiles.length === 0}
  <div class="panel empty-state">
    <h2>No profiles</h2>
    <p>A compatible update is solved for one profile’s enabled members, so this game needs one.</p>
    <a href="/profiles">Go to Profiles</a>
  </div>
{:else if checking}
  <p class="muted" data-testid="updates-loading">Solving the whole profile…</p>
{:else if preview !== null && headline !== null}
  <section class="panel" data-testid="updates-headline">
    <h2 class={`severity-${headline.severity}`}>{headline.label}</h2>
    <p class={`severity-${headline.severity}`}>{headline.detail}</p>
    {#if !evidenceIsCurrent(preview.dependency.evidence)}
      <p class="severity-warning" data-testid="updates-offline">
        Some of this answer came from stored data rather than the provider. Cached results are
        labelled as cached and are never presented as current.
      </p>
    {/if}
    <div class="summary-grid">
      <div>
        <span class="muted">Downloads</span><strong>{preview.downloads.length}</strong>
      </div>
      <div>
        <span class="muted">Filesystem changes</span><strong>{preview.plan.steps.length}</strong>
      </div>
      <div>
        <span class="muted">Bytes to write</span><strong
          >{formatBytes(preview.bytes_to_write)}</strong
        >
      </div>
    </div>
    {#if preview.downloads.length > 0}
      <h3>Downloads</h3>
      <ul>
        {#each preview.downloads as download (download.member_id)}
          <li>
            {download.name} — {download.bytes === null
              ? 'size unknown'
              : formatBytes(download.bytes)}
          </li>
        {/each}
      </ul>
    {/if}
    {#if preview.plan.steps.length > 0}
      <h3>Filesystem changes</h3>
      <table>
        <thead><tr><th>Change</th><th>Target</th><th>Meaning</th></tr></thead>
        <tbody>
          {#each preview.plan.steps as step, index (`${step.kind}-${formatTarget(step.target)}-${index}`)}
            {@const copy = mutationStepCopy(step)}
            <tr>
              <td class={`severity-${copy.severity}`}>{copy.label}</td>
              <td><code>{formatTarget(step.target)}</code></td>
              <td class="muted">{copy.detail}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
    {#if preview.plan.conflicts.length > 0}
      <h3>File conflicts</h3>
      <p class="muted">
        These are path conflicts between mods, decided separately. Accepting a dependency risk never
        chooses a winner for one.
      </p>
      {#each preview.plan.conflicts as conflict (formatTarget(conflict.target))}
        <p class="severity-warning">
          <code>{formatTarget(conflict.target)}</code> — {conflict.providers.length} providers need a
          winner.
        </p>
      {/each}
    {/if}
    {#if preview.blockers.length > 0}
      <h3>Blockers</h3>
      <ul class="severity-danger" data-testid="updates-blockers">
        {#each preview.blockers as blocker, index (`${blocker.kind}-${index}`)}
          <li>
            {blocker.kind.replaceAll('_', ' ')}{blocker.target
              ? ` — ${blocker.target}`
              : ''}{blocker.member_id ? ` — member ${blocker.member_id}` : ''}
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <SolvedPlan
    heading="What the solver proposes for the whole profile"
    result={preview.dependency}
    {members}
    busy={busy !== null}
    onAction={handleAction}
    testId="updates-plan"
  />

  <p>
    <button
      class="primary"
      onclick={apply}
      disabled={busy !== null || !canApplyCompatibleUpdate(preview)}
      data-testid="updates-apply">Update all compatible</button
    >
  </p>
{/if}

{#if reportCopy !== null}
  <section
    class={`panel severity-${reportCopy.severity}`}
    data-testid="updates-result"
    role="status"
  >
    <h2>{reportCopy.label}</h2>
    <p>{reportCopy.detail}</p>
    {#if report?.error !== null && report?.error !== undefined}<p>{report.error}</p>{/if}
  </section>
{/if}

<style>
  .page-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 0.75rem;
    margin-bottom: 1.5rem;
  }
  .page-heading h1,
  .page-heading p {
    margin-top: 0;
    margin-bottom: 0.25rem;
  }
  .summary-grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(8rem, 1fr));
    gap: 0.75rem;
    margin: 1rem 0;
  }
  .summary-grid div {
    display: grid;
    gap: 0.25rem;
  }
  .empty-state {
    text-align: center;
  }
  @media (max-width: 850px) {
    .summary-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
