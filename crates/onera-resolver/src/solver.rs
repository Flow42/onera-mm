use super::{AvailableVersion, MemberRequest, ResolutionRequest};
use onera_core::domain::dependency::{
    DependencyAvailability, DependencyCandidate, DependencyFingerprint, DependencyGroup,
    DependencyHealth, DependencySnapshot, DlcOwnership, DlcRequirement, MemberHealth,
    RequirementKind, ResolutionOutcome, ResolutionResult, SelectedVersion, UnsatisfiedRequirement,
};
use onera_core::domain::profile::DesiredModState;
use onera_core::ids::{
    DependencyGroupId, ProfileMemberId, ProviderFileGroupId, ProviderFileId, ProviderId,
    ProviderModId, ProviderVersionId, StoreDlcId,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

const MAX_ENABLED_MEMBERS: usize = 64;
const MAX_AVAILABLE_MODS: usize = 256;
const MAX_VERSIONS_PER_MOD: usize = 32;
const MAX_SEARCH_STATES: usize = 250_000;
const MAX_DISABLE_MEMBERS: usize = 20;

type ModKey = (ProviderId, ProviderModId);
type FileKey = (ProviderId, ProviderModId, ProviderFileId);
type GroupKey = (ProviderId, ProviderFileGroupId);
type SourceKey = (
    ProviderId,
    String,
    ProviderModId,
    Option<ProviderFileId>,
    Option<ProviderVersionId>,
);
type CandidateKey = (
    ProviderId,
    String,
    ProviderModId,
    Option<ProviderFileId>,
    Option<ProviderVersionId>,
    Option<ProviderFileGroupId>,
);

#[derive(Clone)]
struct Chosen {
    version: AvailableVersion,
    member_id: Option<ProfileMemberId>,
}

type Assignment = BTreeMap<ModKey, Chosen>;

struct Normalized {
    game_slug: String,
    members: Vec<MemberRequest>,
    available: BTreeMap<ModKey, Vec<AvailableVersion>>,
    snapshots: Vec<DependencySnapshot>,
    ownership: BTreeMap<StoreDlcId, DlcOwnership>,
    overrides: Vec<onera_core::domain::dependency::DependencyOverride>,
}

struct SearchResult {
    best: Option<Solution>,
    failures: BTreeMap<FailureKey, UnsatisfiedRequirement>,
    unknown: BTreeSet<String>,
    exceeded_bound: bool,
    states: usize,
}

#[derive(Clone)]
struct Solution {
    assignment: Assignment,
    changed: usize,
    downloads: usize,
}

type FailureKey = (SourceKey, DependencyGroupId);

enum Evaluation {
    Resolved,
    Need {
        failure: UnsatisfiedRequirement,
        options: Vec<AvailableVersion>,
    },
    Dead(UnsatisfiedRequirement),
    Unknown(String),
}

pub(super) fn solve(request: &ResolutionRequest) -> ResolutionResult {
    let evidence = request.evidence();
    let normalized = match Normalized::new(request) {
        Ok(normalized) => normalized,
        Err(reason) => {
            return ResolutionResult {
                outcome: ResolutionOutcome::Unknown { reason },
                health: Vec::new(),
                evidence,
            };
        }
    };

    let health = normalized.current_health();
    let enabled: Vec<_> = normalized
        .members
        .iter()
        .filter(|member| member.desired == DesiredModState::Enabled)
        .cloned()
        .collect();
    let full = normalized.search(&enabled);

    let outcome = if full.exceeded_bound {
        ResolutionOutcome::Unknown {
            reason: format!(
                "dependency search exceeded its deterministic limit of {MAX_SEARCH_STATES} states"
            ),
        }
    } else if let Some(solution) = full.best {
        normalized.classify(&enabled, solution)
    } else if let Some(reason) = full.unknown.iter().next() {
        ResolutionOutcome::Unknown {
            reason: reason.clone(),
        }
    } else {
        normalized
            .minimal_disable_set(&enabled)
            .unwrap_or_else(|| ResolutionOutcome::Unsatisfied {
                requirements: full.failures.into_values().collect(),
            })
    };

    ResolutionResult {
        outcome,
        health,
        evidence,
    }
}

impl Normalized {
    fn new(request: &ResolutionRequest) -> Result<Self, String> {
        let mut members_by_id = BTreeMap::new();
        for member in &request.members {
            match members_by_id.get(&member.profile_member_id) {
                Some(previous) if previous != member => {
                    return Err(format!(
                        "profile member {} has conflicting definitions",
                        member.profile_member_id
                    ));
                }
                Some(_) => {}
                None => {
                    members_by_id.insert(member.profile_member_id, member.clone());
                }
            }
        }
        let members: Vec<MemberRequest> = members_by_id.into_values().collect();
        let enabled_count = members
            .iter()
            .filter(|member| member.desired == DesiredModState::Enabled)
            .count();
        if enabled_count > MAX_ENABLED_MEMBERS {
            return Err(format!(
                "profile has {enabled_count} enabled members; the deterministic limit is {MAX_ENABLED_MEMBERS}"
            ));
        }
        let mut enabled_mods = BTreeMap::new();
        for member in members
            .iter()
            .filter(|member| member.desired == DesiredModState::Enabled)
        {
            let key = member_mod_key(member);
            if let Some(previous) = enabled_mods.insert(key, member.profile_member_id) {
                return Err(format!(
                    "enabled members {previous} and {} identify the same provider mod",
                    member.profile_member_id
                ));
            }
        }

        let mut files: BTreeMap<FileKey, AvailableVersion> = BTreeMap::new();
        for version in &request.available {
            let key = file_key(version);
            if let Some(previous) = files.get_mut(&key) {
                if !same_version_metadata(previous, version) {
                    return Err(format!(
                        "provider file {}/{}/{} has conflicting candidate metadata",
                        key.0, key.1, key.2
                    ));
                }
                previous.installed |= version.installed;
            } else {
                files.insert(key, version.clone());
            }
        }
        let mut available: BTreeMap<ModKey, Vec<AvailableVersion>> = BTreeMap::new();
        for version in files.into_values() {
            available
                .entry(version_mod_key(&version))
                .or_default()
                .push(version);
        }
        if available.len() > MAX_AVAILABLE_MODS {
            return Err(format!(
                "request has {} candidate mods; the deterministic limit is {MAX_AVAILABLE_MODS}",
                available.len()
            ));
        }
        for (key, versions) in &mut available {
            if versions.len() > MAX_VERSIONS_PER_MOD {
                return Err(format!(
                    "provider mod {}/{} has {} candidate versions; the deterministic limit is {MAX_VERSIONS_PER_MOD}",
                    key.0,
                    key.1,
                    versions.len()
                ));
            }
            versions.sort_by(stable_version_cmp);
        }

        let mut snapshots_by_source: BTreeMap<SourceKey, DependencySnapshot> = BTreeMap::new();
        for original in &request.snapshots {
            let mut snapshot = original.clone();
            normalize_snapshot(&mut snapshot)?;
            if DependencyFingerprint::of(&snapshot.groups, &snapshot.dlc) != snapshot.fingerprint {
                return Err(format!(
                    "dependency snapshot {} has a fingerprint that does not match its definition",
                    snapshot.id
                ));
            }
            let key = source_key(&snapshot);
            if let Some(previous) = snapshots_by_source.get(&key) {
                if previous.fingerprint != snapshot.fingerprint
                    || previous.availability != snapshot.availability
                    || previous.groups != snapshot.groups
                    || previous.dlc != snapshot.dlc
                {
                    return Err(format!(
                        "dependency source {}/{}/{} has conflicting snapshots",
                        key.0, key.1, key.2
                    ));
                }
            } else {
                snapshots_by_source.insert(key, snapshot);
            }
        }

        let mut ownership = BTreeMap::new();
        for fact in &request.dlc_ownership {
            if let Some(previous) = ownership.insert(fact.id.clone(), fact.ownership) {
                if previous != fact.ownership {
                    return Err(format!("DLC {} has conflicting ownership facts", fact.id));
                }
            }
        }

        let mut overrides = request.overrides.clone();
        overrides.sort_by(|a, b| {
            (
                a.profile_member_id,
                a.group_id,
                &a.fingerprint,
                a.created_at,
            )
                .cmp(&(
                    b.profile_member_id,
                    b.group_id,
                    &b.fingerprint,
                    b.created_at,
                ))
        });
        overrides.dedup();

        Ok(Self {
            game_slug: request.game_slug.clone(),
            members,
            available,
            snapshots: snapshots_by_source.into_values().collect(),
            ownership,
            overrides,
        })
    }

    fn search(&self, members: &[MemberRequest]) -> SearchResult {
        self.search_with_limit(members, MAX_SEARCH_STATES)
    }

    fn search_with_limit(&self, members: &[MemberRequest], state_limit: usize) -> SearchResult {
        let mut engine = SearchEngine {
            input: self,
            roots: members,
            result: SearchResult {
                best: None,
                failures: BTreeMap::new(),
                unknown: BTreeSet::new(),
                exceeded_bound: false,
                states: 0,
            },
            visited: BTreeSet::new(),
            states: 0,
            state_limit,
        };
        engine.select_roots(0, &mut BTreeMap::new());
        engine.result.states = engine.states;
        engine.result
    }

    fn classify(&self, members: &[MemberRequest], solution: Solution) -> ResolutionOutcome {
        let member_ids: BTreeSet<_> = members
            .iter()
            .map(|member| member.profile_member_id)
            .collect();
        let mut selected: Vec<_> = solution.assignment.values().map(selected_version).collect();
        selected.sort_by(selected_version_cmp);
        let install: Vec<_> = selected
            .iter()
            .filter(|version| version.profile_member_id.is_none())
            .cloned()
            .collect();
        let select: Vec<_> = selected
            .into_iter()
            .filter(|version| {
                version
                    .profile_member_id
                    .is_some_and(|member_id| member_ids.contains(&member_id))
                    && members.iter().any(|member| {
                        member.profile_member_id == version.profile_member_id.unwrap()
                            && !selection_matches_selected(member, version)
                    })
            })
            .collect();

        if select.is_empty() && install.is_empty() {
            ResolutionOutcome::Compatible
        } else if select.is_empty() {
            ResolutionOutcome::InstallMissing { install }
        } else {
            ResolutionOutcome::UpdateSet { select, install }
        }
    }

    fn minimal_disable_set(&self, enabled: &[MemberRequest]) -> Option<ResolutionOutcome> {
        if enabled.is_empty() {
            return None;
        }
        if enabled.len() > MAX_DISABLE_MEMBERS {
            return Some(ResolutionOutcome::Unknown {
                reason: format!(
                    "minimal disable search supports at most {MAX_DISABLE_MEMBERS} enabled members"
                ),
            });
        }
        let mut state_budget = MAX_SEARCH_STATES;
        for count in 1..=enabled.len() {
            let mut indices = Vec::with_capacity(count);
            match self.find_disable_combination(enabled, count, 0, &mut indices, &mut state_budget)
            {
                Ok(Some(disable)) => {
                    return Some(ResolutionOutcome::DisableSet { disable });
                }
                Ok(None) => {}
                Err(()) => {
                    return Some(ResolutionOutcome::Unknown {
                        reason: format!(
                            "minimal disable search exceeded its deterministic limit of {MAX_SEARCH_STATES} states"
                        ),
                    });
                }
            }
        }
        None
    }

    fn find_disable_combination(
        &self,
        enabled: &[MemberRequest],
        target: usize,
        next: usize,
        indices: &mut Vec<usize>,
        state_budget: &mut usize,
    ) -> Result<Option<Vec<ProfileMemberId>>, ()> {
        if indices.len() == target {
            let disabled: BTreeSet<_> = indices.iter().copied().collect();
            let active: Vec<_> = enabled
                .iter()
                .enumerate()
                .filter(|(index, _)| !disabled.contains(index))
                .map(|(_, member)| member.clone())
                .collect();
            if *state_budget == 0 {
                return Err(());
            }
            let result = self.search_with_limit(&active, *state_budget);
            *state_budget = state_budget.saturating_sub(result.states);
            if result.exceeded_bound {
                return Err(());
            }
            let valid = result.best.is_some_and(|solution| {
                solution.changed == 0
                    && solution.assignment.len() == active.len()
                    && solution
                        .assignment
                        .values()
                        .all(|chosen| chosen.member_id.is_some())
            });
            return Ok(valid.then(|| {
                indices
                    .iter()
                    .map(|index| enabled[*index].profile_member_id)
                    .collect()
            }));
        }
        let remaining = target - indices.len();
        for index in next..=enabled.len() - remaining {
            indices.push(index);
            if let Some(found) =
                self.find_disable_combination(enabled, target, index + 1, indices, state_budget)?
            {
                return Ok(Some(found));
            }
            indices.pop();
        }
        Ok(None)
    }

    fn current_health(&self) -> Vec<MemberHealth> {
        let mut current = Assignment::new();
        for member in self
            .members
            .iter()
            .filter(|member| member.desired == DesiredModState::Enabled)
        {
            if let Some(version) = self.current_version(member) {
                current.insert(
                    member_mod_key(member),
                    Chosen {
                        version,
                        member_id: Some(member.profile_member_id),
                    },
                );
            }
        }

        self.members
            .iter()
            .map(|member| {
                if member.desired == DesiredModState::Disabled {
                    return MemberHealth {
                        profile_member_id: member.profile_member_id,
                        health: DependencyHealth::NotApplicable,
                        unsatisfied: Vec::new(),
                    };
                }
                let Some(chosen) = current.get(&member_mod_key(member)) else {
                    return MemberHealth {
                        profile_member_id: member.profile_member_id,
                        health: DependencyHealth::Unsatisfied,
                        unsatisfied: Vec::new(),
                    };
                };
                self.health_for(member, chosen, &current)
            })
            .collect()
    }

    fn health_for(
        &self,
        member: &MemberRequest,
        chosen: &Chosen,
        assignment: &Assignment,
    ) -> MemberHealth {
        let snapshots = self.matching_snapshots(&chosen.version);
        if snapshots.is_empty() {
            return MemberHealth {
                profile_member_id: member.profile_member_id,
                health: DependencyHealth::Unknown,
                unsatisfied: Vec::new(),
            };
        }
        let mut issues = Vec::new();
        let mut hard_unsatisfied = false;
        let mut ignored = false;
        let mut unknown = false;
        let mut any_authoritative = false;
        let mut all_unsupported = true;

        for snapshot in snapshots {
            match &snapshot.availability {
                DependencyAvailability::Unavailable { reason } => {
                    unknown = true;
                    all_unsupported = false;
                    let _ = reason;
                }
                DependencyAvailability::Unsupported => {}
                DependencyAvailability::Fetched | DependencyAvailability::Cached { .. } => {
                    any_authoritative = true;
                    all_unsupported = false;
                    for group in &snapshot.groups {
                        let violated = match group.kind {
                            RequirementKind::Required | RequirementKind::Recommended => {
                                !group_satisfied(group, assignment, &self.game_slug)
                            }
                            RequirementKind::Incompatible => {
                                group_has_named_selection(group, assignment, &self.game_slug)
                            }
                        };
                        if !violated {
                            continue;
                        }
                        let issue = self.group_failure(snapshot, group, assignment);
                        if self.is_ignored(member.profile_member_id, snapshot, group.id) {
                            ignored = true;
                        } else if group.kind.is_blocking() {
                            hard_unsatisfied = true;
                        }
                        issues.push(issue);
                    }
                    for dlc in &snapshot.dlc {
                        if self.is_ignored(member.profile_member_id, snapshot, dlc.id) {
                            if self.evaluate_dlc(dlc) != DlcOwnership::Owned {
                                ignored = true;
                                issues.push(dlc_failure(
                                    snapshot,
                                    dlc,
                                    "DLC requirement is ignored at the user's risk",
                                ));
                            }
                            continue;
                        }
                        match self.evaluate_dlc(dlc) {
                            DlcOwnership::Owned => {}
                            DlcOwnership::Unknown => {
                                unknown = true;
                                issues.push(dlc_failure(snapshot, dlc, "DLC ownership is unknown"));
                            }
                            DlcOwnership::NotOwned => {
                                hard_unsatisfied = true;
                                issues.push(dlc_failure(
                                    snapshot,
                                    dlc,
                                    "required DLC is not owned",
                                ));
                            }
                        }
                    }
                }
            }
        }
        issues.sort_by(unsatisfied_cmp);
        issues.dedup();
        let health = if unknown {
            DependencyHealth::Unknown
        } else if hard_unsatisfied {
            DependencyHealth::Unsatisfied
        } else if ignored {
            DependencyHealth::Ignored
        } else if all_unsupported && !any_authoritative {
            DependencyHealth::NotApplicable
        } else {
            DependencyHealth::Satisfied
        };
        MemberHealth {
            profile_member_id: member.profile_member_id,
            health,
            unsatisfied: issues,
        }
    }

    fn current_version(&self, member: &MemberRequest) -> Option<AvailableVersion> {
        self.available
            .get(&member_mod_key(member))?
            .iter()
            .find(|version| {
                version.status.is_selectable()
                    && version.game_slug == self.game_slug
                    && selection_matches_version(member, version)
            })
            .cloned()
    }

    fn root_options(&self, member: &MemberRequest) -> Vec<AvailableVersion> {
        let mut options: Vec<_> = self
            .available
            .get(&member_mod_key(member))
            .into_iter()
            .flatten()
            .filter(|version| {
                version.status.is_selectable()
                    && version.game_slug == self.game_slug
                    && (!member.pin.is_pinned() || selection_matches_version(member, version))
            })
            .cloned()
            .collect();
        options.sort_by(|a, b| {
            let a_current = selection_matches_version(member, a);
            let b_current = selection_matches_version(member, b);
            b_current
                .cmp(&a_current)
                .then_with(|| preferred_version_cmp(a, b))
        });
        options
    }

    fn matching_snapshots(&self, version: &AvailableVersion) -> Vec<&DependencySnapshot> {
        self.snapshots
            .iter()
            .filter(|snapshot| source_matches_version(snapshot, version))
            .collect()
    }

    fn evaluate(&self, assignment: &Assignment) -> Evaluation {
        let mut file_groups = BTreeSet::new();
        for chosen in assignment.values() {
            if let Some(group) = &chosen.version.provider_file_group_id {
                if !file_groups.insert((chosen.version.provider.clone(), group.clone())) {
                    return Evaluation::Unknown(format!(
                        "more than one selected version belongs to provider file group {group}"
                    ));
                }
            }
            let snapshots = self.matching_snapshots(&chosen.version);
            if snapshots.is_empty() {
                return Evaluation::Unknown(format!(
                    "dependency data is missing for provider file {}",
                    chosen.version.provider_file_id
                ));
            }
            for snapshot in snapshots {
                match &snapshot.availability {
                    DependencyAvailability::Unsupported => continue,
                    DependencyAvailability::Unavailable { reason } => {
                        return Evaluation::Unknown(format!(
                            "dependency data is unavailable for {}/{}: {reason}",
                            snapshot.source.provider, snapshot.source.provider_mod_id
                        ));
                    }
                    DependencyAvailability::Fetched | DependencyAvailability::Cached { .. } => {}
                }
                for group in &snapshot.groups {
                    let ignored = chosen
                        .member_id
                        .is_some_and(|member_id| self.is_ignored(member_id, snapshot, group.id));
                    if ignored || group.kind == RequirementKind::Recommended {
                        continue;
                    }
                    let matched = match group.kind {
                        RequirementKind::Required | RequirementKind::Recommended => {
                            group_satisfied(group, assignment, &self.game_slug)
                        }
                        RequirementKind::Incompatible => {
                            group_has_named_selection(group, assignment, &self.game_slug)
                        }
                    };
                    match group.kind {
                        RequirementKind::Required if !matched => {
                            let failure = self.group_failure(snapshot, group, assignment);
                            let options = self.group_options(group);
                            return if options.is_empty() {
                                Evaluation::Dead(failure)
                            } else {
                                Evaluation::Need { failure, options }
                            };
                        }
                        RequirementKind::Incompatible if matched => {
                            return Evaluation::Dead(
                                self.group_failure(snapshot, group, assignment),
                            );
                        }
                        _ => {}
                    }
                }
                for dlc in &snapshot.dlc {
                    let ignored = chosen
                        .member_id
                        .is_some_and(|member_id| self.is_ignored(member_id, snapshot, dlc.id));
                    if ignored {
                        continue;
                    }
                    match self.evaluate_dlc(dlc) {
                        DlcOwnership::Owned => {}
                        DlcOwnership::NotOwned => {
                            return Evaluation::Dead(dlc_failure(
                                snapshot,
                                dlc,
                                "required DLC is not owned",
                            ));
                        }
                        DlcOwnership::Unknown => {
                            return Evaluation::Unknown(format!(
                                "DLC ownership is unknown for requirement {}",
                                dlc.id
                            ));
                        }
                    }
                }
            }
        }
        Evaluation::Resolved
    }

    fn group_options(&self, group: &DependencyGroup) -> Vec<AvailableVersion> {
        let mut options = BTreeMap::new();
        for candidate in &group.candidates {
            if !candidate.is_selectable_for(&self.game_slug) {
                continue;
            }
            let key = (
                candidate.provider.clone(),
                candidate.provider_mod_id.clone(),
            );
            for version in self
                .available
                .get(&key)
                .into_iter()
                .flatten()
                .filter(|version| {
                    version.status.is_selectable()
                        && version.game_slug == self.game_slug
                        && candidate_matches_version(candidate, version)
                })
            {
                options.insert(file_key(version), version.clone());
            }
        }
        let mut options: Vec<_> = options.into_values().collect();
        options.sort_by(preferred_version_cmp);
        options
    }

    fn group_failure(
        &self,
        snapshot: &DependencySnapshot,
        group: &DependencyGroup,
        assignment: &Assignment,
    ) -> UnsatisfiedRequirement {
        let explanation = match group.kind {
            RequirementKind::Incompatible => {
                let pinned = assignment.values().any(|chosen| {
                    group.candidates.iter().any(|candidate| {
                        candidate.game_slug == self.game_slug
                            && candidate_matches_version(candidate, &chosen.version)
                    }) && chosen.member_id.is_some_and(|id| {
                        self.members
                            .iter()
                            .find(|member| member.profile_member_id == id)
                            .is_some_and(|member| member.pin.is_pinned())
                    })
                });
                if pinned {
                    "a pinned selected candidate is declared incompatible".to_owned()
                } else {
                    "a selected candidate is declared incompatible".to_owned()
                }
            }
            RequirementKind::Required | RequirementKind::Recommended => {
                if group.candidates.is_empty() {
                    "requirement has no candidates".to_owned()
                } else if !group
                    .candidates
                    .iter()
                    .any(|candidate| candidate.is_selectable_for(&self.game_slug))
                {
                    "no candidate targets this game with selectable status".to_owned()
                } else if self.group_options(group).is_empty() {
                    "no available candidate targets this game".to_owned()
                } else {
                    "no satisfying candidate is selected".to_owned()
                }
            }
        };
        UnsatisfiedRequirement {
            source: snapshot.source.clone(),
            group_id: group.id,
            label: group.label.clone(),
            explanation,
        }
    }

    fn evaluate_dlc(&self, requirement: &DlcRequirement) -> DlcOwnership {
        requirement.evaluate(&|id| {
            self.ownership
                .get(id)
                .copied()
                .unwrap_or(DlcOwnership::Unknown)
        })
    }

    fn is_ignored(
        &self,
        member_id: ProfileMemberId,
        snapshot: &DependencySnapshot,
        group_id: DependencyGroupId,
    ) -> bool {
        self.overrides
            .iter()
            .any(|override_| override_.applies_to(member_id, &snapshot.fingerprint, group_id))
    }
}

struct SearchEngine<'a> {
    input: &'a Normalized,
    roots: &'a [MemberRequest],
    result: SearchResult,
    visited: BTreeSet<Vec<FileKey>>,
    states: usize,
    state_limit: usize,
}

