<script lang="ts">
  import { page } from '$app/state';
  import { freshnessCopy } from '$lib/baseline-view';
  import { BridgeError, commands, onProgress } from '$lib/bridge';
  import DependencyDetail from '$lib/components/DependencyDetail.svelte';
  import SolvedPlan from '$lib/components/SolvedPlan.svelte';
  import { editActions, isStalePlan, stalePlanCopy, validateIgnore } from '$lib/dependency-view';
  import type { DependencyActionId } from '$lib/dependency-view';
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
    DependencyGroup,
    DependencySnapshot,
    LocalGame,
    PreviewMemberEdit,
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

  /**
   * A desired-state edit the user has asked for but not committed.
   *
   * Nothing is sent to the backend until `commit` runs, so dismissing this
   * leaves both desired state and the game directory exactly as they were.
   */
  let pendingEdit = $state<{
    label: string;
    edits: PreviewMemberEdit[];
    commit: () => Promise<void>;
  } | null>(null);
  let pendingPreview = $state<ResolutionResult | null>(null);
  let pendingUnavailable = $state(false);

  let detail = $state<{ member: ProfileMember; snapshot: DependencySnapshot } | null>(null);
  let detailNotice = $state<string | null>(null);
  let ignoreDraft = $state<{
    memberId: string;
    groupId: string;
    fingerprint: string;
    label: string;
    reason: string;
  } | null>(null);
  let ignoreProblems = $state<string[]>([]);
  let stalePlan = $state(false);

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
    pendingEdit = null;
    pendingPreview = null;
    detail = null;
    detailNotice = null;
    ignoreDraft = null;
    stalePlan = false;
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

  /**
   * Check a desired-state edit before it is saved.
   *
   * The edit is described to the backend as an uncommitted change, the result
   * is shown, and only an explicit confirmation commits it. Cancelling has
   * never called a mutating command at all.
   */
  async function previewEdit(
    label: string,
    edits: PreviewMemberEdit[],
    commit: () => Promise<void>,
  ) {
    if (selectedProfile === null) return;
    clearMessages();
    stalePlan = false;
    pendingEdit = { label, edits, commit };
    pendingPreview = null;
    pendingUnavailable = false;
    busy = 'Checking dependencies for this change';
    try {
      pendingPreview = await commands.resolveDependencies(selectedProfile, edits);
    } catch (value) {
      pendingUnavailable = true;
      showError(value);
    } finally {
      busy = null;
    }
  }

  /** Abandon an uncommitted edit. No command was sent and none is sent now. */
  function cancelPendingEdit() {
    pendingEdit = null;
    pendingPreview = null;
    pendingUnavailable = false;
    notice = 'Change cancelled. Desired state and the game directory are unchanged.';
  }

  async function confirmPendingEdit() {
    const pending = pendingEdit;
    if (pending === null || selectedProfile === null) return;
    busy = pending.label;
    clearMessages();
    try {
      await pending.commit();
      pendingEdit = null;
      pendingPreview = null;
      pendingUnavailable = false;
      await refreshDependency(selectedProfile);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  function addMember() {
    if (selectedProfile === null || addModId.trim() === '') return;
    const profileId = selectedProfile;
    const modId = addModId.trim();
    const fileId = addFileId.trim();
    void previewEdit(
      'Adding member',
      [{ kind: 'add', mod_id: modId, provider_file_id: fileId === '' ? null : fileId }],
      async () => {
        const member = await commands.addProfileMember(profileId, modId, fileId || undefined);
        members = sortMembers([...members, member]);
        priorityDrafts[member.id] = String(member.priority);
        addModId = '';
        addFileId = '';
        notice = 'Member added to desired state. The game directory has not changed.';
      },
    );
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

  function toggleState(member: ProfileMember) {
    const desired = member.desired === 'enabled' ? 'disabled' : 'enabled';
    void previewEdit(
      desired === 'disabled' ? 'Disabling member' : 'Enabling member',
      [{ kind: 'set_state', profile_member_id: member.id, desired }],
      async () => {
        setMember(await commands.setMemberState(member.id, desired));
        notice = 'Desired state updated. Activate the profile to change files.';
      },
    );
  }

  function togglePin(member: ProfileMember) {
    const pinned = member.pin.kind !== 'pinned';
    void previewEdit(
      pinned ? 'Pinning member' : 'Unpinning member',
      [{ kind: 'set_pin', profile_member_id: member.id, pinned }],
      async () => {
        setMember(await commands.setMemberPin(member.id, pinned));
        notice = pinned
          ? 'Member version pinned. A pinned member never changes version.'
          : 'Member unpinned. The solver may now change its version.';
      },
    );
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

  /** Look up one member's declared requirements for the detail panel. */
  async function showDetail(member: ProfileMember) {
    clearMessages();
    detailNotice = null;
    ignoreDraft = null;
    if (member.selection.provider_file_id === null) {
      detail = null;
      detailNotice =
        'This member names no provider file yet, so there is no dependency definition to look up. That is not the same as requiring nothing.';
      return;
    }
    busy = 'Loading dependency details';
    try {
      detail = {
        member,
        snapshot: await commands.dependencySnapshot(
          member.mod_id,
          member.selection.provider_file_id,
        ),
      };
    } catch (value) {
      detail = null;
      detailNotice =
        'The dependency definition could not be loaded. This does not mean the mod requires nothing.';
      showError(value);
    } finally {
      busy = null;
    }
  }

  function beginIgnore(group: DependencyGroup, fingerprint: string) {
    if (detail === null) return;
    ignoreProblems = [];
    ignoreDraft = {
      memberId: detail.member.id,
      groupId: group.id,
      fingerprint,
      label: group.label ?? `requirement ${group.id}`,
      reason: '',
    };
  }

  async function saveIgnore() {
    const draft = ignoreDraft;
    if (draft === null || selectedProfile === null) return;
    const validated = validateIgnore(draft);
    if (!validated.ok) {
      ignoreProblems = validated.problems;
      return;
    }
    busy = 'Recording accepted risk';
    clearMessages();
    try {
      await commands.setDependencyOverride(
        validated.request.memberId,
        validated.request.groupId,
        validated.request.fingerprint,
        validated.request.reason,
      );
      ignoreDraft = null;
      ignoreProblems = [];
      notice = `Risk accepted for ${draft.label}. It applies only to the definition whose fingerprint was shown.`;
      await refreshDependency(selectedProfile);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  async function clearIgnore(group: DependencyGroup, fingerprint: string) {
    if (detail === null || selectedProfile === null) return;
    busy = 'Clearing accepted risk';
    clearMessages();
    try {
      await commands.clearDependencyOverride(detail.member.id, group.id, fingerprint);
      notice = 'Accepted risk withdrawn. The requirement applies again.';
      await refreshDependency(selectedProfile);
    } catch (value) {
      showError(value);
    } finally {
      busy = null;
    }
  }

  /** Accept a solved plan as a desired-state edit; nothing is written to disk. */
  async function applySolvedPlan() {
    if (selectedProfile === null || dependency === null) return;
    busy = 'Applying the solved plan';
    clearMessages();
    stalePlan = false;
    try {
      const applied = await commands.applyDependencyPlan(
        selectedProfile,
        dependency.fingerprint ?? null,
      );
      members = sortMembers(applied.members);
      priorityDrafts = Object.fromEntries(
        members.map((member) => [member.id, String(member.priority)]),
      );
      dependency = applied.dependency;
      dependencyUnavailable = false;
      activationPreview = null;
      notice = 'Desired state updated to the solved set. Preview activation to change files.';
    } catch (value) {
      if (value instanceof BridgeError && isStalePlan(value.code)) {
        stalePlan = true;
        await refreshDependency(selectedProfile);
      } else {
        showError(value);
      }
    } finally {
      busy = null;
    }
  }

  /** Route one dependency action. Only solved variants reach an apply. */
  function handleDependencyAction(action: DependencyActionId) {
    switch (action) {
      case 'install_missing':
      case 'apply_update_set':
      case 'apply_disable_set':
        void applySolvedPlan();
        break;
      case 'change_pins':
        notice =
          'Change a pin in the Pin column above. Each change is re-checked before it is saved.';
        break;
      case 'ignore_requirement':
        notice =
          'Open Details for the affected member. A requirement can only be ignored where its fingerprint is displayed.';
        break;
      case 'replan':
        stalePlan = false;
        if (selectedProfile !== null) void refreshDependency(selectedProfile);
        break;
      case 'save_edit':
        void confirmPendingEdit();
        break;
      case 'cancel':
        cancelPendingEdit();
        break;
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
                    <div class="row-actions">
                      <button
                        aria-label={`Details for ${member.selection.provider_mod_id}`}
                        onclick={() => showDetail(member)}
                        disabled={busy !== null}>Details</button
                      >
                      <button
                        class="danger"
                        onclick={() => removeMember(member)}
                        disabled={busy !== null}>Remove</button
                      >
                    </div>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>
  {/if}

  {#if pendingEdit !== null}
    <section aria-labelledby="pending-heading" data-testid="pending-edit">
      <h2 id="pending-heading">Before this change is saved</h2>
      <p class="muted">
        {pendingEdit.label} has not been saved. Onera checked what it would mean first; nothing in desired
        state or on disk has changed yet.
      </p>
      {#if pendingUnavailable}
        <p class="severity-warning" role="status">
          The dependency check for this change could not be run. That is not the same as the change
          being safe.
        </p>
        <div class="actions">
          <button class="primary" onclick={confirmPendingEdit} disabled={busy !== null}
            >Save this change anyway</button
          >
          <button onclick={cancelPendingEdit}>Cancel</button>
        </div>
      {:else if pendingPreview !== null}
        <SolvedPlan
          heading="Dependency impact of this change"
          result={pendingPreview}
          {members}
          busy={busy !== null}
          actions={editActions(pendingPreview)}
          onAction={handleDependencyAction}
          testId="pending-plan"
        />
      {/if}
    </section>
  {/if}

  {#if stalePlan}
    {@const copy = stalePlanCopy()}
    <p class={`severity-${copy.severity}`} role="status" data-testid="stale-plan">
      <strong>{copy.label}</strong> — {copy.detail}
    </p>
  {/if}

  {#if currentProfile !== null && dependency !== null && pendingEdit === null}
    <SolvedPlan
      heading="Dependency check for this profile"
      result={dependency}
      {members}
      busy={busy !== null}
      onAction={handleDependencyAction}
      testId="profile-plan"
    />
  {/if}

  {#if detailNotice !== null}
    <p class="severity-warning" role="status" data-testid="detail-notice">{detailNotice}</p>
  {/if}

  {#if detail !== null}
    <section aria-labelledby="detail-heading">
      <div class="section-heading">
        <h2 id="detail-heading">
          {detail.member.selection.provider}:{detail.member.selection.provider_mod_id} requirements
        </h2>
        <button onclick={() => (detail = null)}>Close details</button>
      </div>
      <DependencyDetail
        snapshot={detail.snapshot}
        gameSlug={detail.snapshot.source.game_slug}
        busy={busy !== null}
        onIgnore={beginIgnore}
        onClearIgnore={clearIgnore}
      />
      {#if ignoreDraft !== null}
        <form
          class="panel ignore-form"
          data-testid="ignore-form"
          onsubmit={(event) => {
            event.preventDefault();
            void saveIgnore();
          }}
        >
          <h3>Ignore “{ignoreDraft.label}”</h3>
          <p class="muted">
            This records that you accepted one named risk against the definition fingerprinted
            <code>{ignoreDraft.fingerprint}</code>. It does not pick a winner for any file conflict,
            and it stops applying if the provider changes the requirement.
          </p>
          <label>
            Reason
            <input
              aria-label="Reason for ignoring this requirement"
              bind:value={ignoreDraft.reason}
            />
          </label>
          {#each ignoreProblems as problem (problem)}
            <p class="severity-danger">{problem}</p>
          {/each}
          <div class="actions">
            <button class="danger" type="submit" disabled={busy !== null}>Accept this risk</button>
            <button type="button" onclick={() => (ignoreDraft = null)}>Cancel</button>
          </div>
        </form>
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
  .row-actions,
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .ignore-form label {
    display: grid;
    gap: 0.25rem;
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
