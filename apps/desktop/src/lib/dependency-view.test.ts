import { describe, expect, it } from 'vitest';
import {
  availabilityCopy,
  canApplyCompatibleUpdate,
  candidateLabel,
  candidateStatusCopy,
  candidateTargetCopy,
  changeCopy,
  compatibleUpdateCopy,
  compatibleUpdateReportCopy,
  declaresNoDependencies,
  dependencyHealthCopy,
  dependencyOutcomeCopy,
  dlcCopy,
  editActions,
  evidenceIsCurrent,
  evidenceNotices,
  groupState,
  healthBlocksApply,
  isStalePlan,
  namedRequirements,
  offersAPlan,
  outcomeActions,
  pinsBlockingSolution,
  proposalChanges,
  requirementExplanation,
  requirementKindCopy,
  requirementsForMember,
  selectionLabel,
  snapshotSummary,
  sourceLabel,
  stalePlanCopy,
  validateIgnore,
} from './dependency-view';
import type {
  CompatibleUpdatePreview,
  DependencyCandidate,
  DependencyEvidence,
  DependencyGroup,
  DependencyOutcome,
  DependencySnapshot,
  DependencySource,
  ProfileMember,
  ResolutionResult,
  UnsatisfiedRequirement,
} from './types';

const GAME = 'cyberpunk2077';

const evidence = (over: Partial<DependencyEvidence> = {}): DependencyEvidence => ({
  fresh: 1,
  cached: 0,
  stale: 0,
  unavailable: 0,
  unsupported: 0,
  unknown_dlc: 0,
  ...over,
});

const source = (over: Partial<DependencySource> = {}): DependencySource => ({
  provider: 'nexus',
  game_slug: GAME,
  provider_mod_id: '107',
  provider_file_id: '9001',
  provider_version_id: 'v-9001',
  ...over,
});

const requirement = (over: Partial<UnsatisfiedRequirement> = {}): UnsatisfiedRequirement => ({
  source: source(),
  group_id: 'group-1',
  label: 'Cyber Engine Tweaks',
  explanation: 'no available candidate targets this game',
  ...over,
});

const result = (
  outcome: DependencyOutcome,
  over: Partial<ResolutionResult> = {},
): ResolutionResult => ({
  outcome,
  health: [],
  evidence: evidence(),
  ...over,
});

const candidate = (over: Partial<DependencyCandidate> = {}): DependencyCandidate => ({
  provider: 'nexus',
  game_slug: GAME,
  provider_mod_id: '107',
  provider_file_id: '9001',
  provider_version_id: '9001',
  provider_file_group_id: '4210',
  position: 2_500_000,
  status: 'available',
  display_name: 'CET 1.35.0',
  ...over,
});

const group = (over: Partial<DependencyGroup> = {}): DependencyGroup => ({
  id: 'group-1',
  provider_group_key: 'req-1',
  label: 'Cyber Engine Tweaks',
  kind: 'required',
  candidates: [candidate()],
  ...over,
});

const snapshot = (over: Partial<DependencySnapshot> = {}): DependencySnapshot => ({
  id: 'snapshot-1',
  source: source(),
  availability: { kind: 'fetched' },
  groups: [],
  dlc: [],
  provider_revision: null,
  fingerprint: 'b3:definition',
  fetched_at: '2026-09-01T08:00:00Z',
  ...over,
});

const member = (over: Partial<ProfileMember> = {}): ProfileMember => ({
  id: 'member-1',
  profile_id: 'profile-1',
  mod_id: 'mod-cet',
  selection: {
    provider: 'nexus',
    provider_mod_id: '107',
    provider_file_id: '9001',
    provider_version_id: 'v-9001',
    provider_file_group_id: 'g-107',
  },
  installation_id: 'installation-1',
  desired: 'enabled',
  pin: { kind: 'unpinned' },
  priority: 10,
  added_at: '2026-08-01T12:30:00Z',
  ...over,
});

