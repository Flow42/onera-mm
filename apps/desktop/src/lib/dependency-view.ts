/**
 * Pure presentation rules for provider-declared dependencies.
 *
 * Everything here is a total function over the serialised core shapes, so the
 * routes render what these return and the rules can be tested without a DOM.
 *
 * Three rules the whole module exists to hold:
 *
 * 1. **Unknown is not empty.** An unavailable, unsupported or unknown answer
 *    has its own state. None of them is ever rendered as "requires nothing",
 *    as a satisfied tick, or as a compatible outcome.
 * 2. **Only solved plans are offered.** An action appears for an outcome the
 *    backend actually returned a plan for, never for one the frontend could
 *    imagine.
 * 3. **Nothing is invented.** `position` is opaque and never displayed, version
 *    strings are never compared, and an unrecognised enum variant fails safe
 *    instead of being treated as the nearest known one.
 */

import type {
  CompatibleUpdatePreview,
  DependencyAvailability,
  DependencyCandidate,
  DependencyEvidence,
  DependencyGroup,
  DependencyHealthKind,
  DependencyOutcome,
  DependencySnapshot,
  DependencySource,
  DlcRequirement,
  MemberSelection,
  ProfileMember,
  ResolutionResult,
  SelectedVersion,
  UnsatisfiedRequirement,
} from './types';

/** How urgent a piece of copy is. Mirrors the `severity-*` classes. */
export type Severity = 'neutral' | 'info' | 'warning' | 'danger';

/** One rendered state: a short label, an explanation, and its urgency. */
export interface StateCopy {
  label: string;
  detail: string;
  severity: Severity;
}

/**
 * The copy for a value this build does not recognise.
 *
 * A newer backend enum variant must remain understandable and must never be
 * folded into the nearest known variant, so it gets its own warning state.
 */
function unrecognised(noun: string, value: string): StateCopy {
  return {
    label: `Unrecognised ${noun}`,
    detail: `This version of Onera does not recognise the ${noun} “${value}” and will not assume it is safe.`,
    severity: 'warning',
  };
}

// ---------------------------------------------------------------------------
// Member health, evidence and outcome
// ---------------------------------------------------------------------------

/** Dependency health copy. `unavailable` is the view state for a failed check. */
export function dependencyHealthCopy(health: DependencyHealthKind | 'unavailable' | string) {
  const states: Record<string, StateCopy> = {
    satisfied: {
      label: 'Satisfied',
      detail: 'The dependency check found every required mod.',
      severity: 'neutral',
    },
    unsatisfied: {
      label: 'Unsatisfied',
      detail: 'At least one required dependency is missing.',
      severity: 'danger',
    },
    ignored: {
      label: 'Risk accepted',
      detail: 'A dependency requirement was explicitly ignored for this definition.',
      severity: 'warning',
    },
    not_applicable: {
      label: 'Not applicable',
      detail: 'No dependency check applies to this member.',
      severity: 'neutral',
    },
    unknown: {
      label: 'Unknown',
      detail: 'Onera could not determine whether this member’s dependencies are satisfied.',
      severity: 'warning',
    },
    unavailable: {
      label: 'Unavailable',
      detail: 'The dependency check could not be loaded. This does not mean no dependencies.',
      severity: 'warning',
    },
  };
  return (
    states[health] ?? {
      label: 'Unknown',
      detail: `Onera does not recognise the dependency state “${health}”.`,
      severity: 'warning',
    }
  );
}

/** Whether a member's health stops an apply until the user decides. */
export function healthBlocksApply(health: DependencyHealthKind | string): boolean {
  // Anything this build cannot classify blocks too: an unrecognised state is
  // not evidence that the member is fine.
  return health !== 'satisfied' && health !== 'ignored' && health !== 'not_applicable';
}

