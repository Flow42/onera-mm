/** Pure presentation and safety rules for profile membership and activation. */

import {
  dependencyHealthCopy,
  dependencyOutcomeCopy,
  evidenceNotices,
  healthBlocksApply,
} from './dependency-view';
import type { StateCopy } from './dependency-view';
import type {
  MutationStep,
  MutationTarget,
  ProfileActivation,
  ProfileActivationPreview,
} from './types';

/**
 * Display copy for one profile state.
 *
 * The same shape the dependency view uses, so a route can render an activation
 * state and a dependency state through one code path.
 */
export type ProfileStateCopy = StateCopy;

// The dependency vocabulary lives in `dependency-view`; it is re-exported here
// because the profile route renders both and importing one module for member
// health and another for activation copy would only add a name to remember.
export { dependencyHealthCopy, dependencyOutcomeCopy, evidenceNotices };

/** The UI fails closed on dependency states that the contract says block apply. */
export function canActivate(preview: ProfileActivationPreview): boolean {
  if (!preview.ready || preview.blockers.length > 0) return false;
  if (
    preview.dependency.outcome.kind === 'unknown' ||
    preview.dependency.outcome.kind === 'unsatisfied'
  ) {
    return false;
  }
  return !preview.dependency.health.some((member) => healthBlocksApply(member.health));
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