impl SearchEngine<'_> {
    fn select_roots(&mut self, index: usize, assignment: &mut Assignment) {
        if self.result.exceeded_bound {
            return;
        }
        if index == self.roots.len() {
            self.explore(assignment);
            return;
        }
        let member = &self.roots[index];
        let options = self.input.root_options(member);
        if options.is_empty() {
            return;
        }
        for version in options {
            if has_file_group_conflict(assignment, &version) {
                continue;
            }
            let key = version_mod_key(&version);
            assignment.insert(
                key.clone(),
                Chosen {
                    version,
                    member_id: Some(member.profile_member_id),
                },
            );
            self.select_roots(index + 1, assignment);
            assignment.remove(&key);
        }
    }

    fn explore(&mut self, assignment: &mut Assignment) {
        self.states += 1;
        if self.states > self.state_limit {
            self.result.exceeded_bound = true;
            return;
        }
        let state: Vec<_> = assignment
            .values()
            .map(|chosen| file_key(&chosen.version))
            .collect();
        if !self.visited.insert(state) {
            return;
        }
        match self.input.evaluate(assignment) {
            Evaluation::Resolved => self.record_solution(assignment),
            Evaluation::Unknown(reason) => {
                self.result.unknown.insert(reason);
            }
            Evaluation::Dead(failure) => self.record_failure(failure),
            Evaluation::Need { failure, options } => {
                let mut branched = false;
                for version in options {
                    let key = version_mod_key(&version);
                    if assignment.contains_key(&key)
                        || has_file_group_conflict(assignment, &version)
                    {
                        continue;
                    }
                    branched = true;
                    assignment.insert(
                        key.clone(),
                        Chosen {
                            version,
                            member_id: None,
                        },
                    );
                    self.explore(assignment);
                    assignment.remove(&key);
                }
                if !branched {
                    self.record_failure(failure);
                }
            }
        }
    }

    fn record_failure(&mut self, failure: UnsatisfiedRequirement) {
        let key = (source_key_from_source(&failure.source), failure.group_id);
        self.result.failures.entry(key).or_insert(failure);
    }

    fn record_solution(&mut self, assignment: &Assignment) {
        let changed = self
            .roots
            .iter()
            .filter(|member| {
                assignment
                    .get(&member_mod_key(member))
                    .is_some_and(|chosen| !selection_matches_version(member, &chosen.version))
            })
            .count();
        let downloads = assignment
            .values()
            .filter(|chosen| !chosen.version.installed)
            .count();
        let solution = Solution {
            assignment: assignment.clone(),
            changed,
            downloads,
        };
        if self
            .result
            .best
            .as_ref()
            .is_none_or(|best| solution_cmp(&solution, best) == Ordering::Less)
        {
            self.result.best = Some(solution);
        }
    }
}

