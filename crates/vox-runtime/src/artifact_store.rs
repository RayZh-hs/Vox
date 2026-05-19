use std::collections::BTreeMap;

use vox_core::{ids::ArtifactId, plan::CompiledArtifact};

#[derive(Debug, Default)]
pub struct ArtifactStore {
    artifacts: BTreeMap<ArtifactId, CompiledArtifact>,
}

impl ArtifactStore {
    pub fn insert(&mut self, artifact: CompiledArtifact) {
        self.artifacts.insert(artifact.id, artifact);
    }

    pub fn get(&self, id: ArtifactId) -> Option<&CompiledArtifact> {
        self.artifacts.get(&id)
    }

    pub fn get_mut(&mut self, id: ArtifactId) -> Option<&mut CompiledArtifact> {
        self.artifacts.get_mut(&id)
    }

    pub fn remove(&mut self, id: ArtifactId) -> Option<CompiledArtifact> {
        self.artifacts.remove(&id)
    }

    pub fn len(&self) -> usize {
        self.artifacts.len()
    }
}
