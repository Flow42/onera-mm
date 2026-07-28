/**
 * View-model helpers for installation previews and conflicts.
 *
 * All of it is pure: the component renders what these return, and the functions
 * can be tested without a DOM. The classification vocabulary matches the Rust
 * core exactly so the two cannot drift.
 */

import type { Classification, InstallPlanView, PlannedFileView } from './types';

/** How a classification is described to the user, and how urgent it is. */
export interface ClassificationCopy {
  label: string;
  detail: string;
  severity: 'neutral' | 'info' | 'warning' | 'danger';
  needsDecision: boolean;
}

const COPY: Record<Classification, ClassificationCopy> = {
  create: {
    label: 'New file',
    detail: 'Nothing is at this path yet.',
    severity: 'neutral',
    needsDecision: false,
  },
  identical: {
    label: 'Already identical',
    detail: 'The same content is already there. The file is left alone and shared.',
    severity: 'neutral',
    needsDecision: false,
  },
  replace_previous_release: {
    label: 'Updates this mod',
    detail: 'Replaces a file from an earlier release of the same mod.',
    severity: 'info',
    needsDecision: false,
  },
  conflict_with_other_mod: {
    label: 'Conflicts with another mod',
    detail: 'Another installed mod already provides this file.',
    severity: 'warning',
    needsDecision: true,
  },
  unmanaged_existing: {
    label: 'File Onera did not install',
    detail: 'Something is already here that Onera has never managed.',
    severity: 'warning',
    needsDecision: true,
  },
  externally_modified: {
    label: 'Changed since installation',
    detail: 'This file was edited after Onera deployed it. It will never be overwritten silently.',
    severity: 'danger',
    needsDecision: true,
  },
  invalid_target: {
    label: 'Not allowed',
    detail: 'The game adapter refuses this target.',
    severity: 'danger',
    needsDecision: false,
  },
  skipped_by_rule: {
    label: 'Skipped',
    detail: 'A rule you saved earlier skips this file.',
    severity: 'neutral',
    needsDecision: false,
  },
};

/**
 * Describe a classification.
 *
 * @param classification - The classification from the core.
 * @returns Display copy, with a safe fallback for an unknown value.
 */
export function describe(classification: Classification | string): ClassificationCopy {
  return (
    COPY[classification as Classification] ?? {
      label: classification,
      detail: 'Onera does not recognise this classification. Check for an update.',
      severity: 'warning',
      needsDecision: true,
    }
  );
}

/** Choices offered for a conflict, in the order they should be shown. */
export const CONFLICT_CHOICES = [
  { id: 'keep_existing', label: 'Keep the existing file', destructive: false },
  { id: 'replace_after_backup', label: 'Replace it (a backup is kept)', destructive: false },
  { id: 'adopt_existing', label: 'Adopt the existing file', destructive: false },
  { id: 'abort', label: 'Cancel this installation', destructive: true },
] as const;

/** How widely a decision can be applied. */
export const DECISION_SCOPES = [
  { id: 'this_file', label: 'Just this file' },
  { id: 'equivalent_in_operation', label: 'Every similar conflict in this installation' },
  { id: 'remembered_rule', label: 'Remember for this mod and folder' },
] as const;

/** Files still waiting on the user. */
export function unresolved(plan: InstallPlanView): PlannedFileView[] {
  return plan.files.filter(
    (file) => describe(file.classification).needsDecision && file.decision === null,
  );
}

/** Count of each classification, for the preview header. */
export function summarise(plan: InstallPlanView): { classification: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const file of plan.files) {
    counts.set(file.classification, (counts.get(file.classification) ?? 0) + 1);
  }
  return [...counts.entries()]
    .map(([classification, count]) => ({ classification, count }))
    .sort((a, b) => b.count - a.count);
}

/**
 * Whether the Install button should be enabled.
 *
 * Deliberately stricter than `plan.ready`: the frontend refuses to offer the
 * action when anything is unresolved, even if the backend would also refuse.
 * Two checks are better than one for the operation that writes to a game.
 */
export function canApply(plan: InstallPlanView): boolean {
  return plan.ready && unresolved(plan).length === 0;
}

/** Format a byte count for display. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return '—';
  }
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}