fn normalize_snapshot(snapshot: &mut DependencySnapshot) -> Result<(), String> {
    for group in &mut snapshot.groups {
        let mut candidates: BTreeMap<CandidateKey, DependencyCandidate> = BTreeMap::new();
        for candidate in &group.candidates {
            // Cosmetic provider text is never part of solver identity or order.
            let mut candidate = candidate.clone();
            candidate.display_name = None;
            let key = candidate_key(&candidate);
            if let Some(previous) = candidates.get(&key) {
                if previous.position != candidate.position || previous.status != candidate.status {
                    return Err(format!(
                        "dependency group {} has conflicting metadata for one candidate",
                        group.id
                    ));
                }
            } else {
                candidates.insert(key, candidate);
            }
        }
        group.candidates = candidates.into_values().collect();
        group.candidates.sort_by(candidate_cmp);
    }
    snapshot.groups.sort_by_key(|group| group.id);
    for pair in snapshot.groups.windows(2) {
        if pair[0].id == pair[1].id && pair[0] != pair[1] {
            return Err(format!(
                "dependency group {} has conflicting definitions",
                pair[0].id
            ));
        }
    }
    snapshot.groups.dedup();
    for requirement in &mut snapshot.dlc {
        requirement.alternatives.sort();
        requirement.alternatives.dedup();
    }
    snapshot.dlc.sort_by_key(|requirement| requirement.id);
    for pair in snapshot.dlc.windows(2) {
        if pair[0].id == pair[1].id && pair[0] != pair[1] {
            return Err(format!(
                "DLC requirement {} has conflicting definitions",
                pair[0].id
            ));
        }
    }
    snapshot.dlc.dedup();
    Ok(())
}