describe('outcomes', () => {
  it('describes every documented outcome and never calls an unsolved one compatible', () => {
    expect(dependencyOutcomeCopy(result({ kind: 'compatible' })).label).toBe('Compatible');
    expect(dependencyOutcomeCopy(result({ kind: 'install_missing', install: [] })).severity).toBe(
      'info',
    );
    expect(
      dependencyOutcomeCopy(result({ kind: 'update_set', select: [], install: [] })).label,
    ).toMatch(/update set/i);
    expect(dependencyOutcomeCopy(result({ kind: 'disable_set', disable: [] })).severity).toBe(
      'warning',
    );
    expect(dependencyOutcomeCopy(result({ kind: 'unsatisfied', requirements: [] })).severity).toBe(
      'danger',
    );
    for (const kind of ['unsatisfied', 'unknown', 'disable_set'] as const) {
      expect(dependencyOutcomeCopy(result({ kind } as DependencyOutcome)).label).not.toBe(
        'Compatible',
      );
    }
  });

  it('carries the unknown reason instead of dropping it', () => {
    const copy = dependencyOutcomeCopy(result({ kind: 'unknown', reason: 'offline' }));
    expect(copy.detail).toContain('offline');
    expect(copy.severity).toBe('warning');
  });

  it('treats an outcome kind it does not recognise as undecidable, not compatible', () => {
    const copy = dependencyOutcomeCopy(result({ kind: 'partially_wonderful' }));
    expect(copy.label).toBe('Dependency compatibility unknown');
    expect(copy.severity).toBe('warning');
    expect(offersAPlan({ kind: 'partially_wonderful' })).toBe(false);
  });

  it('offers a plan only for the three solved variants', () => {
    expect(offersAPlan({ kind: 'install_missing', install: [] })).toBe(true);
    expect(offersAPlan({ kind: 'update_set', select: [], install: [] })).toBe(true);
    expect(offersAPlan({ kind: 'disable_set', disable: [] })).toBe(true);
    expect(offersAPlan({ kind: 'compatible' })).toBe(false);
    expect(offersAPlan({ kind: 'unsatisfied', requirements: [] })).toBe(false);
    expect(offersAPlan({ kind: 'unknown', reason: 'offline' })).toBe(false);
  });
});

describe('member health', () => {
  it('keeps unknown and unavailable distinct and never labels either satisfied', () => {
    expect(dependencyHealthCopy('unknown').label).toBe('Unknown');
    expect(dependencyHealthCopy('unavailable').detail).toMatch(/does not mean no dependencies/i);
    expect(dependencyHealthCopy('future_state').severity).toBe('warning');
  });

  it('blocks apply for unsatisfied, unknown, and any state it does not recognise', () => {
    expect(healthBlocksApply('satisfied')).toBe(false);
    expect(healthBlocksApply('ignored')).toBe(false);
    expect(healthBlocksApply('not_applicable')).toBe(false);
    expect(healthBlocksApply('unsatisfied')).toBe(true);
    expect(healthBlocksApply('unknown')).toBe(true);
    expect(healthBlocksApply('provisionally_fine')).toBe(true);
  });
});

describe('evidence', () => {
  it('reports cached, stale, unavailable, unsupported and unknown DLC independently', () => {
    const notices = evidenceNotices(
      evidence({ cached: 1, stale: 2, unavailable: 3, unsupported: 4, unknown_dlc: 5 }),
    );
    expect(notices.map((notice) => notice.label)).toEqual([
      'Cached dependency data',
      'Stale dependency data',
      'Dependency data unavailable',
      'Dependencies unsupported',
      'DLC ownership unknown',
    ]);
  });

  it('says nothing when every answer was fetched fresh', () => {
    expect(evidenceNotices(evidence())).toEqual([]);
    expect(evidenceIsCurrent(evidence())).toBe(true);
  });

  it('never calls cached data current, even when nothing is stale', () => {
    expect(evidenceIsCurrent(evidence({ cached: 3 }))).toBe(false);
    expect(evidenceNotices(evidence({ cached: 3 }))[0]?.label).toBe('Cached dependency data');
  });
});

