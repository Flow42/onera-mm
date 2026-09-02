/** Pure presentation and safety rules for profile membership and activation. */

import type {
  DependencyEvidence,
  DependencyHealthKind,
  MutationStep,
  MutationTarget,
  ProfileActivation,
  ProfileActivationPreview,
  ResolutionResult,
} from './types';

export interface ProfileStateCopy {
  label: string;
  detail: string;
  severity: 'neutral' | 'info' | 'warning' | 'danger';
}

/** Dependency health copy. `unavailable` is the view state for a failed check. */
export function dependencyHealthCopy(health: DependencyHealthKind | 'unavailable' | string) {
  const states: Record<string, ProfileStateCopy> = {
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

/** One disclosure per incomplete evidence class; none is collapsed into another. */
export function evidenceNotices(evidence: DependencyEvidence): ProfileStateCopy[] {
  const notices: ProfileStateCopy[] = [];
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

/** Overall dependency result; only results actually solved are called actionable. */
export function dependencyOutcomeCopy(result: ResolutionResult): ProfileStateCopy {
  switch (result.outcome.kind) {
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
        detail: 'No compatibility claim or solution is available.',
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

/** The UI fails closed on dependency states that the contract says block apply. */
export function canActivate(preview: ProfileActivationPreview): boolean {
  if (!preview.ready || preview.blockers.length > 0) return false;
  if (
    preview.dependency.outcome.kind === 'unknown' ||
    preview.dependency.outcome.kind === 'unsatisfied'
  ) {
    return false;
  }
  return !preview.dependency.health.some(
    (member) => member.health === 'unknown' || member.health === 'unsatisfied',
  );
}

export function formatTarget(target: MutationTarget): string {
  return typeof target === 'string' ? target : `${target.root_key}:${target.path}`;
}

/** Human copy for documented write/delete steps and future kinds. */
export function mutationStepCopy(step: MutationStep): ProfileStateCopy {
  if (step.kind === 'write' && step.provider?.provider.kind === 'unmanaged_backup') {
    return {
      label: 'Restore covered file',
      detail: 'Restore the pre-Onera bytes recorded beneath the selected provider stack.',
      severity: 'info',
    };
  }
  if (step.kind === 'write' && step.expected_previous === null) {
    return {
      label: 'Activate retained file',
      detail: 'Deploy verified bytes from the selected retained artifact.',
      severity: 'info',
    };
  }
  const known: Record<string, ProfileStateCopy> = {
    write: {
      label: 'Change selected version',
      detail:
        'Stage verified bytes, then atomically switch this path for an activation, upgrade, or downgrade.',
      severity: 'info',
    },
    delete: {
      label: 'Deactivate deployed file',
      detail: 'Remove this deployed path because no selected provider remains.',
      severity: 'warning',
    },
  };
  return (
    known[step.kind] ?? {
      label: `Unknown change (${step.kind})`,
      detail: 'This frontend does not recognise the filesystem change and will not call it safe.',
      severity: 'warning',
    }
  );
}

/** Parse a signed i32 priority without turning blank or fractional input into zero. */
export function signedPriority(value: string): number | null {
  if (!/^-?\d+$/.test(value.trim())) return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed >= -2_147_483_648 && parsed <= 2_147_483_647
    ? parsed
    : null;
}

/** Terminal activation copy keeps success, rollback, and recovery failure distinct. */
export function activationCopy(activation: ProfileActivation): ProfileStateCopy {
  switch (activation.state) {
    case 'applied':
      return {
        label: 'Profile activated',
        detail: 'Filesystem verification passed; the target profile is now active.',
        severity: 'neutral',
      };
    case 'rolled_back':
      return {
        label: 'Activation rolled back',
        detail: 'The previous profile remains active and its filesystem state was restored.',
        severity: 'warning',
      };
    case 'failed':
      return {
        label: 'Recovery required',
        detail: 'Rollback could not finish. The target profile was not marked active.',
        severity: 'danger',
      };
    case 'applying':
      return {
        label: 'Applying profile',
        detail: 'The journaled filesystem change is in progress.',
        severity: 'info',
      };
    case 'preparing':
      return {
        label: 'Preparing activation',
        detail: 'Downloads and filesystem changes are being staged; nothing is active yet.',
        severity: 'info',
      };
    default:
      return {
        label: 'Activation state unknown',
        detail: 'The target profile must not be reported active.',
        severity: 'warning',
      };
  }
}
