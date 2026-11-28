use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use time::OffsetDateTime;

use super::Reason;
use super::directive_artifact_intake::DirectiveStatus;
use super::prepared_records::{DirectiveFields, Fields};
use super::reviewed_records::{Review, actor_ref, timestamp, validate_historical_source};
use super::scope_plan::portable_reference;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectiveHistory {
    status: DirectiveStatus,
    area_id: String,
    source_ref: String,
    issuer_ref: String,
    created_at: String,
    updated_at: String,
    approval_and_validity: Approval,
    pub(super) supersession: Edges,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Approval {
    Reviewed {
        approval_ref: Option<String>,
        valid_from: Option<String>,
        expires_at: Option<String>,
    },
    Unresolved,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Edges {
    Reviewed {
        replacement: Option<String>,
        replaces: Option<String>,
    },
    Unresolved,
}

impl DirectiveHistory {
    pub(super) fn prepare(
        &self,
        status: DirectiveStatus,
        area_id: &str,
        observed_at: Option<OffsetDateTime>,
    ) -> Result<Fields, Reason> {
        if self.status != status {
            return Err(Reason::InvalidDirectiveStatus);
        }
        if self.area_id != area_id {
            return Err(Reason::AmbiguousScopeOrCollision);
        }
        match &self.supersession {
            Edges::Unresolved => return Err(Reason::InvalidSupersessionChain),
            Edges::Reviewed {
                replacement: Some(_),
                ..
            } if status != DirectiveStatus::Superseded => {
                return Err(Reason::InvalidSupersessionChain);
            }
            Edges::Reviewed { .. } => {}
        }
        validate_historical_source(&self.source_ref, &self.created_at, &self.updated_at)?;
        if !actor_ref(&self.issuer_ref) {
            return Err(Reason::IncompleteHistoricalProvenance);
        }
        let Approval::Reviewed {
            approval_ref,
            valid_from,
            expires_at,
        } = &self.approval_and_validity
        else {
            return Err(Reason::UnverifiedDirectiveValidity);
        };
        if approval_ref
            .as_deref()
            .is_some_and(|value| !actor_ref(value) && !portable_reference(value))
            || (status == DirectiveStatus::Active && approval_ref.is_none())
        {
            return Err(Reason::MissingDirectiveApproval);
        }
        let bound = |value: &String| timestamp(value).ok_or(Reason::UnverifiedDirectiveValidity);
        let start = valid_from.as_ref().map(bound).transpose()?;
        let end = expires_at.as_ref().map(bound).transpose()?;
        if matches!(start.zip(end), Some((start, end)) if end <= start) {
            return Err(Reason::UnverifiedDirectiveValidity);
        }
        if status == DirectiveStatus::Active {
            let observed_at = observed_at.ok_or(Reason::UnverifiedDirectiveValidity)?;
            if end.is_some_and(|end| observed_at >= end) {
                return Err(Reason::IncompleteHistoricalProvenance);
            }
            if start.is_some_and(|start| observed_at < start)
                || timestamp(&self.updated_at).is_some_and(|updated| observed_at < updated)
            {
                return Err(Reason::UnverifiedDirectiveValidity);
            }
        }
        Ok(Fields::Directive(DirectiveFields {
            status,
            area_id: self.area_id.clone(),
            source_ref: self.source_ref.clone(),
            issuer_ref: self.issuer_ref.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            supersedes_id: None,
            approval_ref: approval_ref.clone(),
            valid_from: valid_from.clone(),
            expires_at: expires_at.clone(),
        }))
    }
}

pub(super) struct ReviewedDirective<'a> {
    pub(super) key: &'a str,
    pub(super) row: usize,
    pub(super) review: &'a Review,
    pub(super) prepared: bool,
}

pub(super) enum LinkDecision<'a> {
    ReverseTo(&'a str),
    Hold,
}

#[derive(Default)]
struct Node<'a> {
    rows: Vec<&'a ReviewedDirective<'a>>,
    neighbors: Vec<&'a str>,
    incoming: Vec<&'a str>,
    outgoing: Vec<&'a str>,
    claims_to_replace: Vec<&'a str>,
}

impl Node<'_> {
    fn ready(&self) -> bool {
        matches!(self.rows.as_slice(), [record] if record.prepared &&
            matches!(record.review.directive_edges(), Some(Edges::Reviewed { .. })))
    }

    fn valid(&self) -> bool {
        self.ready()
            && self.incoming.len() <= 1
            && self.outgoing.len() <= 1
            && self
                .claims_to_replace
                .iter()
                .all(|claim| self.incoming.as_slice() == [*claim])
    }
}

pub(super) fn resolve_links<'a>(
    reviews: &'a [ReviewedDirective<'a>],
) -> Vec<(usize, LinkDecision<'a>)> {
    let mut nodes = BTreeMap::<&str, Node<'_>>::new();
    for record in reviews {
        let node = nodes.entry(record.key).or_default();
        node.rows.push(record);
        if let Some(Edges::Reviewed {
            replacement,
            replaces,
        }) = record.review.directive_edges()
        {
            if let Some(next) = replacement.as_deref() {
                let old = nodes.entry(record.key).or_default();
                old.outgoing.push(next);
                old.neighbors.push(next);
                let replacement = nodes.entry(next).or_default();
                replacement.incoming.push(record.key);
                replacement.neighbors.push(record.key);
            }
            if let Some(previous) = replaces.as_deref() {
                let replacement = nodes.entry(record.key).or_default();
                replacement.claims_to_replace.push(previous);
                replacement.neighbors.push(previous);
                nodes
                    .entry(previous)
                    .or_default()
                    .neighbors
                    .push(record.key);
            }
        }
    }

    let mut visited = HashSet::new();
    let mut decisions = Vec::new();
    for (&key, node) in &nodes {
        if node.neighbors.is_empty() || !visited.insert(key) {
            continue;
        }
        let mut pending = vec![key];
        let mut component = Vec::new();
        while let Some(key) = pending.pop() {
            let node = &nodes[key];
            component.push(node);
            pending.extend(
                node.neighbors
                    .iter()
                    .copied()
                    .filter(|key| visited.insert(*key)),
            );
        }
        // With at most one incoming and one outgoing edge per node, a connected
        // component has a cycle precisely when it has as many edges as nodes.
        let edge_count: usize = component.iter().map(|node| node.outgoing.len()).sum();
        if edge_count >= component.len() || component.iter().any(|node| !node.valid()) {
            decisions.extend(
                component
                    .iter()
                    .flat_map(|node| node.rows.iter())
                    .map(|record| (record.row, LinkDecision::Hold)),
            );
            continue;
        }
        for node in component {
            if let [previous] = node.incoming.as_slice() {
                decisions.push((
                    node.rows[0].row,
                    LinkDecision::ReverseTo(&nodes[previous].rows[0].review.target_id),
                ));
            }
        }
    }
    decisions
}