describe('snapshot availability', () => {
  it('only lets an authoritative answer mean "requires nothing"', () => {
    expect(declaresNoDependencies(snapshot())).toBe(true);
    expect(
      declaresNoDependencies(
        snapshot({ availability: { kind: 'cached', fetched_at: 'then', stale: true } }),
      ),
    ).toBe(true);
    expect(declaresNoDependencies(snapshot({ availability: { kind: 'unsupported' } }))).toBe(false);
    expect(
      declaresNoDependencies(
        snapshot({ availability: { kind: 'unavailable', reason: 'offline' } }),
      ),
    ).toBe(false);
    expect(declaresNoDependencies(snapshot({ groups: [group()] }))).toBe(false);
    expect(
      declaresNoDependencies(
        snapshot({ dlc: [{ id: 'dlc-1', label: 'Phantom Liberty', alternatives: ['1091501'] }] }),
      ),
    ).toBe(false);
  });

  it('never summarises a non-authoritative answer as an empty requirement set', () => {
    for (const availability of [
      { kind: 'unsupported' } as const,
      { kind: 'unavailable', reason: 'the endpoint disappeared' } as const,
    ]) {
      const summary = snapshotSummary(snapshot({ availability }));
      expect(summary.label).toBe('Requirements not known');
      expect(summary.severity).toBe('warning');
      expect(availabilityCopy(availability).detail).toMatch(/not the same as requiring nothing/i);
    }
    expect(snapshotSummary(snapshot()).label).toBe('Requires nothing');
  });

  it('distinguishes a fresh cache from a stale one and never calls either current', () => {
    const fresh = availabilityCopy({ kind: 'cached', fetched_at: 'yesterday', stale: false });
    const stale = availabilityCopy({ kind: 'cached', fetched_at: 'yesterday', stale: true });
    expect(fresh.label).toBe('Cached');
    expect(fresh.severity).toBe('info');
    expect(stale.label).toBe('Stale cache');
    expect(stale.severity).toBe('warning');
    expect(stale.detail).toMatch(/not current/i);
  });

  it('falls back safely on an availability kind it does not recognise', () => {
    const copy = availabilityCopy({ kind: 'partially_fetched' } as never);
    expect(copy.label).toMatch(/unrecognised/i);
    expect(copy.severity).toBe('warning');
  });
});