/** One disclosure per incomplete evidence class; none is collapsed into another. */
export function evidenceNotices(evidence: DependencyEvidence): StateCopy[] {
  const notices: StateCopy[] = [];
  if (evidence.cached > 0) {
    notices.push({
      label: 'Cached dependency data',
      detail: `${evidence.cached} result(s) came from the local cache.`,
      severity: 'info',
    });
  }
  if (evidence.stale > 0) {
    notices.push({
      label: 'Stale dependency data',
      detail: `${evidence.stale} cached result(s) may no longer match the provider.`,
      severity: 'warning',
    });
  }
  if (evidence.unavailable > 0) {
    notices.push({
      label: 'Dependency data unavailable',
      detail: `${evidence.unavailable} mod(s) could not be checked.`,
      severity: 'warning',
    });
  }
  if (evidence.unsupported > 0) {
    notices.push({
      label: 'Dependencies unsupported',
      detail: `${evidence.unsupported} mod(s) use a provider without dependency metadata support.`,
      severity: 'warning',
    });
  }
  if (evidence.unknown_dlc > 0) {
    notices.push({
      label: 'DLC ownership unknown',
      detail: `Ownership could not be determined for ${evidence.unknown_dlc} DLC requirement(s).`,
      severity: 'warning',
    });
  }
  return notices;
}

/**
 * Whether the answer rests on complete, current data.
 *
 * Offline operation is the case this exists for: cached data is usable and
 * labelled, and is never described as current.
 */
export function evidenceIsCurrent(evidence: DependencyEvidence): boolean {
  return (
    evidence.cached === 0 &&
    evidence.stale === 0 &&
    evidence.unavailable === 0 &&
    evidence.unsupported === 0 &&
    evidence.unknown_dlc === 0
  );
}

/** Overall dependency result; only results actually solved are called actionable. */
export function dependencyOutcomeCopy(result: ResolutionResult): StateCopy {
  const outcome = result.outcome;
  switch (outcome.kind) {
    case 'compatible':
      return {
        label: 'Compatible',
        detail: 'Enabled members satisfy the requirements Onera could check.',
        severity: 'neutral',
      };
    case 'install_missing':
      return {
        label: 'Missing dependencies can be installed',
        detail: 'The activation plan includes the solved missing set.',
        severity: 'info',
      };
    case 'update_set':
      return {
        label: 'Compatible update set available',
        detail: 'The activation plan includes the solved version changes.',
        severity: 'info',
      };
    case 'disable_set':
      return {
        label: 'Disable set available',
        detail: 'The activation plan includes the solved member changes.',
        severity: 'warning',
      };
    case 'unsatisfied':
      return {
        label: 'Dependencies unsatisfied',
        detail: 'No compatible set was found for every enabled member.',
        severity: 'danger',
      };
    case 'unknown':
      return {
        label: 'Dependency compatibility unknown',
        detail:
          'reason' in outcome && typeof outcome.reason === 'string' && outcome.reason !== ''
            ? `No compatibility claim or solution is available: ${outcome.reason}`
            : 'No compatibility claim or solution is available.',
        severity: 'warning',
      };
    default:
      return {
        label: 'Dependency compatibility unknown',
        detail: 'Onera does not recognise this dependency outcome.',
        severity: 'warning',
      };
  }
}

/** Whether an outcome carries a plan the user may accept. */
export function offersAPlan(outcome: DependencyOutcome): boolean {
  return (
    outcome.kind === 'install_missing' ||
    outcome.kind === 'update_set' ||
    outcome.kind === 'disable_set'
  );
}

/** Whether an outcome permits an apply with no further decision. */
export function outcomeIsApplyReady(outcome: DependencyOutcome): boolean {
  return outcome.kind === 'compatible';
}

// ---------------------------------------------------------------------------
// Identifiers and labels
// ---------------------------------------------------------------------------

/**
 * A displayable name for the version that stated a requirement.
 *
 * An empty `provider_mod_id` is a real value meaning the provider did not say,
 * so it is reported as such rather than rendered as an empty name.
 */
export function sourceLabel(source: DependencySource): string {
  const mod = source.provider_mod_id === '' ? 'unidentified mod' : source.provider_mod_id;
  return `${source.provider}:${mod}`;
}

/** How a candidate is named. Never a version string, never its position. */
export function candidateLabel(candidate: DependencyCandidate): string {
  if (candidate.display_name !== null && candidate.display_name !== '') {
    return candidate.display_name;
  }
  if (candidate.provider_mod_id !== '') {
    return `${candidate.provider}:${candidate.provider_mod_id}`;
  }
  return 'Unnamed candidate';
}

