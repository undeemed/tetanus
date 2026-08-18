use std::collections::BTreeMap;

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, args: serde_json::Value) -> Result<String, ToolError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool failed: {0}")]
    Failed(String),
    #[error("unknown tool {0}")]
    Unknown(String),
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn register(&mut self, t: Box<dyn Tool>) {
        self.tools.insert(t.name().to_string(), t);
    }
    pub async fn execute(&self, name: &str, args: serde_json::Value) -> Result<String, ToolError> {
        match self.tools.get(name) {
            Some(t) => t.execute(args).await,
            None => Err(ToolError::Unknown(name.to_string())),
        }
    }
    pub fn names(&self) -> impl Iterator<Item = &String> { self.tools.keys() }
}