describe('requirement groups and candidates', () => {
  it('treats an unrecognised requirement kind as blocking', () => {
    expect(requirementKindCopy('required').blocks).toBe(true);
    expect(requirementKindCopy('incompatible').blocks).toBe(true);
    expect(requirementKindCopy('recommended').blocks).toBe(false);
    expect(requirementKindCopy('strongly_encouraged').blocks).toBe(true);
    expect(requirementKindCopy('strongly_encouraged').severity).toBe('warning');
  });

  it('calls an empty candidate list unsatisfiable rather than satisfied', () => {
    const state = groupState(group({ candidates: [] }), GAME);
    expect(state.satisfiable).toBe(false);
    expect(state.severity).toBe('danger');
    expect(state.label).toBe('Nothing can satisfy this');
  });

  it('separates a wrong-game group from a withdrawn one', () => {
    expect(
      groupState(group({ candidates: [candidate({ game_slug: 'skyrimspecialedition' })] }), GAME)
        .label,
    ).toBe('No candidate targets this game');
    const withdrawn = groupState(group({ candidates: [candidate({ status: 'hidden' })] }), GAME);
    expect(withdrawn.label).toBe('Every candidate has been withdrawn');
    expect(withdrawn.detail).toMatch(/not the same as no candidate existing/i);
  });

  it('is satisfiable when one available candidate targets this game', () => {
    const state = groupState(
      group({ candidates: [candidate({ status: 'removed' }), candidate()] }),
      GAME,
    );
    expect(state.satisfiable).toBe(true);
  });

  it('states an incompatible group as an exclusion, never as a missing requirement', () => {
    const state = groupState(group({ kind: 'incompatible' }), GAME);
    expect(state.label).toBe('Must not be installed together');
    expect(state.satisfiable).toBe(true);
  });

  it('makes only an available candidate selectable', () => {
    expect(candidateStatusCopy('available').selectable).toBe(true);
    for (const status of ['hidden', 'removed', 'unknown', 'quarantined']) {
      expect(candidateStatusCopy(status).selectable).toBe(false);
    }
    expect(candidateStatusCopy('quarantined').label).toMatch(/unrecognised/i);
  });

  it('renders an empty game slug as unstated, never as a game name or this game', () => {
    const copy = candidateTargetCopy(candidate({ game_slug: '', status: 'unknown' }), GAME);
    expect(copy.label).toBe('Target game not stated');
    expect(copy.label).not.toContain('""');
    expect(candidateTargetCopy(candidate(), GAME).label).toBe('This game');
    expect(candidateTargetCopy(candidate({ game_slug: 'fallout4' }), GAME).label).toBe(
      'Targets fallout4',
    );
  });

  it('never uses the opaque position or an empty identifier as a name', () => {
    expect(candidateLabel(candidate())).toBe('CET 1.35.0');
    expect(candidateLabel(candidate({ display_name: null }))).toBe('nexus:107');
    expect(candidateLabel(candidate({ display_name: null, provider_mod_id: '' }))).toBe(
      'Unnamed candidate',
    );
    for (const value of [candidate(), candidate({ position: null })]) {
      expect(candidateLabel(value)).not.toContain('2500000');
    }
  });

  it('keeps a known-missing DLC distinct from unknown ownership', () => {
    const dlc = { id: 'dlc-1', label: 'Phantom Liberty', alternatives: ['1091501'] };
    expect(dlcCopy({ ...dlc, ownership: 'owned' }).severity).toBe('neutral');
    expect(dlcCopy({ ...dlc, ownership: 'not_owned' }).label).toBe('Not owned');
    expect(dlcCopy({ ...dlc, ownership: 'not_owned' }).severity).toBe('danger');
    expect(dlcCopy({ ...dlc, ownership: 'unknown' }).label).toBe('Ownership unknown');
    expect(dlcCopy(dlc).label).toBe('Ownership unknown');
    expect(dlcCopy({ ...dlc, ownership: 'probably' }).label).toMatch(/unrecognised/i);
  });

  it('says "not owned" of an OR group only as every alternative being missing', () => {
    const group = {
      id: 'dlc-1',
      label: 'Either expansion',
      alternatives: ['1091501', '1091502'],
      ownership: 'not_owned',
    };
    expect(dlcCopy(group).detail).toMatch(/every alternative is missing/i);
    expect(dlcCopy({ ...group, alternatives: ['1091501'] }).detail).toMatch(/this DLC is missing/i);
  });

  it('is never more confident than the domain about a requirement naming no store item', () => {
    // `DlcRequirement::evaluate` returns Unknown for an empty alternatives
    // list, so no verdict arriving alongside one is believed.
    const empty = { id: 'dlc-1', label: 'Mystery', alternatives: [] };
    for (const copy of [
      dlcCopy(empty),
      ...(['owned', 'not_owned', 'unknown'] as const).map((ownership) =>
        dlcCopy({ ...empty, ownership }),
      ),
    ]) {
      expect(copy.label).toBe('Ownership unknown');
      expect(copy.severity).toBe('warning');
      expect(copy.detail).toMatch(/cannot be decided either way/i);
    }
  });
});

