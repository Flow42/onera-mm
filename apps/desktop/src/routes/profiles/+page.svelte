<script lang="ts">
  import { page } from '$app/state';
  import { freshnessCopy } from '$lib/baseline-view';
  import { BridgeError, commands, onProgress } from '$lib/bridge';
  import { formatBytes } from '$lib/plan-view';
  import {
    activationCopy,
    canActivate,
    dependencyHealthCopy,
    dependencyOutcomeCopy,
    evidenceNotices,
    formatTarget,
    mutationStepCopy,
    signedPriority,
  } from '$lib/profile-view';
  import { fraction, initial, reduce } from '$lib/progress.svelte';
  import type {
    LocalGame,
    Profile,
    ProfileActivation,
    ProfileActivationPreview,
    ProfileMember,
    ResolutionResult,
  } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let selectedGame = $state<string | null>(null);
  let profiles = $state<Profile[]>([]);
  let selectedProfile = $state<string | null>(null);
  let members = $state<ProfileMember[]>([]);
  let dependency = $state<ResolutionResult | null>(null);
  let dependencyUnavailable = $state(false);
  let loadingGames = $state(true);
  let loadingProfiles = $state(false);
  let loadingMembers = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let errorCode = $state<string | null>(null);
  let notice = $state<string | null>(null);

  let createName = $state('');
  let createDescription = $state('');
  let renaming = $state<string | null>(null);
  let renameValue = $state('');
  let addModId = $state('');
  let addFileId = $state('');
  let priorityDrafts = $state<Record<string, string>>({});

  let activationPreview = $state<ProfileActivationPreview | null>(null);
  let activation = $state<ProfileActivation | null>(null);
  let progress = $state(initial());

  const currentProfile = $derived(
    profiles.find((profile) => profile.id === selectedProfile) ?? null,
  );
  const activationStatus = $derived(activation === null ? null : activationCopy(activation));
  const activationFreshness = $derived(
    activationPreview === null ? null : freshnessCopy(activationPreview.baseline_freshness),
  );
  const dependencyStatus = $derived(
    activationPreview === null ? null : dependencyOutcomeCopy(activationPreview.dependency),
  );

  function clearMessages() {
    error = null;
    errorCode = null;
    notice = null;
  }

  function showError(value: unknown) {
    errorCode = value instanceof BridgeError ? value.code : 'internal';
    error =
      errorCode === 'conflict'
        ? 'The active profile cannot be deleted. Activate another profile first.'
        : value instanceof Error
          ? value.message
          : String(value);
  }

  function sortMembers(rows: ProfileMember[]) {
    return [...rows].sort((a, b) => a.priority - b.priority || a.id.localeCompare(b.id));
  }

  function setMember(row: ProfileMember) {
    members = sortMembers(members.map((member) => (member.id === row.id ? row : member)));
    priorityDrafts[row.id] = String(row.priority);
  }

  async function refreshDependency(profileId: string) {
    try {
      dependency = await commands.resolveDependencies(profileId);
      dependencyUnavailable = false;
    } catch {
      dependency = null;
      dependencyUnavailable = true;
    }
  }

  async function showProfile(profileId: string) {
    selectedProfile = profileId;
    loadingMembers = true;
    error = null;
    activationPreview = null;
    activation = null;
    dependency = null;
    dependencyUnavailable = false;
    try {
      const [memberResult, dependencyResult] = await Promise.allSettled([
        commands.profileMembers(profileId),
        commands.resolveDependencies(profileId),
      ]);
      if (memberResult.status === 'rejected') throw memberResult.reason;
      members = sortMembers(memberResult.value);
      priorityDrafts = Object.fromEntries(
        members.map((member) => [member.id, String(member.priority)]),
      );
      if (dependencyResult.status === 'fulfilled') {
        dependency = dependencyResult.value;
      } else {
        dependencyUnavailable = true;
      }
    } catch (value) {
      members = [];
      showError(value);
    } finally {
      loadingMembers = false;
    }
  }

  async function loadProfiles(gameId: string, preferred?: string) {
    selectedGame = gameId;
    selectedProfile = null;
    profiles = [];
    members = [];
    dependency = null;
    activationPreview = null;
    activation = null;
    loadingProfiles = true;
    clearMessages();
    try {
      profiles = await commands.profiles(gameId);
      const next =
        profiles.find((profile) => profile.id === preferred) ??
        profiles.find((profile) => profile.is_active) ??
        profiles[0];
      if (next !== undefined) await showProfile(next.id);
    } catch (value) {
      showError(value);
    } finally {
      loadingProfiles = false;
    }
  }

  async function createProfile(copyFrom?: Profile) {
    if (selectedGame === null) return;
    const name = copyFrom === undefined ? createName.trim() : nextCopyName(copyFrom.name);
    if (name === '') return;
    busy = copyFrom === undefined ? 'Creating profile' : 'Duplicating profile';
    clearMessages();
    try {
      const created = await commands.createProfile(
        selectedGame,
        name,
        copyFrom?.description ?? (createDescription.trim() || undefined),
        copyFrom?.id,
      );
      profiles = [...profiles, created];
      createName = '';
      createDescription = '';
      notice =
        copyFrom === undefined ? `Created ${created.name}.` : `Duplicated as ${created.name}.`;
      await showProfile(created.id);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  function nextCopyName(name: string): string {
    const taken = new Set(profiles.map((profile) => profile.name.toLocaleLowerCase()));
    let candidate = `${name} copy`;
    let number = 2;
    while (taken.has(candidate.toLocaleLowerCase())) {
      candidate = `${name} copy ${number}`;
      number += 1;
    }
    return candidate;
  }

  function beginRename(profile: Profile) {
    renaming = profile.id;
    renameValue = profile.name;
    clearMessages();
  }

  async function saveRename(profile: Profile) {
    const name = renameValue.trim();
    if (name === '') return;
    busy = 'Renaming profile';
    clearMessages();
    try {
      const updated = await commands.renameProfile(profile.id, name);
      profiles = profiles.map((candidate) => (candidate.id === updated.id ? updated : candidate));
      renaming = null;
      notice = `Renamed profile to ${updated.name}.`;
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function deleteProfile(profile: Profile) {
    if (!window.confirm(`Delete profile “${profile.name}”? This cannot be undone.`)) return;
    busy = 'Deleting profile';
    clearMessages();
    try {
      await commands.deleteProfile(profile.id);
      profiles = profiles.filter((candidate) => candidate.id !== profile.id);
      notice = `Deleted ${profile.name}.`;
      if (selectedProfile === profile.id) {
        const next = profiles.find((candidate) => candidate.is_active) ?? profiles[0];
        selectedProfile = null;
        members = [];
        if (next !== undefined) await showProfile(next.id);
      }
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function addMember() {
    if (selectedProfile === null || addModId.trim() === '') return;
    busy = 'Adding member';
    clearMessages();
    try {
      const member = await commands.addProfileMember(
        selectedProfile,
        addModId.trim(),
        addFileId.trim() || undefined,
      );
      members = sortMembers([...members, member]);
      priorityDrafts[member.id] = String(member.priority);
      addModId = '';
      addFileId = '';
      notice = 'Member added to desired state. The game directory has not changed.';
      await refreshDependency(selectedProfile);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function removeMember(member: ProfileMember) {
    if (selectedProfile === null) return;
    busy = 'Removing member';
    clearMessages();
    try {
      await commands.removeProfileMember(member.id);
      members = members.filter((candidate) => candidate.id !== member.id);
      notice = 'Member removed from desired state. The game directory has not changed.';
      await refreshDependency(selectedProfile);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function toggleState(member: ProfileMember) {
    busy = member.desired === 'enabled' ? 'Disabling member' : 'Enabling member';
    clearMessages();
    try {
      setMember(
        await commands.setMemberState(
          member.id,
          member.desired === 'enabled' ? 'disabled' : 'enabled',
        ),
      );
      notice = 'Desired state updated. Activate the profile to change files.';
      if (selectedProfile !== null) await refreshDependency(selectedProfile);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function togglePin(member: ProfileMember) {
    busy = member.pin.kind === 'pinned' ? 'Unpinning member' : 'Pinning member';
    clearMessages();
    try {
      setMember(await commands.setMemberPin(member.id, member.pin.kind !== 'pinned'));
      notice = member.pin.kind === 'pinned' ? 'Member unpinned.' : 'Member version pinned.';
      if (selectedProfile !== null) await refreshDependency(selectedProfile);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function savePriority(member: ProfileMember) {
    const priority = signedPriority(priorityDrafts[member.id] ?? '');
    if (priority === null) {
      errorCode = 'invalid_priority';
      error = 'Priority must be a signed whole number from -2147483648 to 2147483647.';
      return;
    }
    busy = 'Reordering member';
    clearMessages();
    try {
      setMember(await commands.reorderProfileMember(member.id, priority));
      notice = 'Priority updated. Lower values deploy first; higher values sit above them.';
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function previewActivation() {
    if (selectedProfile === null) return;
    busy = 'Planning activation';
    clearMessages();
    activation = null;
    progress = initial();
    try {
      activationPreview = await commands.planProfileActivation(selectedProfile);
    } catch (value) {
      activationPreview = null;
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function activate() {
    if (selectedProfile === null || activationPreview === null || !canActivate(activationPreview)) {
      return;
    }
    busy = 'Activating profile';
    clearMessages();
    activation = null;
    progress = initial();
    try {
      activation = await commands.activateProfile(selectedProfile, activationPreview.fingerprint);
      if (activation.state === 'applied' && selectedGame !== null) {
        const activatedId = selectedProfile;
        profiles = await commands.profiles(selectedGame);
        selectedProfile = activatedId;
      }
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function cancelActivation() {
    if (activation?.operation_id === null || activation?.operation_id === undefined) return;
    progress = { ...progress, cancelRequested: true };
    try {
      await commands.cancelOperation(activation.operation_id);
    } catch (value) {
      showError(value);
    }
  }

  onMount(() => {
    let stop: (() => void) | undefined;
    void onProgress((event) => {
      progress = reduce(progress, event);
    }).then((unlisten) => {
      stop = unlisten;
    });

    void (async () => {
      try {
        games = await commands.localGames();
        loadingGames = false;
        const requested = page.url.searchParams.get('game');
        const game = games.find((candidate) => candidate.id === requested) ?? games[0];
        if (game !== undefined) await loadProfiles(game.id);
      } catch (value) {
        showError(value);
      } finally {
        loadingGames = false;
      }
    })();

    return () => stop?.();
  });
</script>

<header class="page-heading">
  <div>
    <h1>Profiles</h1>
    <p class="muted">Reusable desired mod sets, scoped to one game installation.</p>
  </div>
  {#if games.length > 0}
    <label>
      Installation
      <select
        aria-label="Installation"
        value={selectedGame}
        onchange={(event) => loadProfiles((event.currentTarget as HTMLSelectElement).value)}
        disabled={busy !== null}
      >
        {#each games as game (game.id)}
          <option value={game.id}>{game.adapter_id} — {game.install_root}</option>
        {/each}
      </select>
    </label>
  {/if}
</header>

{#if error !== null}<p class="error" role="alert" data-error-code={errorCode}>{error}</p>{/if}
{#if notice !== null}<p class="severity-neutral" role="status">{notice}</p>{/if}
{#if busy !== null}<p aria-live="polite">{busy}…</p>{/if}

{#if loadingGames}
  <p class="muted">Loading game installations…</p>
{:else if games.length === 0}
  <div class="panel empty-state">
    <h2>No game installations</h2>
    <p>Register a game before creating a profile.</p>
    <a href="/games">Go to Games</a>
  </div>
{:else}
  <section aria-labelledby="profile-heading">
    <div class="section-heading">
      <div>
        <h2 id="profile-heading">Profiles for this installation</h2>
        <p class="muted">Exactly one profile is active after its files have been verified.</p>
      </div>
      <form
        class="inline-form"
        onsubmit={(event) => {
          event.preventDefault();
          void createProfile();
        }}
      >
        <input
          aria-label="New profile name"
          placeholder="Profile name"
          bind:value={createName}
          required
        />
        <input
          aria-label="New profile description"
          placeholder="Description (optional)"
          bind:value={createDescription}
        />
        <button class="primary" type="submit" disabled={busy !== null || createName.trim() === ''}
          >Create profile</button
        >
      </form>
    </div>

    {#if loadingProfiles}
      <p class="muted">Loading profiles…</p>
    {:else if profiles.length === 0 && error === null}
      <div class="panel empty-state">
        <h3>No profiles found</h3>
        <p>This installation has no profile yet. Create one to define its desired mods.</p>
      </div>
    {:else}
      <div class="profile-grid">
        {#each profiles as profile (profile.id)}
          <article class:chosen={selectedProfile === profile.id} class="panel profile-card">
            <div class="card-title">
              <h3>{profile.name}</h3>
              {#if profile.is_active}<span class="badge active">Active</span>{/if}
            </div>
            {#if profile.description !== null}<p class="muted">{profile.description}</p>{/if}
            <p class="muted">Updated {profile.updated_at}</p>
            {#if renaming === profile.id}
              <form
                class="inline-form"
                onsubmit={(event) => {
                  event.preventDefault();
                  void saveRename(profile);
                }}
              >
                <input aria-label={`Rename ${profile.name}`} bind:value={renameValue} required />
                <button type="submit" disabled={busy !== null}>Save name</button>
                <button type="button" onclick={() => (renaming = null)}>Cancel</button>
              </form>
            {:else}
              <div class="actions">
                <button onclick={() => showProfile(profile.id)} disabled={busy !== null}
                  >Show</button
                >
                <button onclick={() => beginRename(profile)} disabled={busy !== null}>Rename</button
                >
                <button onclick={() => createProfile(profile)} disabled={busy !== null}
                  >Duplicate</button
                >
                <button
                  class="danger"
                  onclick={() => deleteProfile(profile)}
                  disabled={busy !== null}>Delete</button
                >
              </div>
            {/if}
          </article>
        {/each}
      </div>
    {/if}
  </section>

  {#if currentProfile !== null}
    <section aria-labelledby="member-heading">
      <div class="section-heading">
        <div>
          <h2 id="member-heading">{currentProfile.name} members</h2>
          <p class="muted">
            These controls change desired state only. Activate/apply the profile to touch the game
            directory.
          </p>
        </div>
        <button onclick={previewActivation} disabled={busy !== null || loadingMembers}
          >Preview activation</button
        >
      </div>

      <form
        class="panel add-member"
        onsubmit={(event) => {
          event.preventDefault();
          void addMember();
        }}
      >
        <label>
          Mod ID
          <input bind:value={addModId} required />
        </label>
        <label>
          Provider file ID <span class="muted">(optional)</span>
          <input bind:value={addFileId} />
        </label>
        <button class="primary" type="submit" disabled={busy !== null || addModId.trim() === ''}
          >Add member</button
        >
      </form>

      {#if loadingMembers}
        <p class="muted">Loading profile members and dependency status…</p>
      {:else if members.length === 0}
        <div class="panel empty-state">
          <h3>No members</h3>
          <p>This profile intentionally has an empty desired mod set.</p>
        </div>
      {:else}
        {#if dependencyUnavailable}
          <p class="severity-warning" role="status">
            Dependency status unavailable. This is not the same as having no dependencies.
          </p>
        {:else if dependency !== null}
          {#each evidenceNotices(dependency.evidence) as item (item.label)}
            <p class={`severity-${item.severity}`}>{item.label}: {item.detail}</p>
          {/each}
        {/if}
        <div class="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Mod</th><th>State</th><th>Pin</th><th>Version</th><th>Dependency</th><th
                  >Download</th
                ><th>Priority</th><th></th>
              </tr>
            </thead>
            <tbody>
              {#each members as member (member.id)}
                {@const health = dependencyUnavailable
                  ? dependencyHealthCopy('unavailable')
                  : dependencyHealthCopy(
                      dependency?.health.find((row) => row.profile_member_id === member.id)
                        ?.health ?? 'unknown',
                    )}
                <tr>
                  <td>
                    <strong>{member.selection.provider}:{member.selection.provider_mod_id}</strong>
                    <span class="muted member-id">{member.mod_id}</span>
                  </td>
                  <td>
                    <button
                      aria-label={`${member.desired === 'enabled' ? 'Disable' : 'Enable'} ${member.selection.provider_mod_id}`}
                      onclick={() => toggleState(member)}
                      disabled={busy !== null}
                      >{member.desired === 'enabled' ? 'Enabled' : 'Disabled'}</button
                    >
                  </td>
                  <td>
                    <button
                      aria-label={`${member.pin.kind === 'pinned' ? 'Unpin' : 'Pin'} ${member.selection.provider_mod_id}`}
                      title={member.pin.kind === 'pinned'
                        ? (member.pin.reason ?? 'Pinned')
                        : 'Not pinned'}
                      onclick={() => togglePin(member)}
                      disabled={busy !== null}
                      >{member.pin.kind === 'pinned' ? 'Pinned' : 'Unpinned'}</button
                    >
                  </td>
                  <td>{member.selection.provider_version_id ?? 'Version not selected'}</td>
                  <td class={`severity-${health.severity}`} title={health.detail}>{health.label}</td
                  >
                  <td
                    class={member.installation_id === null
                      ? 'severity-warning'
                      : 'severity-neutral'}
                  >
                    {member.installation_id === null ? 'Download required' : 'Available locally'}
                  </td>
                  <td>
                    <div class="priority-control">
                      <input
                        aria-label={`Priority for ${member.selection.provider_mod_id}`}
                        inputmode="numeric"
                        value={priorityDrafts[member.id]}
                        oninput={(event) =>
                          (priorityDrafts[member.id] = (
                            event.currentTarget as HTMLInputElement
                          ).value)}
                      />
                      <button onclick={() => savePriority(member)} disabled={busy !== null}
                        >Save</button
                      >
                    </div>
                  </td>
                  <td>
                    <button
                      class="danger"
                      onclick={() => removeMember(member)}
                      disabled={busy !== null}>Remove</button
                    >
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>
  {/if}

  {#if activationPreview !== null}
    <section
      class="panel activation"
      aria-labelledby="activation-heading"
      data-testid="activation-preview"
    >
      <div class="card-title">
        <h2 id="activation-heading">Activation preview</h2>
        <span class:blocked={!canActivate(activationPreview)} class="badge">
          {canActivate(activationPreview) ? 'Ready' : 'Blocked'}
        </span>
      </div>
      <div class="summary-grid">
        <div>
          <span class="muted">Downloads</span><strong>{activationPreview.downloads.length}</strong>
        </div>
        <div>
          <span class="muted">Filesystem changes</span><strong
            >{activationPreview.plan.steps.length}</strong
          >
        </div>
        <div>
          <span class="muted">Bytes to write</span><strong
            >{formatBytes(activationPreview.bytes_to_write)}</strong
          >
        </div>
        <div>
          <span class="muted">Baseline freshness</span>
          <strong
            class={`severity-${activationFreshness?.severity}`}
            data-testid="activation-freshness">{activationFreshness?.label}</strong
          >
        </div>
      </div>
      <p class={`severity-${activationFreshness?.severity}`}>{activationFreshness?.detail}</p>

      <h3>Downloads</h3>
      {#if activationPreview.downloads.length === 0}
        <p class="muted">No downloads required.</p>
      {:else}
        <ul>
          {#each activationPreview.downloads as download (download.member_id)}
            <li>
              {download.name} — {download.bytes === null
                ? 'size unknown'
                : formatBytes(download.bytes)}
            </li>
          {/each}
        </ul>
      {/if}

      <h3>Filesystem changes</h3>
      {#if activationPreview.plan.steps.length === 0}
        <p class="muted">No filesystem changes in this preview.</p>
      {:else}
        <p class="muted">
          Writes cover activations, upgrades, downgrades, and restorations; deletes are
          deactivations.
        </p>
        <table>
          <thead><tr><th>Change</th><th>Target</th><th>Meaning</th></tr></thead>
          <tbody>
            {#each activationPreview.plan.steps as step, index (`${step.kind}-${formatTarget(step.target)}-${index}`)}
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

      {#if activationPreview.plan.conflicts.length > 0}
        <h3>File conflicts</h3>
        {#each activationPreview.plan.conflicts as conflict (formatTarget(conflict.target))}
          <p class="severity-warning">
            <code>{formatTarget(conflict.target)}</code> — {conflict.providers.length} providers need
            a winner.
          </p>
        {/each}
      {/if}

      <h3>Dependency status</h3>
      <p class={`severity-${dependencyStatus?.severity}`} data-testid="dependency-outcome">
        <strong>{dependencyStatus?.label}</strong> — {dependencyStatus?.detail}
      </p>
      {#each evidenceNotices(activationPreview.dependency.evidence) as item (item.label)}
        <p class={`severity-${item.severity}`}>{item.label}: {item.detail}</p>
      {/each}

      {#if activationPreview.blockers.length > 0}
        <h3>Blockers</h3>
        <ul class="severity-danger" data-testid="activation-blockers">
          {#each activationPreview.blockers as blocker, index (`${blocker.kind}-${index}`)}
            <li>
              {blocker.kind.replaceAll('_', ' ')}{blocker.target
                ? ` — ${blocker.target}`
                : ''}{blocker.member_id ? ` — member ${blocker.member_id}` : ''}{blocker.detail
                ? `: ${blocker.detail}`
                : ''}
            </li>
          {/each}
        </ul>
      {/if}

      <p>
        <button
          class="primary"
          onclick={activate}
          disabled={busy !== null || !canActivate(activationPreview)}>Activate profile</button
        >
      </p>
    </section>
  {/if}

  {#if busy === 'Activating profile' || activation !== null || progress.stage !== 'idle'}
    <section class="panel" aria-live="polite" data-testid="activation-progress">
      <h2>Activation progress</h2>
      <p>
        <strong>{progress.stage.replaceAll('_', ' ')}</strong>{progress.detail === null
          ? ''
          : ` — ${progress.detail}`}
      </p>
      {#if fraction(progress) === null}<progress></progress>{:else}<progress
          value={fraction(progress)}
        ></progress>{/if}
      {#if progress.total !== null}
        <p class="muted">{progress.completed} of {progress.total}</p>
      {/if}
      {#each progress.warnings as warning (warning)}<p class="severity-warning">{warning}</p>{/each}
      {#if activation !== null && (activation.state === 'preparing' || activation.state === 'applying')}
        <button
          onclick={cancelActivation}
          disabled={activation.operation_id === null || progress.cancelRequested}
        >
          {progress.cancelRequested ? 'Cancellation requested…' : 'Cancel activation'}
        </button>
        <p class="muted">Cancellation stops at the next safe operation boundary.</p>
      {/if}
    </section>
  {/if}

  {#if activationStatus !== null}
    <section
      class={`panel activation-result severity-${activationStatus.severity}`}
      data-testid="activation-result"
    >
      <h2>{activationStatus.label}</h2>
      <p>{activationStatus.detail}</p>
      {#if activation?.error !== null}<p>{activation?.error}</p>{/if}
      {#if activation?.state === 'failed'}
        <p><a href="/recovery">Open recovery</a> before launching the game.</p>
      {/if}
    </section>
  {/if}
{/if}

<style>
  .page-heading,
  .section-heading,
  .card-title,
  .actions,
  .inline-form,
  .priority-control {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .page-heading,
  .section-heading,
  .card-title {
    justify-content: space-between;
  }
  .page-heading,
  section {
    margin-bottom: 1.5rem;
  }
  .page-heading h1,
  .page-heading p,
  .card-title h3,
  .section-heading h2,
  .section-heading p {
    margin-top: 0;
    margin-bottom: 0.25rem;
  }
  .profile-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 0.75rem;
  }
  .profile-card.chosen {
    border-color: var(--accent);
  }
  .actions,
  .inline-form {
    flex-wrap: wrap;
  }
  .inline-form input {
    width: 12rem;
  }
  .badge {
    border: 1px solid var(--ok);
    border-radius: 999px;
    color: var(--ok);
    padding: 0.1rem 0.5rem;
    font-size: 0.75rem;
  }
  .badge.blocked {
    border-color: var(--danger);
    color: var(--danger);
  }
  .add-member {
    display: grid;
    grid-template-columns: minmax(12rem, 1fr) minmax(12rem, 1fr) auto;
    gap: 0.75rem;
    align-items: end;
    margin-bottom: 0.75rem;
  }
  .add-member label,
  .summary-grid div {
    display: grid;
    gap: 0.25rem;
  }
  .member-id {
    display: block;
    font-size: 0.75rem;
  }
  .priority-control input {
    width: 6rem;
  }
  .table-scroll {
    overflow-x: auto;
  }
  .summary-grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(8rem, 1fr));
    gap: 0.75rem;
    margin: 1rem 0;
  }
  .empty-state {
    text-align: center;
  }
  .activation-result {
    border-color: currentColor;
  }
  @media (max-width: 850px) {
    .page-heading,
    .section-heading {
      align-items: stretch;
      flex-direction: column;
    }
    .add-member,
    .summary-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
