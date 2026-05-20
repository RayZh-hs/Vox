use std::collections::BTreeMap;

use vox_compiler::TreewalkScript;
use vox_core::{ids::ArtifactId, plan::CompiledArtifact};

#[derive(Debug, Clone)]
pub struct StoredArtifact {
    pub artifact: CompiledArtifact,
    pub treewalk: Option<TreewalkScript>,
}

#[derive(Debug, Default)]
pub struct ArtifactStore {
    artifacts: BTreeMap<ArtifactId, StoredArtifact>,
}

impl ArtifactStore {
    pub fn insert(&mut self, artifact: CompiledArtifact, treewalk: Option<TreewalkScript>) {
        self.artifacts
            .insert(artifact.id, StoredArtifact { artifact, treewalk });
    }

    pub fn get(&self, id: ArtifactId) -> Option<&CompiledArtifact> {
        self.artifacts.get(&id).map(|stored| &stored.artifact)
    }

    pub fn get_mut(&mut self, id: ArtifactId) -> Option<&mut CompiledArtifact> {
        self.artifacts
            .get_mut(&id)
            .map(|stored| &mut stored.artifact)
    }

    pub fn treewalk(&self, id: ArtifactId) -> Option<&TreewalkScript> {
        self.artifacts
            .get(&id)
            .and_then(|stored| stored.treewalk.as_ref())
    }

    pub fn remove(&mut self, id: ArtifactId) -> Option<CompiledArtifact> {
        self.artifacts.remove(&id).map(|stored| stored.artifact)
    }

    pub fn len(&self) -> usize {
        self.artifacts.len()
    }
}
