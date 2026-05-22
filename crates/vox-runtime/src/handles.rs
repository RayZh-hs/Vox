use std::collections::{BTreeMap, BTreeSet};

use vox_core::{
    ids::{ArtifactId, HandleId},
    opt::OptimizationLevel,
    value::HandleSummary,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GenericFunctionKey {
    pub artifact: ArtifactId,
    pub optimization: OptimizationLevel,
    pub module: String,
    pub function: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RealizationKey {
    pub type_arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParameterHandleSummary {
    pub name: String,
    pub bound: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericFunctionHandleSummary {
    pub name: String,
    pub generic_parameters: Vec<GenericParameterHandleSummary>,
    pub parameters: Vec<String>,
    pub return_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealizedFunctionHandleSummary {
    pub name: String,
    pub parameters: Vec<String>,
    pub return_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HandleEntry {
    payload: HandlePayload,
    refs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleMetadata {
    pub summary: HandleSummary,
    pub ref_count: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HandlePayload {
    Data(HandleSummary),
    GenericFunction(GenericFunctionHandle),
    RealizedFunction(RealizedFunctionHandle),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GenericFunctionHandle {
    signature: GenericFunctionHandleSummary,
    realized: BTreeMap<RealizationKey, HandleId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RealizedFunctionHandle {
    signature: RealizedFunctionHandleSummary,
}

#[derive(Debug, Default)]
pub struct HandleStore {
    next_id: u64,
    entries: BTreeMap<HandleId, HandleEntry>,
    free_ids: BTreeSet<HandleId>,
}

impl HandleStore {
    pub fn allocate_data(&mut self, summary: HandleSummary) -> HandleId {
        self.allocate_payload(HandlePayload::Data(summary))
    }

    pub fn allocate_generic_function(
        &mut self,
        signature: GenericFunctionHandleSummary,
        realized: BTreeMap<RealizationKey, HandleId>,
    ) -> HandleId {
        self.allocate_payload(HandlePayload::GenericFunction(GenericFunctionHandle {
            signature,
            realized,
        }))
    }

    pub fn allocate_realized_function(
        &mut self,
        signature: RealizedFunctionHandleSummary,
    ) -> HandleId {
        self.allocate_payload(HandlePayload::RealizedFunction(RealizedFunctionHandle {
            signature,
        }))
    }

    pub fn describe(&self, id: HandleId) -> Option<HandleSummary> {
        self.entries
            .get(&id)
            .map(|entry| handle_summary(&entry.payload))
    }

    pub fn metadata(&self, id: HandleId) -> Option<HandleMetadata> {
        self.entries.get(&id).map(|entry| HandleMetadata {
            summary: handle_summary(&entry.payload),
            ref_count: entry.refs.min(u32::MAX as usize) as u32,
            flags: 0,
        })
    }

    pub fn retain(&mut self, id: HandleId) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };

        entry.refs += 1;
        true
    }

    pub fn release(&mut self, id: HandleId) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };

        if entry.refs > 1 {
            entry.refs -= 1;
            return true;
        }

        self.entries.remove(&id);
        self.free_ids.insert(id);
        true
    }

    pub fn update_generic_function_realization(
        &mut self,
        id: HandleId,
        key: RealizationKey,
        realized: HandleId,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };
        let HandlePayload::GenericFunction(folder) = &mut entry.payload else {
            return false;
        };
        folder.realized.insert(key, realized);
        true
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn ids(&self) -> Vec<HandleId> {
        self.entries.keys().copied().collect()
    }

    fn allocate_payload(&mut self, payload: HandlePayload) -> HandleId {
        let id = if let Some(id) = self.free_ids.pop_first() {
            id
        } else {
            let id = HandleId(self.next_id);
            self.next_id += 1;
            id
        };
        self.entries.insert(id, HandleEntry { payload, refs: 1 });
        id
    }
}

fn handle_summary(payload: &HandlePayload) -> HandleSummary {
    match payload {
        HandlePayload::Data(summary) => summary.clone(),
        HandlePayload::GenericFunction(folder) => HandleSummary {
            type_name: "Function".to_owned(),
            summary: render_generic_function_summary(folder),
            bytes: None,
        },
        HandlePayload::RealizedFunction(function) => HandleSummary {
            type_name: "Function".to_owned(),
            summary: render_realized_function_summary(function),
            bytes: None,
        },
    }
}

fn render_generic_function_summary(folder: &GenericFunctionHandle) -> String {
    let generics = if folder.signature.generic_parameters.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            folder
                .signature
                .generic_parameters
                .iter()
                .map(|parameter| format!("{}: {}", parameter.name, parameter.bound))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "<generic function {}{} realized={}>",
        folder.signature.name,
        generics,
        folder.realized.len()
    )
}

fn render_realized_function_summary(function: &RealizedFunctionHandle) -> String {
    format!(
        "<function {}({}) -> {}>",
        function.signature.name,
        function.signature.parameters.join(", "),
        function.signature.return_type
    )
}
