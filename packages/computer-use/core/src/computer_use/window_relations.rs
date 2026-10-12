//! Pure window ownership rules shared by capture, pointer routing and keyboard focus.
//! PID, titles, geometry and z-order are deliberately not ownership evidence.
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WindowKind {
    Document,
    Sheet,
    Popover,
    Auxiliary,
    Passive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowRelation {
    pub kind: WindowKind,
    owners: BTreeMap<u32, BTreeSet<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WindowRelations(pub BTreeMap<u32, WindowRelation>);

impl WindowRelations {
    pub fn merge(&mut self, id: u32, relation: WindowRelation) {
        self.record(id, relation.kind, None);
        for (owner, evidence) in relation.owners {
            for attribute in evidence {
                self.record(id, relation.kind, Some((owner, &attribute)));
            }
        }
    }
    pub fn record(&mut self, id: u32, kind: WindowKind, owner: Option<(u32, &str)>) {
        let relation = self.0.entry(id).or_insert(WindowRelation {
            kind,
            owners: BTreeMap::new(),
        });
        // A document classification is a hard boundary, even if some other
        // AX path links it to another window. Generic content must not erase it.
        if kind == WindowKind::Document
            || relation.kind == WindowKind::Unknown
            || (relation.kind == WindowKind::Auxiliary
                && matches!(
                    kind,
                    WindowKind::Sheet | WindowKind::Popover | WindowKind::Passive
                ))
        {
            relation.kind = kind;
        }
        if let Some((owner, evidence)) = owner.filter(|(owner, _)| *owner != id) {
            relation
                .owners
                .entry(owner)
                .or_default()
                .insert(evidence.into());
        }
    }

    pub fn kind(&self, id: u32) -> WindowKind {
        self.0.get(&id).map_or(WindowKind::Unknown, |r| r.kind)
    }

    pub fn belongs_to(&self, id: u32, root: u32) -> bool {
        self.reaches_owner(id, root, &mut BTreeSet::new(), &mut BTreeMap::new())
    }

    fn reaches_owner(
        &self,
        id: u32,
        root: u32,
        seen: &mut BTreeSet<u32>,
        cache: &mut BTreeMap<u32, bool>,
    ) -> bool {
        if id == root {
            return true;
        }
        if let Some(result) = cache.get(&id) {
            return *result;
        }
        if seen.len() >= 32 || !seen.insert(id) {
            return false;
        }
        let matches = self.0.get(&id).is_some_and(|relation| {
            !matches!(relation.kind, WindowKind::Document | WindowKind::Unknown)
                && !relation.owners.is_empty()
                // AXWindow may name the document while AXParent names a sheet.
                // Every ownership path must agree; one good path cannot hide
                // conflicting evidence that leads to a sibling document.
                && relation.owners.keys().all(|owner| self.reaches_owner(*owner, root, seen, cache))
        });
        seen.remove(&id);
        cache.insert(id, matches);
        matches
    }

    fn nearest_owner(&self, id: u32) -> Option<u32> {
        let relation = self.0.get(&id)?;
        relation.owners.keys().copied().find(|candidate| {
            relation
                .owners
                .keys()
                .all(|owner| candidate == owner || self.belongs_to(*candidate, *owner))
        })
    }

    pub fn receipt(&self, id: u32) -> Value {
        let relation = self.0.get(&id);
        let owner = self.nearest_owner(id);
        json!({
            "window_id": id.to_string(),
            "kind": self.kind(id),
            "owner_window_id": owner.map(|id| id.to_string()),
            "relationship_evidence": relation.map(|r| r.owners.iter().map(|(owner, evidence)| {
                json!({"owner_window_id": owner.to_string(), "attributes": evidence})
            }).collect::<Vec<_>>()).unwrap_or_default(),
            "relationship_uncertain": relation.is_none_or(|r| r.kind == WindowKind::Unknown || (r.kind != WindowKind::Document && owner.is_none())),
        })
    }

    pub fn resolve_focus(&self, requested: u32, focused: Option<u32>) -> &'static str {
        match focused {
            Some(id) if self.kind(id) == WindowKind::Passive => "unresolved",
            Some(id) if id == requested => "requested_window",
            Some(id) if self.belongs_to(id, requested) => "owned_auxiliary",
            Some(id) if self.kind(id) == WindowKind::Document => "other_document",
            _ => "unresolved",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_requires_evidence_and_never_crosses_documents() {
        let mut relations = WindowRelations::default();
        relations.record(1, WindowKind::Document, None);
        relations.record(2, WindowKind::Document, Some((1, "AXChildren")));
        relations.record(3, WindowKind::Sheet, Some((1, "AXSheets")));
        relations.record(4, WindowKind::Popover, Some((3, "AXParent")));
        relations.record(4, WindowKind::Popover, Some((1, "AXWindow")));
        relations.record(5, WindowKind::Auxiliary, None);
        assert!(!relations.belongs_to(2, 1));
        assert!(relations.belongs_to(4, 1));
        assert_eq!(relations.receipt(4)["owner_window_id"], "3");
        assert_eq!(relations.receipt(4)["relationship_uncertain"], false);
        assert!(!relations.belongs_to(5, 1));
        assert_eq!(relations.resolve_focus(1, Some(2)), "other_document");
        assert_eq!(relations.resolve_focus(1, Some(4)), "owned_auxiliary");
        assert_eq!(relations.resolve_focus(1, Some(5)), "unresolved");
        // Explicit selection of the editor remains possible without guessing its owner.
        assert_eq!(relations.resolve_focus(5, Some(5)), "requested_window");
        assert_eq!(relations.resolve_focus(1, None), "unresolved");
    }

    #[test]
    fn conflicting_owners_cycles_and_passive_surfaces_cannot_receive_redirected_keys() {
        let mut relations = WindowRelations::default();
        relations.record(3, WindowKind::Auxiliary, Some((1, "AXParent")));
        relations.record(3, WindowKind::Auxiliary, Some((2, "AXWindow")));
        assert!(!relations.belongs_to(3, 1));
        assert_eq!(relations.receipt(3)["relationship_uncertain"], true);
        relations.record(4, WindowKind::Auxiliary, Some((5, "AXParent")));
        relations.record(5, WindowKind::Auxiliary, Some((4, "AXParent")));
        assert!(!relations.belongs_to(4, 1));
        relations.record(6, WindowKind::Passive, Some((1, "AXChildren")));
        assert!(relations.belongs_to(6, 1));
        assert_eq!(relations.resolve_focus(1, Some(6)), "unresolved");
        assert_eq!(relations.resolve_focus(6, Some(6)), "unresolved");
    }
}
