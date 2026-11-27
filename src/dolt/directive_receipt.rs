//! Canonical payloads for a confirmed Directive activation receipt. Historical
//! approval may establish record provenance and never authorizes a new action.

use serde::{Deserialize, Serialize};

pub(super) const KIND: &str = "directive.activate";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DirectiveScope {
    Workspace,
    Area {
        id: String,
        include_descendants: bool,
    },
    Project {
        id: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActivatedDirective {
    pub(super) id: String,
    pub(super) revision: String,
    pub(super) scope: DirectiveScope,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Approval {
    pub(super) version: u8,
    pub(super) workspace_id: String,
    pub(super) request_id: String,
    pub(super) fingerprint: String,
    pub(super) actor_ref: String,
    pub(super) reviewer_ref: String,
    pub(super) approval_ref: String,
    pub(super) decision: Decision,
    pub(super) directive: ActivatedDirective,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Decision {
    Approved,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResultRecord {
    pub(super) version: u8,
    pub(super) directive: ActivatedDirective,
}