describe('explanations', () => {
  it('says which source requires what, and repeats the backend explanation verbatim', () => {
    const copy = requirementExplanation(requirement());
    expect(copy.label).toBe('nexus:107 requires Cyber Engine Tweaks');
    expect(copy.detail).toBe('no available candidate targets this game');
  });

  it('names an unlabelled requirement by its group id rather than by nothing', () => {
    expect(requirementExplanation(requirement({ label: null })).label).toContain('group-1');
  });

  it('reports an unidentified source instead of an empty provider mod id', () => {
    expect(sourceLabel(source({ provider_mod_id: '' }))).toBe('nexus:unidentified mod');
    expect(selectionLabel({ ...member().selection })).toBe('nexus:107 (9001)');
  });

  it('collects named requirements from both health rows and the outcome', () => {
    const rows = namedRequirements(
      result(
        { kind: 'unsatisfied', requirements: [requirement({ group_id: 'group-2' })] },
        {
          health: [
            { profile_member_id: 'member-1', health: 'unsatisfied', unsatisfied: [requirement()] },
          ],
        },
      ),
    );
    expect(rows.map((row) => row.group_id)).toEqual(['group-1', 'group-2']);
    expect(requirementsForMember(result({ kind: 'compatible' }), 'member-1')).toEqual([]);
  });

  it('does not guess a direction the provider did not state', () => {
    expect(changeCopy('downgrade').label).toBe('Downgrade');
    expect(changeCopy('upgrade').label).toBe('Upgrade');
    expect(changeCopy('install').label).toBe('Install');
    expect(changeCopy('unchanged').label).toBe('Unchanged');
    expect(changeCopy(null).label).toBe('Version change');
    expect(changeCopy(null).detail).toMatch(/never compares version strings/i);
    expect(changeCopy('sidegrade').label).toMatch(/unrecognised/i);
    expect(
      changeCopy('downgrade', 'Cyber Engine Tweaks 1.34 is the last compatible one.').detail,
    ).toContain('1.34');
  });
});

describe('solved plans', () => {
  const selection = {
    provider: 'nexus',
    provider_mod_id: '107',
    provider_file_id: '9002',
    provider_version_id: 'v-9002',
    provider_file_group_id: 'g-107',
    profile_member_id: 'member-1',
  };

  it('renders no proposal rows for an outcome that carries no plan', () => {
    expect(proposalChanges(result({ kind: 'compatible' }))).toEqual([]);
    expect(proposalChanges(result({ kind: 'unsatisfied', requirements: [requirement()] }))).toEqual(
      [],
    );
    expect(proposalChanges(result({ kind: 'unknown', reason: 'offline' }))).toEqual([]);
  });

  it('links a selected candidate back to the requirement that forced it', () => {
    const rows = proposalChanges(
      result(
        { kind: 'update_set', select: [{ ...selection, change: 'downgrade' }], install: [] },
        {
          health: [
            { profile_member_id: 'member-1', health: 'unsatisfied', unsatisfied: [requirement()] },
          ],
        },
      ),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0]?.kind).toBe('select');
    expect(rows[0]?.copy.label).toBe('Downgrade');
    expect(rows[0]?.because[0]?.label).toBe('Cyber Engine Tweaks');
  });

  it('labels an added candidate as an install', () => {
    const rows = proposalChanges(
      result({ kind: 'install_missing', install: [{ ...selection, profile_member_id: null }] }),
    );
    expect(rows[0]?.kind).toBe('install');
    expect(rows[0]?.copy.label).toBe('Install');
    expect(rows[0]?.because).toEqual([]);
  });

  it('explains a disable as the solver’s cardinality-minimal fallback', () => {
    const rows = proposalChanges(
      result(
        { kind: 'disable_set', disable: ['member-1'] },
        {
          health: [
            { profile_member_id: 'member-1', health: 'unsatisfied', unsatisfied: [requirement()] },
          ],
        },
      ),
    );
    expect(rows[0]?.kind).toBe('disable');
    expect(rows[0]?.copy.detail).toMatch(/smallest one/i);
    expect(rows[0]?.because).toHaveLength(1);
  });

  it('reports the pins that stand between the profile and a solution', () => {
    const unsolved = result(
      { kind: 'unsatisfied', requirements: [requirement()] },
      {
        health: [
          { profile_member_id: 'member-1', health: 'unsatisfied', unsatisfied: [requirement()] },
        ],
      },
    );
    const pinned = member({
      pin: { kind: 'pinned', pinned_at: '2026-08-20T09:00:00Z', reason: 'known-good with my save' },
    });
    expect(pinsBlockingSolution(unsolved, [pinned])).toHaveLength(1);
    expect(pinsBlockingSolution(unsolved, [member()])).toEqual([]);
    // A pin that merely constrains a solved plan is not reported as a blocker.
    expect(
      pinsBlockingSolution(result({ kind: 'update_set', select: [], install: [] }), [pinned]),
    ).toEqual([]);
  });
});

