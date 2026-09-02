/**
 * View-model helpers for the Game Integrity panel.
 *
 * Pure, so the rules that matter most can be tested without a DOM. Three of
 * them exist only to stop the interface saying something untrue:
 *
 * 1. `unknown` freshness is its own state. It is never drawn as `fresh`.
 * 2. a `local_snapshot` baseline is labelled as one. It proves the files have
 *    not changed since Onera looked, never that they were ever correct.
 * 3. a metadata-only verification is never "clean", however good its counts
 *    look — matching the `BaselineVerification::is_clean` rule in the core.
 *
 * A classification this build does not recognise fails safe: it is treated as a
 * difference that needs a decision, exactly as the plan view does.
 */

import type {
  BaselineFreshness,
  BaselineSource,
  BaselineVerification,
  FileClassification,
  GameBaseline,
  StoreBuildIdentity,
} from './types';

/** How something is described to the user, and how urgent it is. */
export interface StateCopy {
  label: string;
  detail: string;
  severity: 'neutral' | 'info' | 'warning' | 'danger';
}

/** Copy for a freshness state, including the two identities of a stale one. */
export function freshnessCopy(freshness: BaselineFreshness): StateCopy {
  switch (freshness.kind) {
    case 'fresh':
      return {
        label: 'Fresh',
        detail: 'The game build is unchanged since the baseline was captured.',
        severity: 'neutral',
      };
    case 'none':
      return {
        label: 'Not captured',
        detail: 'Onera has no record of what this installation looks like when clean.',
        severity: 'info',
      };
    case 'stale':
      return {
        label: 'Stale',
        detail:
          `The store's build changed from ${buildLabel(freshness.captured)} to ` +
          `${buildLabel(freshness.observed)}. Run the store's own file verification, then ` +
          'replace the baseline.',
        severity: 'warning',
      };
    case 'unknown':
      return {
        label: 'Cannot be verified',
        detail: `${freshness.reason}. This is not the same as "unchanged".`,
        severity: 'warning',
      };
    default:
      // A freshness kind from a newer backend is not evidence of freshness.
      return {
        label: 'Cannot be verified',
        detail: 'Onera does not recognise this freshness state.',
        severity: 'warning',
      };
  }
}

/** A short, honest label for a build identity. */
export function buildLabel(identity: StoreBuildIdentity | null): string {
  if (identity === null) return 'unknown';
  const parts: string[] = [];
  if (identity.build_id !== null) parts.push(`build ${identity.build_id}`);
  if (identity.branch !== null) parts.push(`branch ${identity.branch}`);
  if (parts.length === 0 && identity.depots.length > 0) {
    parts.push(`${identity.depots.length} depot(s)`);
  }
  return parts.length === 0 ? 'unknown' : parts.join(', ');
}

/** How a baseline's source is described, and what it does not prove. */
export function sourceCopy(source: BaselineSource): StateCopy {
  switch (source) {
    case 'store_verified_capture':
      return {
        label: 'Store-verified capture',
        detail:
          'Captured after you confirmed the store verified its own files. A local observation ' +
          'stamped with the build it saw, not a claim that the store attested every byte.',
        severity: 'neutral',
      };
    case 'local_snapshot':
      return {
        label: 'Local snapshot (not store-verified)',
        detail:
          'Captured from whatever was on disk. It proves the files have not changed since ' +
          'Onera looked, not that they were ever correct.',
        severity: 'info',
      };
    case 'store_manifest':
      return {
        label: 'Store manifest',
        detail: 'Derived from a manifest supplied by the store.',
        severity: 'neutral',
      };
    default:
      return {
        label: 'Unrecognised source',
        detail: 'Onera does not recognise how this baseline was captured.',
        severity: 'warning',
      };
  }
}

/** Copy for one finding classification. */
export interface ClassificationCopy extends StateCopy {
  /** Whether Onera refuses to act on this without an explicit decision. */
  needsDecision: boolean;
}