/** How a member's chosen file is named. Version strings are display-only. */
export function selectionLabel(selection: MemberSelection | SelectedVersion): string {
  const mod = selection.provider_mod_id === '' ? 'unidentified mod' : selection.provider_mod_id;
  const file = selection.provider_file_id;
  return file === null || file === ''
    ? `${selection.provider}:${mod}`
    : `${selection.provider}:${mod} (${file})`;
}

// ---------------------------------------------------------------------------
// Snapshot detail
// ---------------------------------------------------------------------------

/** How much a snapshot's contents can be believed. */
export function availabilityCopy(availability: DependencyAvailability): StateCopy {
  switch (availability.kind) {
    case 'fetched':
      return {
        label: 'Fetched',
        detail:
          'Read from the provider directly. An empty requirement list means it requires nothing.',
        severity: 'neutral',
      };
    case 'cached':
      return availability.stale
        ? {
            label: 'Stale cache',
            detail: `Answered from a stored snapshot fetched ${availability.fetched_at}, past its freshness window. Usable, but not current.`,
            severity: 'warning',
          }
        : {
            label: 'Cached',
            detail: `Answered from a stored snapshot fetched ${availability.fetched_at}, not re-checked against the provider.`,
            severity: 'info',
          };
    case 'unsupported':
      return {
        label: 'Not supported by this provider',
        detail:
          'This provider does not model dependencies. That is not the same as requiring nothing.',
        severity: 'warning',
      };
    case 'unavailable':
      return {
        label: 'Unavailable',
        detail: `Onera could not ask the provider (${availability.reason}). That is not the same as requiring nothing.`,
        severity: 'warning',
      };
    default:
      return unrecognised('availability', (availability as { kind: string }).kind);
  }
}

/** Whether the snapshot's contents are the provider's real answer. */
export function isAuthoritative(availability: DependencyAvailability): boolean {
  return availability.kind === 'fetched' || availability.kind === 'cached';
}

/** Whether the data is known to be out of date. */
export function isStaleSnapshot(availability: DependencyAvailability): boolean {
  return availability.kind === 'cached' && availability.stale;
}

/**
 * Whether an empty requirement list may be read as "requires nothing".
 *
 * Only an authoritative answer earns that reading.
 */
export function declaresNoDependencies(snapshot: DependencySnapshot): boolean {
  return (
    isAuthoritative(snapshot.availability) &&
    snapshot.groups.length === 0 &&
    snapshot.dlc.length === 0
  );
}

/** The one-line summary above a snapshot's requirement list. */
export function snapshotSummary(snapshot: DependencySnapshot): StateCopy {
  if (declaresNoDependencies(snapshot)) {
    return {
      label: 'Requires nothing',
      detail: 'The provider answered, and stated no requirements for this file.',
      severity: 'neutral',
    };
  }
  if (!isAuthoritative(snapshot.availability)) {
    return {
      label: 'Requirements not known',
      detail:
        'Onera has no answer for this file. An empty list here is a missing answer, not an empty requirement set.',
      severity: 'warning',
    };
  }
  const blocking = snapshot.groups.filter((group) => requirementKindCopy(group.kind).blocks).length;
  return {
    label: `${snapshot.groups.length} requirement group(s)`,
    detail: `${blocking} of them can block an apply. Groups are combined with AND; the candidates inside one are alternatives.`,
    severity: blocking > 0 ? 'info' : 'neutral',
  };
}

/** How strongly a requirement is stated, and whether it can block. */
export function requirementKindCopy(kind: string): StateCopy & { blocks: boolean } {
  switch (kind) {
    case 'required':
      return {
        label: 'Required',
        detail: 'One of these candidates must be selected.',
        severity: 'info',
        blocks: true,
      };
    case 'recommended':
      return {
        label: 'Recommended',
        detail: 'Advisory only. This is reported and never blocks an apply.',
        severity: 'neutral',
        blocks: false,
      };
    case 'incompatible':
      return {
        label: 'Incompatible',
        detail: 'None of these candidates may be selected alongside the source mod.',
        severity: 'warning',
        blocks: true,
      };
    default:
      // Fail safe: an unrecognised strength is treated as blocking, because
      // guessing "advisory" would silently drop a real requirement.
      return { ...unrecognised('requirement kind', kind), blocks: true };
  }
}

