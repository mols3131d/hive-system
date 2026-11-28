use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, MapAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Inventory, Scope, ScopeOutcome, invalid, is_portable_scope, is_uuid_v7};
use crate::error::HiveError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Bindings {
    version: u32,
    bindings: Vec<BindingObject>,
}

pub(super) fn deserialize_section<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Bindings>, D::Error> {
    deserialize_object(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(transparent)]
struct BindingObject(#[serde(deserialize_with = "deserialize_object")] Binding);

// Serde's derived struct visitors also accept positional sequences.
// Require a map while retaining the derived field and duplicate checks.
fn deserialize_object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    struct ObjectVisitor<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a JSON object")
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    scope: String,
    area_id: String,
    #[serde(deserialize_with = "deserialize_object")]
    resource: Resource,
    evidence_path: String,
    evidence_blob_oid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resource {
    id: String,
    kind: ResourceKind,
    name: String,
}

#[derive(Clone, Copy)]
pub(crate) enum ResourceKind {
    GitRepo,
}

impl ResourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::GitRepo => "git-repo",
        }
    }
}

impl<'de> Deserialize<'de> for ResourceKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kind = String::deserialize(deserializer)?;
        match kind.as_str() {
            "git-repo" => Ok(Self::GitRepo),
            _ => Err(de::Error::unknown_variant(&kind, &["git-repo"])),
        }
    }
}

// A staging transaction supplies the row revision. Keep the reviewed fields
// and verified URI together, with a separate typed row for the Area link.
pub(crate) struct ResourceRow<'a> {
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) kind: ResourceKind,
    pub(crate) uri: &'a str,
}

pub(crate) struct AreaResourceRow<'a> {
    pub(crate) area_id: &'a str,
    pub(crate) resource_id: &'a str,
}

pub(crate) struct PreparedBinding<'a> {
    pub(crate) resource: ResourceRow<'a>,
    pub(crate) link: AreaResourceRow<'a>,
}

pub(crate) struct PreparedResources<'a> {
    version: u32,
    outcomes: Vec<(&'a str, Result<PreparedBinding<'a>, Hold>)>,
}

impl PreparedResources<'_> {
    pub(crate) fn outcomes(
        &self,
    ) -> impl Iterator<Item = (&str, Result<&PreparedBinding<'_>, Hold>)> {
        self.outcomes
            .iter()
            .map(|(id, outcome)| (*id, outcome.as_ref().map_err(|reason| *reason)))
    }

    pub(super) fn report(&self) -> Value {
        let outcomes: Vec<_> = self
            .outcomes
            .iter()
            .map(|(resource_id, outcome)| match outcome {
                Ok(prepared) => prepared.report(),
                Err(reason) => json!({
                    "status": "held",
                    "resource_id": resource_id,
                    "reason": reason,
                }),
            })
            .collect();
        json!({"version": self.version, "outcomes": outcomes})
    }
}

impl PreparedBinding<'_> {
    fn report(&self) -> Value {
        json!({
            "status": "prepared",
            "area_id": self.link.area_id,
            "resource_id": self.link.resource_id,
            "uri_sha256": format!("{:x}", Sha256::digest(self.resource.uri)),
        })
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Hold {
    ScopeHeld,
    ScopeExcluded,
    AreaMismatch,
    UnverifiedResourceEvidence,
    ResourceCollision,
}

impl Bindings {
    pub(super) fn prepare<'a>(
        &'a self,
        inventory: &Inventory,
        scopes: &[Scope<'a>],
    ) -> Result<PreparedResources<'a>, HiveError> {
        if self.version != 1 {
            return Err(invalid(
                "The scope Resource bindings version is unsupported.",
            ));
        }
        let scopes: HashMap<_, _> = scopes.iter().map(|scope| (scope.scope, scope)).collect();
        let mut ids = HashMap::new();
        let mut names = HashMap::new();
        for BindingObject(binding) in &self.bindings {
            let resource = &binding.resource;
            if !is_portable_scope(&binding.scope)
                || !scopes.contains_key(binding.scope.as_str())
                || !is_uuid_v7(&binding.area_id)
                || !is_uuid_v7(&resource.id)
                || resource.name.is_empty()
                || resource.name.trim() != resource.name
                || resource.name.chars().count() > 255
                || resource.name.chars().any(char::is_control)
            {
                return Err(invalid("A scope Resource binding has invalid fields."));
            }
            *ids.entry(resource.id.as_str()).or_insert(0) += 1;
            *names.entry(resource.name.to_lowercase()).or_insert(0) += 1;
        }
        let outcomes = self
            .bindings
            .iter()
            .map(|BindingObject(binding)| {
                let scope = scopes[binding.scope.as_str()];
                let result = binding.bind(inventory, scope).and_then(|prepared| {
                    let resource = &prepared.resource;
                    if ids[resource.id] != 1 || names[&resource.name.to_lowercase()] != 1 {
                        return Err(Hold::ResourceCollision);
                    }
                    Ok(prepared)
                });
                (binding.resource.id.as_str(), result)
            })
            .collect();
        Ok(PreparedResources {
            version: self.version,
            outcomes,
        })
    }
}

impl Binding {
    fn bind<'a>(
        &'a self,
        inventory: &Inventory,
        scope: &Scope<'a>,
    ) -> Result<PreparedBinding<'a>, Hold> {
        let (area, evidence) = match scope.outcome {
            ScopeOutcome::Mapped { area, evidence } => (area, evidence),
            ScopeOutcome::Held { .. } => return Err(Hold::ScopeHeld),
            ScopeOutcome::Excluded { .. } => return Err(Hold::ScopeExcluded),
        };
        if area.id != self.area_id {
            return Err(Hold::AreaMismatch);
        }
        let evidence = evidence
            .iter()
            .find(|item| item.path == self.evidence_path)
            .ok_or(Hold::UnverifiedResourceEvidence)?;
        let supported_entrypoint = match self.resource.kind {
            ResourceKind::GitRepo => {
                evidence.path.starts_with("tasks/") || evidence.path.starts_with("directives/")
            }
        };
        let uri = evidence.canonical_ref.as_str();
        if !supported_entrypoint
            || uri.len() > usize::from(u16::MAX)
            || inventory
                .entries
                .get(&evidence.path)
                .is_none_or(|entry| entry.oid != self.evidence_blob_oid)
        {
            return Err(Hold::UnverifiedResourceEvidence);
        }

        Ok(PreparedBinding {
            resource: ResourceRow {
                id: &self.resource.id,
                name: &self.resource.name,
                kind: self.resource.kind,
                uri,
            },
            link: AreaResourceRow {
                area_id: &area.id,
                resource_id: &self.resource.id,
            },
        })
    }
}
