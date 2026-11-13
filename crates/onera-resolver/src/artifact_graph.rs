//! Pure dependency graph solver for downloaded/imported project artifacts.
//!
//! Computes topological deployment order, detects missing required
//! dependencies, flags conflicting artifacts, and reports dependency cycles.

use onera_core::ids::ArtifactId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Relationship kind between two artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactEdgeKind {
    /// Hard requirement: `from` cannot be deployed without `to`.
    Required,
    /// Soft ordering hint: if both are selected, `to` should deploy before `from`.
    Recommended,
    /// Mutual exclusion: `from` and `to` cannot both be active.
    Conflicts,
}

/// Directed dependency edge between two artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactEdge {
    /// Artifact declaring the relationship.
    pub from: ArtifactId,
    /// Target artifact referenced.
    pub to: ArtifactId,
    /// Kind of dependency or conflict.
    pub kind: ArtifactEdgeKind,
}

/// Result of resolving an artifact dependency graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactGraphResolution {
    /// Selected artifacts in topological deployment order (dependencies first).
    pub ordered_install: Vec<ArtifactId>,
    /// `(dependent, missing_dependency)` pairs for required edges whose target is not selected/present.
    pub missing_required: Vec<(ArtifactId, ArtifactId)>,
    /// Pairs of selected artifacts that conflict with each other.
    pub conflicts: Vec<(ArtifactId, ArtifactId)>,
    /// Detected dependency cycles among selected artifacts.
    pub cycles: Vec<Vec<ArtifactId>>,
}

impl ArtifactGraphResolution {
    /// Whether the dependency graph is valid and ready for deployment.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.missing_required.is_empty() && self.conflicts.is_empty() && self.cycles.is_empty()
    }
}

/// Resolve a set of selected artifacts and their dependency/conflict edges.
#[must_use]
pub fn resolve_artifact_graph(
    selected: &BTreeSet<ArtifactId>,
    edges: &[ArtifactEdge],
) -> ArtifactGraphResolution {
    let mut missing_required = Vec::new();
    let mut conflicts = Vec::new();
    let mut adj: BTreeMap<ArtifactId, BTreeSet<ArtifactId>> = BTreeMap::new();
    let mut in_degree: BTreeMap<ArtifactId, usize> = BTreeMap::new();

    for &id in selected {
        adj.entry(id).or_default();
        in_degree.entry(id).or_insert(0);
    }

    for edge in edges {
        if !selected.contains(&edge.from) {
            continue;
        }
        match edge.kind {
            ArtifactEdgeKind::Conflicts => {
                if selected.contains(&edge.to) {
                    let pair = if edge.from <= edge.to {
                        (edge.from, edge.to)
                    } else {
                        (edge.to, edge.from)
                    };
                    if !conflicts.contains(&pair) {
                        conflicts.push(pair);
                    }
                }
            }
            ArtifactEdgeKind::Required => {
                if !selected.contains(&edge.to) {
                    missing_required.push((edge.from, edge.to));
                } else if edge.from != edge.to && adj.entry(edge.to).or_default().insert(edge.from) {
                    *in_degree.entry(edge.from).or_insert(0) += 1;
                }
            }
            ArtifactEdgeKind::Recommended => {
                if selected.contains(&edge.to)
                    && edge.from != edge.to
                    && adj.entry(edge.to).or_default().insert(edge.from)
                {
                    *in_degree.entry(edge.from).or_insert(0) += 1;
                }
            }
        }
    }

    // Kahn's topological sort with deterministic BTreeSet queue
    let mut queue: BTreeSet<ArtifactId> = in_degree
        .iter()
        .filter_map(|(&id, &deg)| if deg == 0 { Some(id) } else { None })
        .collect();

    let mut ordered_install = Vec::with_capacity(selected.len());
    while let Some(node) = queue.pop_first() {
        ordered_install.push(node);
        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                if let Some(deg) = in_degree.get_mut(&next) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        queue.insert(next);
                    }
                }
            }
        }
    }

    // Any remaining nodes with in_degree > 0 are part of or blocked by a cycle.
    let mut cycles = Vec::new();
    if ordered_install.len() < selected.len() {
        let remaining: Vec<ArtifactId> = in_degree
            .iter()
            .filter_map(|(&id, &deg)| if deg > 0 { Some(id) } else { None })
            .collect();
        if !remaining.is_empty() {
            cycles.push(remaining);
        }
    }

    ArtifactGraphResolution {
        ordered_install,
        missing_required,
        conflicts,
        cycles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_valid_dag_in_dependency_first_order() {
        let base = ArtifactId::new();
        let mid = ArtifactId::new();
        let top = ArtifactId::new();
        let selected: BTreeSet<ArtifactId> = [base, mid, top].into_iter().collect();

        let edges = vec![
            ArtifactEdge {
                from: mid,
                to: base,
                kind: ArtifactEdgeKind::Required,
            },
            ArtifactEdge {
                from: top,
                to: mid,
                kind: ArtifactEdgeKind::Required,
            },
        ];

        let res = resolve_artifact_graph(&selected, &edges);
        assert!(res.is_valid());
        assert_eq!(res.ordered_install, vec![base, mid, top]);
    }

    #[test]
    fn detects_missing_required_conflicts_and_cycles() {
        let a = ArtifactId::new();
        let b = ArtifactId::new();
        let missing = ArtifactId::new();
        let selected: BTreeSet<ArtifactId> = [a, b].into_iter().collect();

        let edges = vec![
            ArtifactEdge {
                from: a,
                to: b,
                kind: ArtifactEdgeKind::Required,
            },
            ArtifactEdge {
                from: b,
                to: a,
                kind: ArtifactEdgeKind::Required,
            },
            ArtifactEdge {
                from: a,
                to: missing,
                kind: ArtifactEdgeKind::Required,
            },
            ArtifactEdge {
                from: a,
                to: b,
                kind: ArtifactEdgeKind::Conflicts,
            },
        ];

        let res = resolve_artifact_graph(&selected, &edges);
        assert!(!res.is_valid());
        assert_eq!(res.missing_required, vec![(a, missing)]);
        assert_eq!(res.conflicts.len(), 1);
        assert!(!res.cycles.is_empty());
    }
}