describe('action availability', () => {
  const ids = (value: ResolutionResult) => outcomeActions(value).map((action) => action.id);

  it('offers an apply action only for the variant that was actually solved', () => {
    expect(ids(result({ kind: 'install_missing', install: [] }))).toContain('install_missing');
    expect(ids(result({ kind: 'install_missing', install: [] }))).not.toContain('apply_update_set');
    expect(ids(result({ kind: 'update_set', select: [], install: [] }))).toContain(
      'apply_update_set',
    );
    expect(ids(result({ kind: 'disable_set', disable: [] }))).toContain('apply_disable_set');
  });

  it('offers no plan for unsatisfied, unknown, or an unrecognised outcome', () => {
    for (const outcome of [
      { kind: 'unsatisfied', requirements: [] } as DependencyOutcome,
      { kind: 'unknown', reason: 'offline' } as DependencyOutcome,
      { kind: 'a_new_idea' } as DependencyOutcome,
    ]) {
      const offered = ids(result(outcome));
      expect(offered).not.toContain('install_missing');
      expect(offered).not.toContain('apply_update_set');
      expect(offered).not.toContain('apply_disable_set');
      expect(offered).toContain('cancel');
    }
  });

  it('offers nothing but cancel when the profile is already compatible', () => {
    expect(ids(result({ kind: 'compatible' }))).toEqual(['cancel']);
  });

  it('offers an ignore only when there is a named requirement to attribute it to', () => {
    expect(ids(result({ kind: 'unsatisfied', requirements: [] }))).not.toContain(
      'ignore_requirement',
    );
    expect(ids(result({ kind: 'unsatisfied', requirements: [requirement()] }))).toContain(
      'ignore_requirement',
    );
  });

  it('always offers a way out that changes nothing', () => {
    for (const outcome of [
      { kind: 'compatible' } as DependencyOutcome,
      { kind: 'update_set', select: [], install: [] } as DependencyOutcome,
      { kind: 'unknown', reason: 'offline' } as DependencyOutcome,
    ]) {
      const cancel = outcomeActions(result(outcome)).find((action) => action.id === 'cancel');
      expect(cancel?.detail).toMatch(/leave desired state/i);
    }
  });

  it('offers a re-check for the two outcomes a fresh answer could change', () => {
    expect(ids(result({ kind: 'unknown', reason: 'offline' }))).toContain('replan');
    expect(ids(result({ kind: 'unsatisfied', requirements: [] }))).toContain('replan');
    expect(ids(result({ kind: 'update_set', select: [], install: [] }))).not.toContain('replan');
  });
});

describe('previewing an uncommitted edit', () => {
  it('offers only save and cancel, never a plan solved against an unsaved change', () => {
    const offered = editActions(result({ kind: 'update_set', select: [], install: [] }));
    expect(offered.map((action) => action.id)).toEqual(['save_edit', 'cancel']);
  });

  it('warns that saving does not clear a dependency problem', () => {
    expect(editActions(result({ kind: 'compatible' }))[0]!.detail).toMatch(/nothing is written/i);
    expect(editActions(result({ kind: 'unsatisfied', requirements: [] }))[0]!.detail).toMatch(
      /still blocks activation/i,
    );
  });

  it('always offers a cancel that changes nothing', () => {
    expect(editActions(result({ kind: 'unknown', reason: 'offline' }))[1]!.detail).toMatch(
      /leave desired state/i,
    );
  });
});

describe('ignoring a requirement', () => {
  it('requires a named group, the displayed fingerprint, and a non-empty reason', () => {
    const rejected = validateIgnore({ memberId: 'member-1' });
    expect(rejected.ok).toBe(false);
    if (!rejected.ok) {
      expect(rejected.problems).toHaveLength(3);
      expect(rejected.problems.join(' ')).toMatch(/fingerprint/i);
    }
    expect(
      validateIgnore({
        memberId: 'member-1',
        groupId: 'group-1',
        fingerprint: 'b3:definition',
        reason: '   ',
      }).ok,
    ).toBe(false);
  });

  it('accepts a complete decision and carries nothing that could pick a conflict winner', () => {
    const accepted = validateIgnore({
      memberId: ' member-1 ',
      groupId: 'group-1',
      fingerprint: 'b3:definition',
      reason: ' I know what I am doing ',
    });
    expect(accepted.ok).toBe(true);
    if (accepted.ok) {
      expect(Object.keys(accepted.request).sort()).toEqual([
        'fingerprint',
        'groupId',
        'memberId',
        'reason',
      ]);
      expect(accepted.request.memberId).toBe('member-1');
      expect(accepted.request.reason).toBe('I know what I am doing');
    }
  });
});