/** Whether a candidate may be selected: right game, selectable status. */
export function isCandidateSelectable(candidate: DependencyCandidate, gameSlug: string): boolean {
  return candidate.status === 'available' && candidate.game_slug === gameSlug;
}

/** What a candidate's provider status means for selection. */
export function candidateStatusCopy(status: string): StateCopy & { selectable: boolean } {
  switch (status) {
    case 'available':
      return {
        label: 'Available',
        detail: 'Visible and downloadable.',
        severity: 'neutral',
        selectable: true,
      };
    case 'hidden':
      return {
        label: 'Hidden by the author',
        detail: 'Still exists but is no longer offered, so it cannot be selected.',
        severity: 'warning',
        selectable: false,
      };
    case 'removed':
      return {
        label: 'Removed',
        detail: 'Deleted or archived by the provider. It cannot be selected.',
        severity: 'warning',
        selectable: false,
      };
    case 'unknown':
      return {
        label: 'Status unknown',
        detail:
          'The provider did not say. Onera never guesses that an unknown candidate would work.',
        severity: 'warning',
        selectable: false,
      };
    default:
      return { ...unrecognised('candidate status', status), selectable: false };
  }
}

/**
 * Which game a candidate targets.
 *
 * An empty slug is a real value: the provider did not say. It is never shown
 * as a game name and never assumed to be the current game.
 */
export function candidateTargetCopy(candidate: DependencyCandidate, gameSlug: string): StateCopy {
  if (candidate.game_slug === '') {
    return {
      label: 'Target game not stated',
      detail:
        'The provider did not say which game this candidate targets, so it cannot be selected.',
      severity: 'warning',
    };
  }
  if (candidate.game_slug === gameSlug) {
    return { label: 'This game', detail: 'Targets the game being modded.', severity: 'neutral' };
  }
  return {
    label: `Targets ${candidate.game_slug}`,
    detail:
      'A candidate for another game is never a valid selection, however confidently it was offered.',
    severity: 'warning',
  };
}

/** Whether a group can be satisfied at all, and how to say why not. */
export function groupState(
  group: DependencyGroup,
  gameSlug: string,
): StateCopy & { satisfiable: boolean } {
  const kind = requirementKindCopy(group.kind);
  if (group.kind === 'incompatible') {
    return {
      label: 'Must not be installed together',
      detail: `${group.candidates.length} named candidate(s) conflict with the source mod.`,
      severity: 'warning',
      satisfiable: true,
    };
  }
  if (group.candidates.length === 0) {
    return {
      label: 'Nothing can satisfy this',
      detail:
        'The provider stated a requirement and listed no candidate for it. An empty candidate list is not a satisfied one.',
      severity: kind.blocks ? 'danger' : 'warning',
      satisfiable: false,
    };
  }
  if (group.candidates.some((candidate) => isCandidateSelectable(candidate, gameSlug))) {
    return {
      label: 'Can be satisfied',
      detail: 'At least one candidate is available for this game.',
      severity: 'neutral',
      satisfiable: true,
    };
  }
  const wrongGame = group.candidates.every(
    (candidate) => candidate.game_slug !== gameSlug && candidate.game_slug !== '',
  );
  if (wrongGame) {
    return {
      label: 'No candidate targets this game',
      detail: 'Every candidate the provider offered belongs to a different game.',
      severity: kind.blocks ? 'danger' : 'warning',
      satisfiable: false,
    };
  }
  const withdrawn = group.candidates.some(
    (candidate) => candidate.status === 'hidden' || candidate.status === 'removed',
  );
  return {
    label: withdrawn ? 'Every candidate has been withdrawn' : 'No candidate is selectable',
    detail: withdrawn
      ? 'The candidates exist but the provider no longer offers them. This is not the same as no candidate existing.'
      : 'No candidate is both available and targeted at this game.',
    severity: kind.blocks ? 'danger' : 'warning',
    satisfiable: false,
  };
}