fn member_mod_key(member: &MemberRequest) -> ModKey {
    (
        member.selection.provider.clone(),
        member.selection.provider_mod_id.clone(),
    )
}

fn version_mod_key(version: &AvailableVersion) -> ModKey {
    (version.provider.clone(), version.provider_mod_id.clone())
}

fn file_key(version: &AvailableVersion) -> FileKey {
    (
        version.provider.clone(),
        version.provider_mod_id.clone(),
        version.provider_file_id.clone(),
    )
}

fn source_key(snapshot: &DependencySnapshot) -> SourceKey {
    source_key_from_source(&snapshot.source)
}

fn candidate_key(candidate: &DependencyCandidate) -> CandidateKey {
    (
        candidate.provider.clone(),
        candidate.game_slug.clone(),
        candidate.provider_mod_id.clone(),
        candidate.provider_file_id.clone(),
        candidate.provider_version_id.clone(),
        candidate.provider_file_group_id.clone(),
    )
}

fn source_key_from_source(source: &onera_core::domain::dependency::DependencySource) -> SourceKey {
    (
        source.provider.clone(),
        source.game_slug.clone(),
        source.provider_mod_id.clone(),
        source.provider_file_id.clone(),
        source.provider_version_id.clone(),
    )
}

fn same_version_metadata(a: &AvailableVersion, b: &AvailableVersion) -> bool {
    a.provider == b.provider
        && a.game_slug == b.game_slug
        && a.provider_mod_id == b.provider_mod_id
        && a.provider_file_id == b.provider_file_id
        && a.provider_version_id == b.provider_version_id
        && a.provider_file_group_id == b.provider_file_group_id
        && a.position == b.position
        && a.status == b.status
}

fn selection_matches_version(member: &MemberRequest, version: &AvailableVersion) -> bool {
    member.selection.provider == version.provider
        && member.selection.provider_mod_id == version.provider_mod_id
        && member
            .selection
            .provider_file_id
            .as_ref()
            .is_some_and(|id| id == &version.provider_file_id)
        && member
            .selection
            .provider_version_id
            .as_ref()
            .is_none_or(|id| Some(id) == version.provider_version_id.as_ref())
        && member
            .selection
            .provider_file_group_id
            .as_ref()
            .is_none_or(|id| Some(id) == version.provider_file_group_id.as_ref())
}

