use std::collections::BTreeMap;

use vox_core::{ids::HandleId, value::HandleSummary};

#[derive(Debug, Clone, PartialEq, Eq)]
struct HandleEntry {
    summary: HandleSummary,
    refs: usize,
}

#[derive(Debug, Default)]
pub struct HandleStore {
    next_id: u64,
    entries: BTreeMap<HandleId, HandleEntry>,
}

impl HandleStore {
    pub fn allocate(&mut self, summary: HandleSummary) -> HandleId {
        self.next_id += 1;
        let id = HandleId(self.next_id);
        self.entries.insert(id, HandleEntry { summary, refs: 1 });
        id
    }

    pub fn describe(&self, id: HandleId) -> Option<&HandleSummary> {
        self.entries.get(&id).map(|entry| &entry.summary)
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
        true
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn ids(&self) -> Vec<HandleId> {
        self.entries.keys().copied().collect()
    }
}
