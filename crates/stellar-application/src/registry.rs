use std::sync::Arc;

use stellar_domain::{EngineDescriptor, EngineFactory};

/// Registre des moteurs de calcul disponibles.
///
/// La racine de composition (l'exécutable) y enregistre chaque moteur ;
/// l'interface en propose la liste dans les paramètres.
#[derive(Default, Clone)]
pub struct EngineRegistry {
    factories: Vec<Arc<dyn EngineFactory>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, factory: Arc<dyn EngineFactory>) -> &mut Self {
        self.factories.push(factory);
        self
    }

    pub fn descriptors(&self) -> Vec<EngineDescriptor> {
        self.factories.iter().map(|f| f.descriptor()).collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn EngineFactory>> {
        self.factories
            .iter()
            .find(|f| f.descriptor().id == id)
            .cloned()
    }

    /// Identifiant du moteur par défaut (le premier enregistré).
    pub fn default_id(&self) -> Option<&'static str> {
        self.factories.first().map(|f| f.descriptor().id)
    }
}