/**
 * A DLC requirement, keeping known-missing distinct from unknown ownership.
 *
 * `ownership` is the *requirement's* verdict, not one store item's, and the
 * domain resolves an alternatives list three ways: owned if any alternative is
 * owned, not owned only if every alternative is confirmed not owned, and
 * unknown if any alternative is unknown **or the list is empty**
 * (`DlcRequirement::evaluate`). Whoever populates the field must mirror that;
 * this function refuses to be more confident than the domain regardless, and
 * reports an empty list as unknown even if a verdict came with it.
 */
export function dlcCopy(requirement: DlcRequirement): StateCopy {
  const count = requirement.alternatives.length;
  const alternatives =
    count === 0
      ? 'The provider named no store item for it.'
      : count === 1
        ? 'One store item satisfies it.'
        : `Any one of ${count} store items satisfies it.`;
  const unknown: StateCopy = {
    label: 'Ownership unknown',
    detail: `The store gave no ownership answer for every alternative. Unknown is never counted as owned. ${alternatives}`,
    severity: 'warning',
  };
  // A requirement naming no store item cannot be evaluated at all, so no
  // verdict attached to it is believed.
  if (count === 0) {
    return {
      ...unknown,
      detail: `This requirement names no store item, so ownership of it cannot be decided either way. ${alternatives}`,
    };
  }
  switch (requirement.ownership) {
    case 'owned':
      return {
        label: 'Owned',
        detail: `The store confirmed you own ${count === 1 ? 'it' : 'one of these'}. ${alternatives}`,
        severity: 'neutral',
      };
    case 'not_owned':
      return {
        label: 'Not owned',
        detail: `The store confirmed ${count === 1 ? 'this DLC is' : 'every alternative is'} missing, so the requirement cannot be met. ${alternatives}`,
        severity: 'danger',
      };
    case 'unknown':
    case undefined:
    case null:
      return unknown;
    default:
      return unrecognised('DLC ownership', String(requirement.ownership));
  }
}

// ---------------------------------------------------------------------------
// Explanations
// ---------------------------------------------------------------------------

/** One unsatisfied requirement, said as source → requirement → why. */
export function requirementExplanation(requirement: UnsatisfiedRequirement): StateCopy {
  const named =
    requirement.label !== null && requirement.label !== ''
      ? requirement.label
      : `requirement ${requirement.group_id}`;
  return {
    label: `${sourceLabel(requirement.source)} requires ${named}`,
    detail: requirement.explanation,
    severity: 'danger',
  };
}