describe('stale plans', () => {
  it('recognises the conflict code and asks for a fresh plan', () => {
    expect(isStalePlan('conflict')).toBe(true);
    expect(isStalePlan('not_found')).toBe(false);
    expect(isStalePlan(null)).toBe(false);
    expect(isStalePlan(undefined)).toBe(false);
    expect(stalePlanCopy().detail).toMatch(/refused rather than applied/i);
    expect(stalePlanCopy().severity).toBe('warning');
  });
});

describe('whole-profile compatible updates', () => {
  const preview = (over: Partial<CompatibleUpdatePreview> = {}): CompatibleUpdatePreview => ({
    profile_id: 'profile-1',
    dependency: result({ kind: 'update_set', select: [], install: [] }),
    plan: { steps: [{ kind: 'write', target: 'game:a' }], conflicts: [] },
    downloads: [],
    bytes_to_write: 10,
    ready: true,
    blockers: [],
    fingerprint: 'b3:update',
    ...over,
  });

  it('frames the result as one whole-profile set, not per-mod latest versions', () => {
    const copy = compatibleUpdateCopy(preview());
    expect(copy.label).toMatch(/whole profile/i);
    expect(copy.detail).toMatch(/not chosen per mod/i);
  });

  it('separates "nothing to update" from "cannot say"', () => {
    const nothing = compatibleUpdateCopy(
      preview({
        dependency: result({ kind: 'compatible' }),
        plan: { steps: [], conflicts: [] },
      }),
    );
    expect(nothing.label).toBe('No compatible update');
    const offline = compatibleUpdateCopy(
      preview({
        dependency: result({ kind: 'unknown', reason: 'offline' }),
        plan: { steps: [], conflicts: [] },
      }),
    );
    expect(offline.label).toBe('Dependency compatibility unknown');
    expect(offline.severity).toBe('warning');
  });

  it('labels an up-to-date profile answered from incomplete data as such', () => {
    const copy = compatibleUpdateCopy(
      preview({
        dependency: result({ kind: 'compatible' }, { evidence: evidence({ stale: 2 }) }),
        plan: { steps: [], conflicts: [] },
      }),
    );
    expect(copy.detail).toMatch(/incomplete data/i);
  });

  it('never reports a rolled-back or unrecognised update as applied', () => {
    expect(compatibleUpdateReportCopy('applied').label).toBe('Profile updated');
    expect(compatibleUpdateReportCopy('rolled_back').severity).toBe('warning');
    expect(compatibleUpdateReportCopy('failed').label).toBe('Recovery required');
    expect(compatibleUpdateReportCopy('half_way').label).toBe('Update state unknown');
    expect(compatibleUpdateReportCopy('half_way').detail).toMatch(
      /must not be reported as updated/i,
    );
  });

  it('refuses to apply anything that is not ready, unblocked, solved and healthy', () => {
    expect(canApplyCompatibleUpdate(preview())).toBe(true);
    expect(canApplyCompatibleUpdate(preview({ ready: false }))).toBe(false);
    expect(canApplyCompatibleUpdate(preview({ blockers: [{ kind: 'cross_mod_conflict' }] }))).toBe(
      false,
    );
    expect(
      canApplyCompatibleUpdate(preview({ dependency: result({ kind: 'unknown', reason: 'x' }) })),
    ).toBe(false);
    expect(
      canApplyCompatibleUpdate(
        preview({
          dependency: result(
            { kind: 'update_set', select: [], install: [] },
            {
              health: [{ profile_member_id: 'member-1', health: 'unknown', unsatisfied: [] }],
            },
          ),
        }),
      ),
    ).toBe(false);
  });
});
