use std::sync::Arc;

use stellar_domain::{EngineDescriptor, EngineFactory};

/// Registry of the available computation engines.
///
/// The composition root (the executable) registers each engine in it; the
/// interface offers the list in the parameters.
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

    /// Identifier of the default engine (the first one registered).
    pub fn default_id(&self) -> Option<&'static str> {
        self.factories.first().map(|f| f.descriptor().id)
    }
}