/** Every unsatisfied requirement the result names, from health and outcome. */
export function namedRequirements(result: ResolutionResult): UnsatisfiedRequirement[] {
  const rows = result.health.flatMap((member) => member.unsatisfied);
  const outcome = result.outcome as { kind: string; requirements?: UnsatisfiedRequirement[] };
  if (outcome.kind === 'unsatisfied' && Array.isArray(outcome.requirements)) {
    rows.push(...outcome.requirements);
  }
  const seen = new Set<string>();
  return rows.filter((row) => {
    const key = `${row.group_id}${sourceLabel(row.source)}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** The unsatisfied requirements recorded against one member. */
export function requirementsForMember(
  result: ResolutionResult,
  memberId: string,
): UnsatisfiedRequirement[] {
  return result.health
    .filter((member) => member.profile_member_id === memberId)
    .flatMap((member) => member.unsatisfied);
}

/**
 * Why a selection changes.
 *
 * Direction is the backend's to state. Onera never compares version strings and
 * `position` is opaque, so an absent `change` is reported as an unexplained
 * version change rather than guessed at.
 */
export function changeCopy(change: string | null | undefined, reason?: string | null): StateCopy {
  const suffix = reason !== null && reason !== undefined && reason !== '' ? ` ${reason}` : '';
  switch (change) {
    case 'upgrade':
      return {
        label: 'Upgrade',
        detail: `The solver moved this mod to a later file in its group.${suffix}`,
        severity: 'info',
      };
    case 'downgrade':
      return {
        label: 'Downgrade',
        detail: `The solver moved this mod to an earlier file in its group to satisfy a requirement.${suffix}`,
        severity: 'warning',
      };
    case 'install':
      return {
        label: 'Install',
        detail: `This file is added to satisfy a requirement.${suffix}`,
        severity: 'info',
      };
    case 'unchanged':
      return {
        label: 'Unchanged',
        detail: `The current selection is kept.${suffix}`,
        severity: 'neutral',
      };
    case null:
    case undefined:
    case '':
      return {
        label: 'Version change',
        detail: `The solver selected a different file. The provider did not report whether that is newer or older, and Onera never compares version strings.${suffix}`,
        severity: 'info',
      };
    default:
      return unrecognised('change', change);
  }
}

/** One row of a solved plan the user is being asked to accept. */
export interface ProposalChange {
  kind: 'install' | 'select' | 'disable';
  key: string;
  label: string;
  profileMemberId: string | null;
  selection: SelectedVersion | null;
  copy: StateCopy;
  because: UnsatisfiedRequirement[];
}

/**
 * Flatten a solved outcome into displayable rows.
 *
 * Returns an empty list for every outcome that carries no plan, so a caller
 * cannot accidentally render a proposal for `unknown` or `unsatisfied`.
 */
export function proposalChanges(result: ResolutionResult): ProposalChange[] {
  const outcome = result.outcome;
  const rows: ProposalChange[] = [];
  const select = 'select' in outcome && Array.isArray(outcome.select) ? outcome.select : [];
  const install = 'install' in outcome && Array.isArray(outcome.install) ? outcome.install : [];
  const disable = 'disable' in outcome && Array.isArray(outcome.disable) ? outcome.disable : [];
  if (!offersAPlan(outcome)) return rows;

  for (const [index, selection] of select.entries()) {
    rows.push({
      kind: 'select',
      key: `select-${index}-${selection.provider_file_id}`,
      label: selection.display_name ?? selectionLabel(selection),
      profileMemberId: selection.profile_member_id,
      selection,
      copy: changeCopy(selection.change, selection.reason),
      because:
        selection.profile_member_id === null
          ? []
          : requirementsForMember(result, selection.profile_member_id),
    });
  }
  for (const [index, selection] of install.entries()) {
    rows.push({
      kind: 'install',
      key: `install-${index}-${selection.provider_file_id}`,
      label: selection.display_name ?? selectionLabel(selection),
      profileMemberId: selection.profile_member_id,
      selection,
      copy: changeCopy(selection.change ?? 'install', selection.reason),
      because:
        selection.profile_member_id === null
          ? []
          : requirementsForMember(result, selection.profile_member_id),
    });
  }
  for (const [index, memberId] of disable.entries()) {
    rows.push({
      kind: 'disable',
      key: `disable-${index}-${memberId}`,
      label: `Member ${memberId}`,
      profileMemberId: memberId,
      selection: null,
      copy: {
        label: 'Disable',
        detail:
          'Disabling is the solver’s last resort, and the set it returned is the smallest one that makes the rest valid.',
        severity: 'warning',
      },
      because: requirementsForMember(result, memberId),
    });
  }
  return rows;
}

/**
 * Pins that stand between the profile and a solution.
 *
 * Only reported for an outcome with no solution: a pin that merely constrains a
 * solved plan is not a problem the user has to hear about.
 */
export function pinsBlockingSolution(
  result: ResolutionResult,
  members: ProfileMember[],
): { member: ProfileMember; requirements: UnsatisfiedRequirement[] }[] {
  if (result.outcome.kind !== 'unsatisfied') return [];
  const sources = new Set(namedRequirements(result).map((row) => row.source.provider_mod_id));
  return members
    .filter((member) => member.pin.kind === 'pinned')
    .filter(
      (member) =>
        sources.has(member.selection.provider_mod_id) ||
        requirementsForMember(result, member.id).length > 0,
    )
    .map((member) => ({ member, requirements: requirementsForMember(result, member.id) }));
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/** The actions a dependency result can offer. */
export type DependencyActionId =
  | 'save_edit'
  | 'install_missing'
  | 'apply_update_set'
  | 'apply_disable_set'
  | 'change_pins'
  | 'ignore_requirement'
  | 'replan'
  | 'cancel';

export interface DependencyAction {
  id: DependencyActionId;
  label: string;
  detail: string;
  primary: boolean;
  destructive: boolean;
}

/**
 * The buttons to offer for one result.
 *
 * An apply action exists only for a variant the backend actually solved, so a
 * frontend that grew a new button could not offer a plan that does not exist.
 * Ignoring is offered only when there is a named requirement to attribute the
 * risk to, and Cancel is always offered.
 */
export function outcomeActions(result: ResolutionResult): DependencyAction[] {
  const actions: DependencyAction[] = [];
  switch (result.outcome.kind) {
    case 'install_missing':
      actions.push({
        id: 'install_missing',
        label: 'Install missing requirements',
        detail: 'Add the solved candidates to this profile’s desired state.',
        primary: true,
        destructive: false,
      });
      break;
    case 'update_set':
      actions.push({
        id: 'apply_update_set',
        label: 'Update and downgrade to the compatible set',
        detail: 'Apply every version change the solver returned, as one set.',
        primary: true,
        destructive: false,
      });
      break;
    case 'disable_set':
      actions.push({
        id: 'apply_disable_set',
        label: 'Disable the proposed members',
        detail: 'Disable the smallest set of members that makes the rest valid.',
        primary: true,
        destructive: true,
      });
      break;
    default:
      break;
  }
  if (!outcomeIsApplyReady(result.outcome)) {
    actions.push({
      id: 'change_pins',
      label: 'Change pins and solve again',
      detail: 'Pinned members cannot change version. Unpinning may make a solution possible.',
      primary: false,
      destructive: false,
    });
    if (namedRequirements(result).length > 0) {
      actions.push({
        id: 'ignore_requirement',
        label: 'Ignore a requirement at my own risk',
        detail: 'Records a named, attributable exception. It does not resolve any file conflict.',
        primary: false,
        destructive: true,
      });
    }
    if (result.outcome.kind === 'unknown' || result.outcome.kind === 'unsatisfied') {
      actions.push({
        id: 'replan',
        label: 'Check again',
        detail: 'Re-run the dependency check against the provider.',
        primary: false,
        destructive: false,
      });
    }
  }
  actions.push({
    id: 'cancel',
    label: 'Cancel',
    detail: 'Leave desired state and the game directory exactly as they are.',
    primary: false,
    destructive: false,
  });
  return actions;
}

/**
 * The buttons to offer while previewing an uncommitted desired-state edit.
 *
 * No apply action appears here: a plan solved against a change that has not
 * been saved is not something to accept. The edit is saved or abandoned, and
 * abandoning it leaves desired and active state untouched.
 */
export function editActions(result: ResolutionResult): DependencyAction[] {
  const blocked = !outcomeIsApplyReady(result.outcome);
  return [
    {
      id: 'save_edit',
      label: 'Save this change',
      detail: blocked
        ? 'Saves the desired-state edit. The dependency problem above still blocks activation until it is resolved.'
        : 'Saves the desired-state edit. Nothing is written to the game directory.',
      primary: true,
      destructive: false,
    },
    {
      id: 'cancel',
      label: 'Cancel',
      detail: 'Leave desired state and the game directory exactly as they are.',
      primary: false,
      destructive: false,
    },
  ];
}

// ---------------------------------------------------------------------------
// Ignoring a requirement
// ---------------------------------------------------------------------------

/**
 * The arguments of one ignore decision.
 *
 * Deliberately carries no target, choice or scope: a dependency override says
 * "this named requirement may go unmet", and can never be used to pick the
 * winner of a filesystem path conflict. Those are decided by `decide`, which
 * this shape cannot express.
 */
export interface IgnoreRequest {
  memberId: string;
  groupId: string;
  fingerprint: string;
  reason: string;
}

export type IgnoreValidation =
  { ok: true; request: IgnoreRequest } | { ok: false; problems: string[] };

/**
 * Validate an ignore before it is sent.
 *
 * The fingerprint must be the one that was displayed with the requirement, so
 * accepting a risk cannot silently cover a definition the user never saw.
 */
export function validateIgnore(draft: Partial<IgnoreRequest>): IgnoreValidation {
  const problems: string[] = [];
  const memberId = (draft.memberId ?? '').trim();
  const groupId = (draft.groupId ?? '').trim();
  const fingerprint = (draft.fingerprint ?? '').trim();
  const reason = (draft.reason ?? '').trim();
  if (memberId === '') problems.push('Choose the profile member the requirement belongs to.');
  if (groupId === '') problems.push('Choose the named requirement to ignore.');
  if (fingerprint === '') {
    problems.push(
      'The dependency fingerprint that was displayed is missing. Reload the requirement before accepting a risk.',
    );
  }
  if (reason === '')
    problems.push('Give a reason. Ignoring a requirement is an attributable decision.');
  return problems.length > 0
    ? { ok: false, problems }
    : { ok: true, request: { memberId, groupId, fingerprint, reason } };
}

// ---------------------------------------------------------------------------
// Stale plans
// ---------------------------------------------------------------------------

/** Whether a failure means the plan moved and the user needs a fresh one. */
export function isStalePlan(code: string | null | undefined): boolean {
  return code === 'conflict';
}

/** What to say when an approved plan no longer matches the current state. */
export function stalePlanCopy(): StateCopy {
  return {
    label: 'This plan is out of date',
    detail:
      'The profile or its dependency data changed after this plan was shown, so it was refused rather than applied. Check again to see the current one.',
    severity: 'warning',
  };
}

// ---------------------------------------------------------------------------
// Whole-profile compatible updates
// ---------------------------------------------------------------------------

/**
 * The headline for a compatible-update preview.
 *
 * "Update all compatible" is one solve of the whole enabled profile. There is
 * no per-mod latest version to accept independently, so the copy never implies
 * one.
 */
export function compatibleUpdateCopy(preview: CompatibleUpdatePreview): StateCopy {
  const outcome = dependencyOutcomeCopy(preview.dependency);
  if (preview.dependency.outcome.kind === 'compatible' && preview.plan.steps.length === 0) {
    return {
      label: 'No compatible update',
      detail: evidenceIsCurrent(preview.dependency.evidence)
        ? 'The whole profile already solves to its current versions.'
        : 'The whole profile solves to its current versions, but the answer rests on incomplete data.',
      severity: 'neutral',
    };
  }
  if (offersAPlan(preview.dependency.outcome)) {
    return {
      label: 'One compatible set for the whole profile',
      detail:
        'These changes were solved together. They are accepted or refused as one set, not chosen per mod.',
      severity: 'info',
    };
  }
  return outcome;
}

/**
 * The terminal state of one applied whole-profile update.
 *
 * Shares the activation vocabulary because it is the same journaled machinery:
 * only `applied` means the files on disk match the solved set, and every other
 * state leaves the profile as it was.
 */
export function compatibleUpdateReportCopy(state: string): StateCopy {
  switch (state) {
    case 'applied':
      return {
        label: 'Profile updated',
        detail:
          'Filesystem verification passed; the profile now deploys the solved compatible set.',
        severity: 'neutral',
      };
    case 'rolled_back':
      return {
        label: 'Update rolled back',
        detail:
          'The profile still deploys the versions it did before. Nothing was left half-applied.',
        severity: 'warning',
      };
    case 'failed':
      return {
        label: 'Recovery required',
        detail:
          'Rollback could not finish, so the deployed set is not known to match either version.',
        severity: 'danger',
      };
    case 'preparing':
      return {
        label: 'Preparing update',
        detail: 'Downloads are being staged. The game directory has not changed.',
        severity: 'info',
      };
    case 'applying':
      return {
        label: 'Applying update',
        detail: 'The journaled filesystem change is in progress.',
        severity: 'info',
      };
    default:
      return {
        label: 'Update state unknown',
        detail: 'The profile must not be reported as updated.',
        severity: 'warning',
      };
  }
}

/** Whether a whole-profile update may be applied. Fails closed. */
export function canApplyCompatibleUpdate(preview: CompatibleUpdatePreview): boolean {
  if (!preview.ready || preview.blockers.length > 0) return false;
  if (!offersAPlan(preview.dependency.outcome)) return false;
  return !preview.dependency.health.some((member) => healthBlocksApply(member.health));
}
