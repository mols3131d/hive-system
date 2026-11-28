use std::collections::{HashMap, HashSet, VecDeque};

use super::json_object::Object;
use super::task_intake::CandidateStatus;
use super::task_provenance::{Key, Obligations, RelationDeclaration, Relations, TaskReview};

pub(super) struct Node<'a> {
    pub(super) key: Key<'a>,
    pub(super) source_reviews: &'a [&'a TaskReview],
    pub(super) status: Option<CandidateStatus>,
}

impl Node<'_> {
    fn obligations(&self) -> impl Iterator<Item = &Obligations> {
        self.source_reviews
            .iter()
            .filter_map(|review| match &review.relations {
                Some(Relations::Reviewed(Object(obligations))) => Some(obligations),
                _ => None,
            })
    }

    fn declares_no_edges(&self) -> bool {
        self.source_reviews.iter().any(|review| {
            matches!(
                review.relations,
                Some(Relations::Simple(RelationDeclaration::NoEdges))
            )
        })
    }
}

pub(super) fn held_nodes(nodes: &[Node<'_>]) -> Vec<bool> {
    // Every row in a duplicate group is held. References to that key reach a
    // held endpoint, while declarations from every matching row are inspected.
    let by_key: HashMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.key, index))
        .collect();
    let mut held: Vec<_> = nodes.iter().map(|node| node.status.is_none()).collect();
    let mut obligations = vec![0; nodes.len()];
    let mut dependents = vec![Vec::new(); nodes.len()];
    // A child with a held parent cannot retain its parent_id. This reverse
    // constraint propagates holds, but isn't itself a completion edge.
    let mut children = vec![Vec::new(); nodes.len()];

    let declarations = nodes
        .iter()
        .enumerate()
        .flat_map(|(index, node)| node.obligations().map(move |review| (index, node, review)));
    for (index, node, review) in declarations {
        if let Some(parent) = review.parent() {
            match by_key.get(&parent.key()).copied() {
                Some(parent) => {
                    children[parent].push(index);
                    if nodes[parent].status.is_some()
                        && !nodes[parent].obligations().any(|review| {
                            review
                                .children()
                                .iter()
                                .any(|child| child.legacy_key.key() == node.key)
                        })
                    {
                        held[index] = true;
                        held[parent] = true;
                    }
                }
                None => held[index] = true,
            }
        }

        let mut seen = HashSet::new();
        for prerequisite in review.prerequisites() {
            let key = prerequisite.key();
            let Some(target) = by_key.get(&key).copied().filter(|_| seen.insert(key)) else {
                held[index] = true;
                continue;
            };
            obligations[index] += 1;
            dependents[target].push(index);
            if node.status == Some(CandidateStatus::Done)
                && nodes[target].status != Some(CandidateStatus::Done)
            {
                held[index] = true;
            }
            if nodes[target].declares_no_edges() {
                held[target] = true;
            }
        }

        seen.clear();
        for child in review.children() {
            let key = child.legacy_key.key();
            let Some(target) = by_key.get(&key).copied().filter(|_| seen.insert(key)) else {
                held[index] = true;
                continue;
            };
            obligations[index] += 1;
            dependents[target].push(index);
            if nodes[target].status.is_some()
                && !nodes[target]
                    .obligations()
                    .filter_map(Obligations::parent)
                    .any(|parent| parent.key() == node.key)
            {
                held[index] = true;
                held[target] = true;
            }
            let done = node.status == Some(CandidateStatus::Done);
            let child_status = nodes[target].status;
            let needs_acceptance = done && child_status == Some(CandidateStatus::Cancelled);
            if (done && !child_status.is_some_and(CandidateStatus::is_terminal))
                || needs_acceptance != child.scope_acceptance.is_some()
            {
                held[index] = true;
            }
        }
    }

    // Peel obligations from the combined graph. Anything left has a cycle or
    // depends on one. The outcome does not depend on the order of reviews.
    let mut ready: VecDeque<_> = obligations
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect();
    while let Some(index) = ready.pop_front() {
        for &dependent in &dependents[index] {
            obligations[dependent] -= 1;
            if obligations[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }
    for (index, count) in obligations.into_iter().enumerate() {
        held[index] |= count != 0;
    }

    // Keep required endpoints together transitively, including the reverse
    // parent_id constraint. Each newly held node is visited once.
    let mut pending: VecDeque<_> = held
        .iter()
        .enumerate()
        .filter_map(|(index, held)| held.then_some(index))
        .collect();
    while let Some(index) = pending.pop_front() {
        for &dependent in dependents[index].iter().chain(&children[index]) {
            if !held[dependent] {
                held[dependent] = true;
                pending.push_back(dependent);
            }
        }
    }
    held
}