const CLASSIFICATIONS: Record<FileClassification, ClassificationCopy> = {
  matching: {
    label: 'Matching',
    detail: 'Present with the recorded contents.',
    severity: 'neutral',
    needsDecision: false,
  },
  modified: {
    label: 'Modified',
    detail: 'Present with different contents than the baseline recorded.',
    severity: 'warning',
    needsDecision: false,
  },
  missing: {
    label: 'Missing',
    detail: 'Recorded in the baseline but absent from disk.',
    severity: 'warning',
    needsDecision: false,
  },
  extra_managed: {
    label: 'Deployed by Onera',
    detail: 'Not in the baseline, but Onera deployed it and knows which mod provides it.',
    severity: 'info',
    needsDecision: false,
  },
  extra_unknown: {
    label: 'Unknown extra',
    detail: 'Nobody claims this file. Onera never deletes it for you.',
    severity: 'warning',
    needsDecision: true,
  },
  unreadable: {
    label: 'Unreadable',
    detail: 'Onera could not read this path, so it cannot say what is there.',
    severity: 'danger',
    needsDecision: true,
  },
  special_file: {
    label: 'Link or special file',
    detail:
      'A symlink or other non-regular file. It is reported rather than followed, because its ' +
      'target is outside the scope Onera can reason about.',
    severity: 'danger',
    needsDecision: true,
  },
};

/**
 * Describe a finding classification.
 *
 * An unrecognised value from a newer backend fails safe: it is presented as a
 * difference that needs a decision rather than being quietly ignored.
 *
 * @param classification - The classification as the backend serialised it.
 * @returns Display copy for it.
 */
export function classificationCopy(classification: FileClassification): ClassificationCopy {
  return (
    CLASSIFICATIONS[classification] ?? {
      label: classification,
      detail: 'Onera does not recognise this result and will not act on it automatically.',
      severity: 'danger',
      needsDecision: true,
    }
  );
}

/**
 * Whether a verification may be shown as clean.
 *
 * Mirrors `BaselineVerification::is_clean`: a completed, content-hashed scan
 * over the captured scope with no non-matching findings. A quick scan can prove
 * something changed and can never prove nothing did.
 *
 * @param verification - The verification returned by the backend.
 * @param baseline - The baseline it was compared against.
 * @returns True only when all four conditions hold.
 */
export function isClean(
  verification: BaselineVerification,
  baseline: GameBaseline | null,
): boolean {
  if (baseline === null) return false;
  return (
    verification.state === 'completed' &&
    verification.evidence === 'content_hashed' &&
    verification.scope_fingerprint === baseline.scope_fingerprint &&
    !hasDifferences(verification)
  );
}

/** Whether anything other than matching files was found. */
export function hasDifferences(verification: BaselineVerification): boolean {
  const c = verification.counts;
  return (
    c.modified > 0 ||
    c.missing > 0 ||
    c.extra_managed > 0 ||
    c.extra_unknown > 0 ||
    c.unreadable > 0 ||
    c.special > 0
  );
}

/** The one-line verdict shown above the findings table. */
export function verdict(
  verification: BaselineVerification,
  baseline: GameBaseline | null,
): StateCopy {
  if (isClean(verification, baseline)) {
    return {
      label: 'Clean',
      detail: 'Every file in the baseline scope matches what was captured.',
      severity: 'neutral',
    };
  }
  if (verification.evidence === 'metadata_only') {
    return {
      label: hasDifferences(verification) ? 'Differences found' : 'Not verified',
      detail:
        'This was a quick scan: sizes and modes only. It can show that something changed, ' +
        'never that nothing did. Run a full check to confirm.',
      severity: hasDifferences(verification) ? 'warning' : 'info',
    };
  }
  if (verification.state !== 'completed') {
    return {
      label: 'Incomplete',
      detail: `The scan ${verification.state} before it covered the whole baseline scope.`,
      severity: 'warning',
    };
  }
  if (baseline !== null && verification.scope_fingerprint !== baseline.scope_fingerprint) {
    return {
      label: 'Scope changed',
      detail:
        'This scan covered a different set of paths than the baseline did, so the two cannot ' +
        'be compared. Capture a new baseline.',
      severity: 'warning',
    };
  }
  return {
    label: 'Differences found',
    detail: 'The findings below differ from the captured baseline.',
    severity: 'warning',
  };
}

/** Findings worth showing: everything that is not a plain match. */
export function differences(verification: BaselineVerification) {
  return verification.findings.filter((finding) => finding.classification !== 'matching');
}

/** Format a byte count for display. */
export function bytes(count: number): string {
  const units = ['B', 'kB', 'MB', 'GB', 'TB'];
  let value = count;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}