fn selection_matches_selected(member: &MemberRequest, version: &SelectedVersion) -> bool {
    member.selection.provider == version.provider
        && member.selection.provider_mod_id == version.provider_mod_id
        && member.selection.provider_file_id.as_ref() == Some(&version.provider_file_id)
        && member
            .selection
            .provider_version_id
            .as_ref()
            .is_none_or(|id| Some(id) == version.provider_version_id.as_ref())
        && member
            .selection
            .provider_file_group_id
            .as_ref()
            .is_none_or(|id| Some(id) == version.provider_file_group_id.as_ref())
}

fn candidate_matches_version(candidate: &DependencyCandidate, version: &AvailableVersion) -> bool {
    candidate.provider == version.provider
        && candidate.provider_mod_id == version.provider_mod_id
        && candidate
            .provider_file_id
            .as_ref()
            .is_none_or(|id| id == &version.provider_file_id)
        && candidate
            .provider_version_id
            .as_ref()
            .is_none_or(|id| Some(id) == version.provider_version_id.as_ref())
        && candidate
            .provider_file_group_id
            .as_ref()
            .is_none_or(|id| Some(id) == version.provider_file_group_id.as_ref())
}

fn source_matches_version(snapshot: &DependencySnapshot, version: &AvailableVersion) -> bool {
    snapshot.source.provider == version.provider
        && snapshot.source.game_slug == version.game_slug
        && snapshot.source.provider_mod_id == version.provider_mod_id
        && snapshot
            .source
            .provider_file_id
            .as_ref()
            .is_none_or(|id| id == &version.provider_file_id)
        && snapshot
            .source
            .provider_version_id
            .as_ref()
            .is_none_or(|id| Some(id) == version.provider_version_id.as_ref())
}

fn group_satisfied(group: &DependencyGroup, assignment: &Assignment, game_slug: &str) -> bool {
    group.candidates.iter().any(|candidate| {
        candidate.is_selectable_for(game_slug)
            && assignment
                .get(&(
                    candidate.provider.clone(),
                    candidate.provider_mod_id.clone(),
                ))
                .is_some_and(|chosen| candidate_matches_version(candidate, &chosen.version))
    })
}

fn group_has_named_selection(
    group: &DependencyGroup,
    assignment: &Assignment,
    game_slug: &str,
) -> bool {
    group.candidates.iter().any(|candidate| {
        candidate.game_slug == game_slug
            && assignment
                .get(&(
                    candidate.provider.clone(),
                    candidate.provider_mod_id.clone(),
                ))
                .is_some_and(|chosen| candidate_matches_version(candidate, &chosen.version))
    })
}

fn has_file_group_conflict(assignment: &Assignment, version: &AvailableVersion) -> bool {
    let Some(group) = &version.provider_file_group_id else {
        return false;
    };
    let key: GroupKey = (version.provider.clone(), group.clone());
    assignment.values().any(|chosen| {
        chosen
            .version
            .provider_file_group_id
            .as_ref()
            .is_some_and(|selected_group| {
                (chosen.version.provider.clone(), selected_group.clone()) == key
            })
    })
}

fn selected_version(chosen: &Chosen) -> SelectedVersion {
    SelectedVersion {
        provider: chosen.version.provider.clone(),
        provider_mod_id: chosen.version.provider_mod_id.clone(),
        provider_file_id: chosen.version.provider_file_id.clone(),
        provider_version_id: chosen.version.provider_version_id.clone(),
        provider_file_group_id: chosen.version.provider_file_group_id.clone(),
        profile_member_id: chosen.member_id,
    }
}

fn solution_cmp(a: &Solution, b: &Solution) -> Ordering {
    a.changed
        .cmp(&b.changed)
        .then_with(|| a.downloads.cmp(&b.downloads))
        .then_with(|| assignment_cmp(&a.assignment, &b.assignment))
}

