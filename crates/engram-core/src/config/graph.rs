//! Role lookups over the ontology and `describe_ontology`.

use super::*;

impl GraphConfig {
    /// Parse a stored document into the live config. `None` means the graph
    /// never customized; a corrupt document reads as defaults with a warning —
    /// config can never brick an open.
    pub fn from_stored(raw: Option<&str>) -> Self {
        let Some(raw) = raw else {
            return Self::default();
        };
        match serde_json::from_str(raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("engram: couldn't read graph config, using defaults: {e}");
                Self::default()
            }
        }
    }

    /// The type definition for `name`, if the ontology declares it.
    pub fn type_def(&self, name: &str) -> Option<&TypeDef> {
        self.ontology.types.iter().find(|t| t.name == name)
    }

    /// The verb definition for `name`, if the ontology declares it.
    pub fn verb_def(&self, name: &str) -> Option<&VerbDef> {
        self.ontology.verbs.iter().find(|v| v.name == name)
    }

    /// Names of the types carrying the `worklist` role (open/resolved
    /// lifecycle; Problem/Intent in the shipped set).
    pub fn worklist_types(&self) -> Vec<&str> {
        self.ontology
            .types
            .iter()
            .filter(|t| t.roles.worklist)
            .map(|t| t.name.as_str())
            .collect()
    }

    /// Names of the types carrying the `tombstone` role (deletion markers;
    /// Tombstone in the shipped set). Zero or more — ontologies without one
    /// simply have no tombstone behavior.
    pub fn tombstone_types(&self) -> Vec<&str> {
        self.ontology
            .types
            .iter()
            .filter(|t| t.roles.tombstone)
            .map(|t| t.name.as_str())
            .collect()
    }

    /// The first type carrying the `tombstone` role, if any — the type a
    /// delete-with-tombstone gesture mints.
    pub fn tombstone_type(&self) -> Option<&str> {
        self.ontology
            .types
            .iter()
            .find(|t| t.roles.tombstone)
            .map(|t| t.name.as_str())
    }

    /// The custom-field definition for `name`, if the graph declares it.
    pub fn field_def(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The custom fields that apply to nodes of `type_name` (empty
    /// `applies_to` = every type).
    pub fn fields_for(&self, type_name: &str) -> Vec<&FieldDef> {
        self.fields
            .iter()
            .filter(|f| f.applies_to.is_empty() || f.applies_to.iter().any(|t| t == type_name))
            .collect()
    }

    /// Names of the custom fields that join the search index, sorted — the
    /// stable fingerprint the re-embed guard and the keyword index key on.
    pub fn indexed_field_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .fields
            .iter()
            .filter(|f| f.indexed)
            .map(|f| f.name.as_str())
            .collect();
        v.sort_unstable();
        v
    }

    /// The one verb carrying the `supersession` role (`replaces` in the
    /// shipped set). Validation guarantees exactly one exists.
    pub fn supersession_verb(&self) -> &str {
        self.ontology
            .verbs
            .iter()
            .find(|v| v.roles.supersession)
            .map_or("replaces", |v| v.name.as_str())
    }

    /// The one verb carrying the `contradiction` role (`conflicts-with` in
    /// the shipped set). Validation guarantees exactly one exists.
    pub fn contradiction_verb(&self) -> &str {
        self.ontology
            .verbs
            .iter()
            .find(|v| v.roles.contradiction)
            .map_or("conflicts-with", |v| v.name.as_str())
    }

    /// Whether `verb` carries inheritance (see [`VerbRoles::inherits`]).
    pub fn verb_inherits(&self, verb: &str) -> bool {
        self.ontology
            .verbs
            .iter()
            .any(|v| v.name == verb && v.inherits())
    }

    /// The supersession verb as an [`EdgeType`], for creating edges.
    pub fn supersession_edge(&self) -> crate::types::EdgeType {
        crate::types::EdgeType::parse(self.supersession_verb())
            .unwrap_or(crate::types::EdgeType::Replaces)
    }

    /// The contradiction verb as an [`EdgeType`], for creating edges.
    pub fn contradiction_edge(&self) -> crate::types::EdgeType {
        crate::types::EdgeType::parse(self.contradiction_verb())
            .unwrap_or(crate::types::EdgeType::ConflictsWith)
    }

    /// A compact teaching rendition of this graph's ontology — the same
    /// content whether it rides at the top of the brief (the optional
    /// `brief.ontology` section) or answers the `describe_ontology` MCP tool.
    /// For graphs whose ontology the assistant can't know from its skill.
    pub fn describe_ontology(&self) -> String {
        fn paren(roles: &[&str]) -> String {
            if roles.is_empty() {
                String::new()
            } else {
                format!(" ({})", roles.join("; "))
            }
        }
        let mut out = String::from("This graph's ontology — node types:\n");
        for t in &self.ontology.types {
            let mut roles = Vec::new();
            if t.roles.worklist {
                roles.push("worklist: carries open/resolved status");
            }
            if t.roles.anchor {
                roles.push("anchor: a code subject, carries code_refs");
            }
            if t.roles.tombstone {
                roles.push("tombstone: marks knowledge deliberately removed — don't resurrect it");
            }
            out.push_str(&format!(
                "- {} — \"{}\"; default durability {}{}\n",
                t.name,
                t.thought,
                t.durability.as_str(),
                paren(&roles)
            ));
        }
        if !self.fields.is_empty() {
            out.push_str(
                "Custom fields (pass as a `fields` object on add_note/update_node, e.g. \
                 {\"fields\": {\"name\": value}}):\n",
            );
            for f in &self.fields {
                let applies = if f.applies_to.is_empty() {
                    "every type".to_string()
                } else {
                    f.applies_to.join("/")
                };
                let mut line = format!(
                    "- {} ({}{}; on {})",
                    f.name,
                    f.kind.as_str(),
                    if f.required { ", REQUIRED" } else { "" },
                    applies
                );
                if f.kind == FieldKind::Enum {
                    line.push_str(&format!(" — one of: {}", f.values.join(" | ")));
                }
                if !f.label.is_empty() {
                    line.push_str(&format!(" — \"{}\"", f.label));
                }
                line.push('\n');
                out.push_str(&line);
            }
        }
        out.push_str("Edge verbs (a triple must read as English):\n");
        for v in &self.ontology.verbs {
            let mut roles = Vec::new();
            if v.roles.supersession {
                roles.push("supersession: archives the older endpoint");
            }
            if v.roles.contradiction {
                roles.push("contradiction: flags a conflict, demotes trust");
            }
            if v.roles.reason {
                roles.push("the reason edge");
            }
            if v.roles.answer {
                roles.push("closes worklist nodes");
            }
            if v.roles.dependency {
                roles.push("a live dependency");
            }
            if v.inherits() {
                roles.push("inherits: a contradiction of the target reaches the source");
            }
            out.push_str(&format!(
                "- {} — e.g. {}{}\n",
                v.name,
                v.reads_as,
                paren(&roles)
            ));
        }
        out
    }
}
