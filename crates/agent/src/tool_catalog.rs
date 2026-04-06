use prism_ingest::llm::{FunctionDef, ToolDefinition};
use serde_json::{json, Value};

use crate::permissions::{get_tool_permission, PermissionMode};

/// Full metadata for one loaded tool. Rust keeps this alongside the OpenAI
/// function definition so command views, permission logic, and approval UI all
/// talk about the same concrete tool facts.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub requires_approval: bool,
    pub permission_mode: PermissionMode,
}

impl LoadedTool {
    #[must_use]
    pub fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.input_schema.clone(),
            },
        }
    }
}

/// Catalog of tools loaded from the Python registry. The LLM still receives
/// plain `ToolDefinition`s, but the runtime keeps the richer metadata here.
#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: Vec<LoadedTool>,
    definitions: Vec<ToolDefinition>,
}

impl ToolCatalog {
    #[must_use]
    pub fn from_tool_server_json(tools_json: &Value) -> Self {
        let empty = Vec::new();
        let raw_tools = tools_json
            .get("tools")
            .and_then(|value| value.as_array())
            .unwrap_or(&empty);

        let tools = raw_tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?.to_string();
                let description = tool.get("description")?.as_str()?.to_string();
                let input_schema = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                let requires_approval = tool
                    .get("requires_approval")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);

                Some(LoadedTool {
                    permission_mode: get_tool_permission(&name),
                    name,
                    description,
                    input_schema,
                    requires_approval,
                })
            })
            .collect::<Vec<_>>();

        let definitions = tools.iter().map(LoadedTool::to_definition).collect();
        Self { tools, definitions }
    }

    #[must_use]
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LoadedTool> {
        self.tools.iter()
    }

    #[must_use]
    pub fn find(&self, tool_name: &str) -> Option<&LoadedTool> {
        self.tools
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(tool_name))
    }

    pub fn extend(&mut self, extra_tools: Vec<LoadedTool>) {
        for tool in extra_tools {
            self.tools
                .retain(|loaded| !loaded.name.eq_ignore_ascii_case(&tool.name));
            self.tools.push(tool);
        }
        self.definitions = self.tools.iter().map(LoadedTool::to_definition).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_metadata_from_python_tool_server() {
        let json = json!({
            "tools": [
                {
                    "name": "execute_bash",
                    "description": "Run a guarded local bash command",
                    "input_schema": { "type": "object", "properties": { "command": { "type": "string" } } },
                    "requires_approval": true
                }
            ]
        });

        let catalog = ToolCatalog::from_tool_server_json(&json);
        let tool = catalog.find("execute_bash").expect("tool should load");

        assert_eq!(catalog.len(), 1);
        assert!(tool.requires_approval);
        assert_eq!(tool.permission_mode, PermissionMode::FullAccess);
        assert_eq!(catalog.definitions()[0].function.name, "execute_bash");
    }

    #[test]
    fn extend_rebuilds_definitions() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![LoadedTool {
            name: "query".to_string(),
            description: "Run prism query".to_string(),
            input_schema: json!({ "type": "object" }),
            requires_approval: false,
            permission_mode: PermissionMode::ReadOnly,
        }]);

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.definitions()[0].function.name, "query");
    }
}