fn assignment_cmp(a: &Assignment, b: &Assignment) -> Ordering {
    let mut a_iter = a.iter();
    let mut b_iter = b.iter();
    loop {
        match (a_iter.next(), b_iter.next()) {
            (Some((a_key, a_value)), Some((b_key, b_value))) => {
                let ordering = a_key
                    .cmp(b_key)
                    .then_with(|| preferred_version_cmp(&a_value.version, &b_value.version));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

fn preferred_version_cmp(a: &AvailableVersion, b: &AvailableVersion) -> Ordering {
    if a.provider_file_group_id.is_some()
        && a.provider_file_group_id == b.provider_file_group_id
        && a.provider == b.provider
    {
        if let (Some(a_position), Some(b_position)) = (a.position, b.position) {
            let position = b_position.cmp(&a_position);
            if position != Ordering::Equal {
                return position;
            }
        }
    }
    stable_version_cmp(a, b)
}

fn stable_version_cmp(a: &AvailableVersion, b: &AvailableVersion) -> Ordering {
    (
        &a.provider,
        &a.provider_mod_id,
        &a.provider_file_group_id,
        &a.provider_file_id,
        &a.provider_version_id,
    )
        .cmp(&(
            &b.provider,
            &b.provider_mod_id,
            &b.provider_file_group_id,
            &b.provider_file_id,
            &b.provider_version_id,
        ))
}

fn candidate_cmp(a: &DependencyCandidate, b: &DependencyCandidate) -> Ordering {
    (
        &a.provider,
        &a.game_slug,
        &a.provider_mod_id,
        &a.provider_file_group_id,
        &a.provider_file_id,
        &a.provider_version_id,
        a.position,
        a.status,
    )
        .cmp(&(
            &b.provider,
            &b.game_slug,
            &b.provider_mod_id,
            &b.provider_file_group_id,
            &b.provider_file_id,
            &b.provider_version_id,
            b.position,
            b.status,
        ))
}

fn selected_version_cmp(a: &SelectedVersion, b: &SelectedVersion) -> Ordering {
    (
        &a.provider,
        &a.provider_mod_id,
        &a.provider_file_group_id,
        &a.provider_file_id,
        &a.provider_version_id,
        a.profile_member_id,
    )
        .cmp(&(
            &b.provider,
            &b.provider_mod_id,
            &b.provider_file_group_id,
            &b.provider_file_id,
            &b.provider_version_id,
            b.profile_member_id,
        ))
}

fn unsatisfied_cmp(a: &UnsatisfiedRequirement, b: &UnsatisfiedRequirement) -> Ordering {
    source_key_from_source(&a.source)
        .cmp(&source_key_from_source(&b.source))
        .then_with(|| a.group_id.cmp(&b.group_id))
}

fn dlc_failure(
    snapshot: &DependencySnapshot,
    requirement: &DlcRequirement,
    explanation: &str,
) -> UnsatisfiedRequirement {
    UnsatisfiedRequirement {
        source: snapshot.source.clone(),
        group_id: requirement.id,
        label: requirement.label.clone(),
        explanation: explanation.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DlcOwnershipFact, MemberRequest, ResolutionRequest};
    use chrono::{DateTime, Utc};
    use onera_core::domain::dependency::{
        CandidateStatus, DependencyGroup, DependencyOverride, DependencySource,
    };
    use onera_core::domain::profile::{MemberPin, MemberSelection};
    use onera_core::ids::{DependencyGroupId, ModId, ProfileMemberId};
    use proptest::prelude::*;
    use std::str::FromStr;

    const GAME: &str = "cyberpunk2077";

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn uuid_text(value: u128) -> String {
        format!("{value:032x}")
    }

    fn member_id(value: u128) -> ProfileMemberId {
        ProfileMemberId::from_str(&uuid_text(value)).unwrap()
    }

    fn mod_id(value: u128) -> ModId {
        ModId::from_str(&uuid_text(value)).unwrap()
    }

    fn group_id(value: u128) -> DependencyGroupId {
        DependencyGroupId::from_str(&uuid_text(value)).unwrap()
    }

    fn version(mod_name: &str, file: &str, position: i64, installed: bool) -> AvailableVersion {
        AvailableVersion {
            provider: ProviderId::nexus(),
            game_slug: GAME.into(),
            provider_mod_id: ProviderModId::new(mod_name),
            provider_file_id: ProviderFileId::new(file),
            provider_version_id: Some(ProviderVersionId::new(format!("v-{file}"))),
            provider_file_group_id: Some(ProviderFileGroupId::new(format!("g-{mod_name}"))),
            position: Some(position),
            status: CandidateStatus::Available,
            installed,
        }
    }

    fn candidate(version: &AvailableVersion) -> DependencyCandidate {
        DependencyCandidate {
            provider: version.provider.clone(),
            game_slug: version.game_slug.clone(),
            provider_mod_id: version.provider_mod_id.clone(),
            provider_file_id: Some(version.provider_file_id.clone()),
            provider_version_id: version.provider_version_id.clone(),
            provider_file_group_id: version.provider_file_group_id.clone(),
            position: version.position,
            status: version.status,
            display_name: Some(format!("display-{}", version.provider_file_id)),
        }
    }

    fn group(
        value: u128,
        kind: RequirementKind,
        candidates: Vec<DependencyCandidate>,
    ) -> DependencyGroup {
        DependencyGroup {
            id: group_id(value),
            provider_group_key: Some(format!("group-{value}")),
            label: Some(format!("requirement-{value}")),
            kind,
            candidates,
        }
    }

    fn member(value: u128, version: &AvailableVersion, pinned: bool) -> MemberRequest {
        MemberRequest {
            profile_member_id: member_id(value),
            mod_id: mod_id(value),
            selection: MemberSelection {
                provider: version.provider.clone(),
                provider_mod_id: version.provider_mod_id.clone(),
                provider_file_id: Some(version.provider_file_id.clone()),
                provider_version_id: version.provider_version_id.clone(),
                provider_file_group_id: version.provider_file_group_id.clone(),
            },
            pin: if pinned {
                MemberPin::Pinned {
                    pinned_at: now(),
                    reason: Some("test pin".into()),
                }
            } else {
                MemberPin::Unpinned
            },
            desired: DesiredModState::Enabled,
        }
    }

    fn snapshot(
        version: &AvailableVersion,
        groups: Vec<DependencyGroup>,
        dlc: Vec<DlcRequirement>,
    ) -> DependencySnapshot {
        DependencySnapshot::fetched(
            DependencySource {
                provider: version.provider.clone(),
                game_slug: version.game_slug.clone(),
                provider_mod_id: version.provider_mod_id.clone(),
                provider_file_id: Some(version.provider_file_id.clone()),
                provider_version_id: version.provider_version_id.clone(),
            },
            groups,
            dlc,
            now(),
        )
    }

    fn request(
        members: Vec<MemberRequest>,
        available: Vec<AvailableVersion>,
        mut snapshots: Vec<DependencySnapshot>,
    ) -> ResolutionRequest {
        for version in &available {
            if !snapshots.iter().any(|existing| {
                existing.source.provider == version.provider
                    && existing.source.provider_mod_id == version.provider_mod_id
                    && existing.source.provider_file_id.as_ref() == Some(&version.provider_file_id)
            }) {
                snapshots.push(snapshot(version, vec![], vec![]));
            }
        }
        ResolutionRequest {
            game_slug: GAME.into(),
            members,
            snapshots,
            available,
            dlc_ownership: vec![],
            overrides: vec![],
        }
    }

    fn installed_files(outcome: &ResolutionOutcome) -> Vec<&str> {
        match outcome {
            ResolutionOutcome::InstallMissing { install }
            | ResolutionOutcome::UpdateSet { install, .. } => install
                .iter()
                .map(|selected| selected.provider_file_id.as_str())
                .collect(),
            _ => vec![],
        }
    }

    #[test]
    fn satisfied_and_missing_single_dependencies() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "b1", 1, false);
        let a_snapshot = snapshot(
            &a,
            vec![group(10, RequirementKind::Required, vec![candidate(&b)])],
            vec![],
        );

        let satisfied = solve(&request(
            vec![member(1, &a, false), member(2, &b, false)],
            vec![a.clone(), b.clone()],
            vec![a_snapshot.clone()],
        ));
        assert_eq!(satisfied.outcome, ResolutionOutcome::Compatible);

        let missing = solve(&request(
            vec![member(1, &a, false)],
            vec![a.clone(), b.clone()],
            vec![a_snapshot],
        ));
        assert_eq!(installed_files(&missing.outcome), vec!["b1"]);
        assert!(matches!(
            missing.outcome,
            ResolutionOutcome::InstallMissing { .. }
        ));
    }

    #[test]
    fn independent_and_groups_choose_one_or_alternative_each() {
        let a = version("a", "a1", 1, true);
        let b1 = version("b", "b1", 1, false);
        let b2 = version("b", "b2", 2, true);
        let c = version("c", "c1", 1, false);
        let definition = snapshot(
            &a,
            vec![
                group(
                    10,
                    RequirementKind::Required,
                    vec![candidate(&b1), candidate(&b2)],
                ),
                group(11, RequirementKind::Required, vec![candidate(&c)]),
            ],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a, false)],
            vec![a, b1, b2, c],
            vec![definition],
        ));
        assert_eq!(installed_files(&result.outcome), vec!["b2", "c1"]);
    }

    #[test]
    fn recommended_is_advisory_and_incompatible_is_hard() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "b1", 1, true);
        let recommended = snapshot(
            &a,
            vec![group(10, RequirementKind::Recommended, vec![candidate(&b)])],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a, false)],
            vec![a.clone(), b.clone()],
            vec![recommended],
        ));
        assert_eq!(result.outcome, ResolutionOutcome::Compatible);
        assert_eq!(result.health[0].health, DependencyHealth::Satisfied);
        assert_eq!(result.health[0].unsatisfied.len(), 1);

        let incompatible = snapshot(
            &a,
            vec![group(
                11,
                RequirementKind::Incompatible,
                vec![candidate(&b)],
            )],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a, false), member(2, &b, false)],
            vec![a, b],
            vec![incompatible],
        ));
        assert!(matches!(
            result.outcome,
            ResolutionOutcome::DisableSet { .. }
        ));
        assert_eq!(result.health[0].health, DependencyHealth::Unsatisfied);
    }

    #[test]
    fn valid_cycles_and_self_dependencies_terminate() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "b1", 1, false);
        let snapshots = vec![
            snapshot(
                &a,
                vec![group(10, RequirementKind::Required, vec![candidate(&b)])],
                vec![],
            ),
            snapshot(
                &b,
                vec![
                    group(11, RequirementKind::Required, vec![candidate(&a)]),
                    group(12, RequirementKind::Required, vec![candidate(&b)]),
                ],
                vec![],
            ),
        ];
        let result = solve(&request(vec![member(1, &a, false)], vec![a, b], snapshots));
        assert!(matches!(
            result.outcome,
            ResolutionOutcome::InstallMissing { .. }
        ));
    }

    #[test]
    fn invalid_cycle_and_empty_group_produce_explained_fallbacks() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "b1", 1, false);
        let invalid_cycle = vec![
            snapshot(
                &a,
                vec![group(10, RequirementKind::Required, vec![candidate(&b)])],
                vec![],
            ),
            snapshot(
                &b,
                vec![group(
                    11,
                    RequirementKind::Incompatible,
                    vec![candidate(&a)],
                )],
                vec![],
            ),
        ];
        let result = solve(&request(
            vec![member(1, &a, false)],
            vec![a.clone(), b],
            invalid_cycle,
        ));
        assert!(matches!(
            result.outcome,
            ResolutionOutcome::DisableSet { .. }
        ));

        let empty = snapshot(
            &a,
            vec![group(12, RequirementKind::Required, vec![])],
            vec![],
        );
        let result = solve(&request(vec![member(1, &a, false)], vec![a], vec![empty]));
        assert!(matches!(
            result.outcome,
            ResolutionOutcome::DisableSet { .. }
        ));
        assert!(result.health[0].unsatisfied[0]
            .explanation
            .contains("no candidates"));
    }

    #[test]
    fn duplicate_candidates_are_set_like() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "b1", 1, false);
        let once = snapshot(
            &a,
            vec![group(10, RequirementKind::Required, vec![candidate(&b)])],
            vec![],
        );
        let twice = snapshot(
            &a,
            vec![group(
                10,
                RequirementKind::Required,
                vec![candidate(&b), candidate(&b)],
            )],
            vec![],
        );
        let one = solve(&request(
            vec![member(1, &a, false)],
            vec![a.clone(), b.clone()],
            vec![once],
        ));
        let two = solve(&request(
            vec![member(1, &a, false)],
            vec![a, b],
            vec![twice],
        ));
        assert_eq!(one, two);
    }

    #[test]
    fn wrong_game_and_non_selectable_candidates_are_filtered() {
        let a = version("a", "a1", 1, true);
        let mut wrong_game = version("b", "b1", 1, false);
        wrong_game.game_slug = "skyrim".into();
        let mut hidden = version("c", "c1", 1, false);
        hidden.status = CandidateStatus::Hidden;
        let definition = snapshot(
            &a,
            vec![group(
                10,
                RequirementKind::Required,
                vec![candidate(&wrong_game), candidate(&hidden)],
            )],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a, false)],
            vec![a, wrong_game, hidden],
            vec![definition],
        ));
        assert!(matches!(
            result.outcome,
            ResolutionOutcome::DisableSet { .. }
        ));
        assert!(result.health[0].unsatisfied[0]
            .explanation
            .contains("selectable status"));
    }

    #[test]
    fn pins_are_retained_and_incompatible_pins_are_explained() {
        let a1 = version("a", "a1", 1, true);
        let a2 = version("a", "a2", 2, true);
        let b = version("b", "b1", 1, true);
        let incompatible = snapshot(
            &b,
            vec![group(
                10,
                RequirementKind::Incompatible,
                vec![candidate(&a1)],
            )],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a1, true), member(2, &b, false)],
            vec![a1, a2, b],
            vec![incompatible],
        ));
        assert!(matches!(
            result.outcome,
            ResolutionOutcome::DisableSet { .. }
        ));
        assert!(result.health[1].unsatisfied[0]
            .explanation
            .contains("pinned"));
    }

    #[test]
    fn objective_retains_current_then_minimizes_downloads() {
        let a1 = version("a", "a1", 1, true);
        let a2 = version("a", "a2", 2, true);
        let b1 = version("b", "b1", 1, false);
        let b2 = version("b", "b2", 2, true);
        let a1_snapshot = snapshot(
            &a1,
            vec![group(
                10,
                RequirementKind::Required,
                vec![candidate(&b1), candidate(&b2)],
            )],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a1, false)],
            vec![a1, a2, b1, b2],
            vec![a1_snapshot],
        ));
        assert_eq!(installed_files(&result.outcome), vec!["b2"]);
    }

    #[test]
    fn higher_position_only_breaks_ties_inside_one_file_group() {
        let a = version("a", "a1", 1, true);
        let b1 = version("b", "z-file", 1, true);
        let b2 = version("b", "a-file", 2, true);
        let definition = snapshot(
            &a,
            vec![group(
                10,
                RequirementKind::Required,
                vec![candidate(&b1), candidate(&b2)],
            )],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a, false)],
            vec![a.clone(), b1, b2],
            vec![definition],
        ));
        assert_eq!(installed_files(&result.outcome), vec!["a-file"]);

        let mut c = version("c", "c-file", 999, true);
        c.provider_file_group_id = Some(ProviderFileGroupId::new("unrelated"));
        let definition = snapshot(
            &a,
            vec![group(
                11,
                RequirementKind::Required,
                vec![candidate(&c), candidate(&version("b", "b-file", 1, true))],
            )],
            vec![],
        );
        let b = version("b", "b-file", 1, true);
        let result = solve(&request(
            vec![member(1, &a, false)],
            vec![a, b, c],
            vec![definition],
        ));
        assert_eq!(installed_files(&result.outcome), vec!["b-file"]);
    }

    #[test]
    fn stable_ids_are_the_final_tie_breaker() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "z", 1, true);
        let c = version("c", "a", 1, true);
        let definition = snapshot(
            &a,
            vec![group(
                10,
                RequirementKind::Required,
                vec![candidate(&c), candidate(&b)],
            )],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a, false)],
            vec![c, a, b],
            vec![definition],
        ));
        assert_eq!(installed_files(&result.outcome), vec!["z"]);
    }

    #[test]
    fn compatible_upgrade_can_require_a_dependency_downgrade() {
        let a1 = version("a", "a1", 1, true);
        let a2 = version("a", "a2", 2, false);
        let b1 = version("b", "b1", 1, true);
        let b2 = version("b", "b2", 2, true);
        let impossible = snapshot(
            &a1,
            vec![group(10, RequirementKind::Required, vec![])],
            vec![],
        );
        let upgrade = snapshot(
            &a2,
            vec![group(11, RequirementKind::Required, vec![candidate(&b1)])],
            vec![],
        );
        let result = solve(&request(
            vec![member(1, &a1, false), member(2, &b2, false)],
            vec![a1, a2, b1, b2],
            vec![impossible, upgrade],
        ));
        let ResolutionOutcome::UpdateSet { select, install } = result.outcome else {
            panic!("expected update set");
        };
        assert!(install.is_empty());
        assert_eq!(
            select
                .iter()
                .map(|selected| selected.provider_file_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a2", "b1"]
        );
    }

    #[test]
    fn disable_sets_are_cardinality_minimal_and_deterministic() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "b1", 1, true);
        let c = version("c", "c1", 1, true);
        let snapshots = vec![
            snapshot(
                &a,
                vec![group(
                    10,
                    RequirementKind::Incompatible,
                    vec![candidate(&b)],
                )],
                vec![],
            ),
            snapshot(
                &c,
                vec![group(
                    11,
                    RequirementKind::Incompatible,
                    vec![candidate(&b)],
                )],
                vec![],
            ),
        ];
        let result = solve(&request(
            vec![
                member(1, &a, true),
                member(2, &b, true),
                member(3, &c, true),
            ],
            vec![a, b, c],
            snapshots,
        ));
        assert_eq!(
            result.outcome,
            ResolutionOutcome::DisableSet {
                disable: vec![member_id(2)]
            }
        );
    }

    #[test]
    fn dlc_owned_missing_unknown_and_alternatives_are_distinct() {
        let a = version("a", "a1", 1, true);
        let requirement = DlcRequirement {
            id: group_id(10),
            label: Some("Phantom Liberty".into()),
            alternatives: vec![StoreDlcId::new("dlc-a"), StoreDlcId::new("dlc-b")],
        };
        let definition = snapshot(&a, vec![], vec![requirement]);
        let base = request(vec![member(1, &a, false)], vec![a], vec![definition]);

        let mut owned = base.clone();
        owned.dlc_ownership = vec![
            DlcOwnershipFact {
                id: StoreDlcId::new("dlc-a"),
                ownership: DlcOwnership::NotOwned,
            },
            DlcOwnershipFact {
                id: StoreDlcId::new("dlc-b"),
                ownership: DlcOwnership::Owned,
            },
        ];
        assert_eq!(solve(&owned).outcome, ResolutionOutcome::Compatible);

        let mut missing = owned.clone();
        missing.dlc_ownership[1].ownership = DlcOwnership::NotOwned;
        assert!(matches!(
            solve(&missing).outcome,
            ResolutionOutcome::DisableSet { .. }
        ));

        let mut unknown = owned;
        unknown.dlc_ownership[1].ownership = DlcOwnership::Unknown;
        assert!(matches!(
            solve(&unknown).outcome,
            ResolutionOutcome::Unknown { .. }
        ));
    }

    #[test]
    fn unavailable_unsupported_fresh_and_stale_evidence_are_preserved() {
        let a = version("a", "a1", 1, true);
        let b = version("b", "b1", 1, true);
        let c = version("c", "c1", 1, true);
        let d = version("d", "d1", 1, true);
        let fresh = snapshot(&a, vec![], vec![]);
        let mut cached = snapshot(&b, vec![], vec![]);
        cached.availability = DependencyAvailability::Cached {
            fetched_at: now(),
            stale: false,
        };
        let mut stale = snapshot(&c, vec![], vec![]);
        stale.availability = DependencyAvailability::Cached {
            fetched_at: now(),
            stale: true,
        };
        let unsupported = DependencySnapshot::unsupported(
            DependencySource {
                provider: d.provider.clone(),
                game_slug: d.game_slug.clone(),
                provider_mod_id: d.provider_mod_id.clone(),
                provider_file_id: Some(d.provider_file_id.clone()),
                provider_version_id: d.provider_version_id.clone(),
            },
            now(),
        );
        let result = solve(&request(
            vec![
                member(1, &a, false),
                member(2, &b, false),
                member(3, &c, false),
                member(4, &d, false),
            ],
            vec![a, b, c, d],
            vec![fresh, cached, stale, unsupported],
        ));
        assert_eq!(result.outcome, ResolutionOutcome::Compatible);
        assert_eq!(result.evidence.fresh, 1);
        assert_eq!(result.evidence.cached, 1);
        assert_eq!(result.evidence.stale, 1);
        assert_eq!(result.evidence.unsupported, 1);

        let mut unavailable_request = request(vec![], vec![], vec![]);
        let e = version("e", "e1", 1, true);
        unavailable_request.members = vec![member(5, &e, false)];
        unavailable_request.available = vec![e.clone()];
        unavailable_request.snapshots = vec![DependencySnapshot::unavailable(
            DependencySource {
                provider: e.provider,
                game_slug: e.game_slug,
                provider_mod_id: e.provider_mod_id,
                provider_file_id: Some(e.provider_file_id),
                provider_version_id: e.provider_version_id,
            },
            "offline",
            now(),
        )];
        let result = solve(&unavailable_request);
        assert!(matches!(result.outcome, ResolutionOutcome::Unknown { .. }));
        assert_eq!(result.evidence.unavailable, 1);
    }

    #[test]
    fn overrides_are_member_group_and_fingerprint_scoped() {
        let a = version("a", "a1", 1, true);
        let definition = snapshot(
            &a,
            vec![group(10, RequirementKind::Required, vec![])],
            vec![],
        );
        let mut ignored = request(
            vec![member(1, &a, false)],
            vec![a.clone()],
            vec![definition.clone()],
        );
        ignored.overrides = vec![DependencyOverride {
            profile_member_id: member_id(1),
            fingerprint: definition.fingerprint.clone(),
            group_id: group_id(10),
            reason: "accepted".into(),
            created_at: now(),
        }];
        let result = solve(&ignored);
        assert_eq!(result.outcome, ResolutionOutcome::Compatible);
        assert_eq!(result.health[0].health, DependencyHealth::Ignored);

        ignored.overrides[0].profile_member_id = member_id(2);
        assert!(matches!(
            solve(&ignored).outcome,
            ResolutionOutcome::DisableSet { .. }
        ));

        ignored.overrides[0].profile_member_id = member_id(1);
        ignored.overrides[0].fingerprint = DependencyFingerprint::of(&[], &[]);
        assert!(matches!(
            solve(&ignored).outcome,
            ResolutionOutcome::DisableSet { .. }
        ));
    }

    proptest! {
        #[test]
        fn result_is_invariant_under_permutation_and_duplication(reverse in any::<bool>(), duplicate in any::<bool>()) {
            let a = version("a", "a1", 1, true);
            let b = version("b", "b1", 1, false);
            let definition = snapshot(
                &a,
                vec![group(10, RequirementKind::Required, vec![candidate(&b)])],
                vec![],
            );
            let canonical = request(
                vec![member(1, &a, false)],
                vec![a.clone(), b.clone()],
                vec![definition.clone()],
            );
            let mut permuted = canonical.clone();
            if duplicate {
                permuted.members.push(permuted.members[0].clone());
                permuted.available.extend(permuted.available.clone());
                permuted.snapshots.extend(permuted.snapshots.clone());
            }
            if reverse {
                permuted.members.reverse();
                permuted.available.reverse();
                permuted.snapshots.reverse();
            }
            prop_assert_eq!(solve(&canonical), solve(&permuted));
        }

        #[test]
        fn bounded_cycles_always_terminate(size in 1usize..12) {
            let versions: Vec<_> = (0..size)
                .map(|index| version(&format!("m-{index}"), &format!("f-{index}"), 1, true))
                .collect();
            let snapshots = (0..size)
                .map(|index| snapshot(
                    &versions[index],
                    vec![group(
                        100 + index as u128,
                        RequirementKind::Required,
                        vec![candidate(&versions[(index + 1) % size])],
                    )],
                    vec![],
                ))
                .collect();
            let result = solve(&request(
                vec![member(1, &versions[0], false)],
                versions,
                snapshots,
            ));
            let solved = matches!(
                result.outcome,
                ResolutionOutcome::Compatible | ResolutionOutcome::InstallMissing { .. }
            );
            prop_assert!(solved);
        }

        #[test]
        fn pins_never_change(prefer_newer in any::<bool>()) {
            let a1 = version("a", "a1", if prefer_newer { 2 } else { 1 }, true);
            let a2 = version("a", "a2", if prefer_newer { 1 } else { 2 }, true);
            let result = solve(&request(
                vec![member(1, &a1, true)],
                vec![a2, a1],
                vec![],
            ));
            prop_assert_eq!(result.outcome, ResolutionOutcome::Compatible);
        }

        #[test]
        fn successful_simple_assignments_satisfy_the_required_group(installed in any::<bool>()) {
            let a = version("a", "a1", 1, true);
            let b = version("b", "b1", 1, installed);
            let definition = snapshot(
                &a,
                vec![group(10, RequirementKind::Required, vec![candidate(&b)])],
                vec![],
            );
            let result = solve(&request(
                vec![member(1, &a, false)],
                vec![a, b],
                vec![definition],
            ));
            let files = installed_files(&result.outcome);
            prop_assert_eq!(files, vec!["b1"]);
        }

        #[test]
        fn returned_disable_set_has_no_smaller_valid_subset(extra in 0usize..5) {
            let a = version("a", "a1", 1, true);
            let b = version("b", "b1", 1, true);
            let mut members = vec![member(1, &a, true), member(2, &b, true)];
            let mut versions = vec![a.clone(), b.clone()];
            for index in 0..extra {
                let value = version(&format!("x-{index}"), &format!("x-{index}-1"), 1, true);
                members.push(member(10 + index as u128, &value, true));
                versions.push(value);
            }
            let definition = snapshot(
                &a,
                vec![group(10, RequirementKind::Incompatible, vec![candidate(&b)])],
                vec![],
            );
            let result = solve(&request(members, versions, vec![definition]));
            let ResolutionOutcome::DisableSet { disable } = result.outcome else {
                prop_assert!(false, "expected disable set");
                return Ok(());
            };
            prop_assert_eq!(disable.len(), 1);
        }
    }
}
