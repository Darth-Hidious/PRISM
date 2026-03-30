use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Manifest types (loaded from manifest.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandArg {
    pub name: String,
    /// One of: "string", "file", "number", "bool", "list".
    pub arg_type: String,
    #[serde(default)]
    pub required: bool,
    pub default: Option<serde_json::Value>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCommand {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub args: Vec<CommandArg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    #[serde(default)]
    pub commands: Vec<ToolCommand>,
    /// Python package dependencies required by the tool.
    #[serde(default)]
    pub requires: Vec<String>,
}

// ---------------------------------------------------------------------------
// Registry entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub manifest: ToolManifest,
    /// Directory containing the tool (parent of manifest.json).
    pub path: PathBuf,
    pub installed_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// In-memory registry of discovered tool manifests.
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: Vec<ToolEntry>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Walk immediate children of `dir` looking for `manifest.json` files.
    /// Returns the number of manifests successfully loaded.
    pub fn scan_directory(&mut self, dir: &Path) -> Result<usize> {
        let entries = fs::read_dir(dir)
            .with_context(|| format!("failed to read directory: {}", dir.display()))?;

        let mut count = 0usize;
        for entry in entries {
            let entry = entry?;
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }

            match Self::load_manifest(&manifest_path) {
                Ok(manifest) => {
                    let tool_dir = entry.path();
                    debug!(name = %manifest.name, path = %tool_dir.display(), "loaded tool manifest");

                    // Replace existing entry with the same name.
                    self.tools.retain(|t| t.manifest.name != manifest.name);
                    self.tools.push(ToolEntry {
                        manifest,
                        path: tool_dir,
                        installed_at: Some(Utc::now()),
                    });
                    count += 1;
                }
                Err(e) => {
                    warn!(path = %manifest_path.display(), error = %e, "skipping invalid manifest");
                }
            }
        }

        Ok(count)
    }

    /// Register a pre-built entry (e.g. from a remote source).
    pub fn register(&mut self, entry: ToolEntry) {
        self.tools.retain(|t| t.manifest.name != entry.manifest.name);
        self.tools.push(entry);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.iter().find(|t| t.manifest.name == name)
    }

    /// Return all registered tools.
    pub fn list(&self) -> &[ToolEntry] {
        &self.tools
    }

    /// Find a specific command within a named tool.
    pub fn find_command(&self, tool_name: &str, command_name: &str) -> Option<(&ToolEntry, &ToolCommand)> {
        let entry = self.get(tool_name)?;
        let cmd = entry.manifest.commands.iter().find(|c| c.name == command_name)?;
        Some((entry, cmd))
    }

    /// Remove a tool by name. Returns `true` if it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.tools.len();
        self.tools.retain(|t| t.manifest.name != name);
        self.tools.len() < before
    }

    // -- private helpers ----------------------------------------------------

    fn load_manifest(path: &Path) -> Result<ToolManifest> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let manifest: ToolManifest = serde_json::from_str(&contents)
            .with_context(|| format!("invalid JSON in {}", path.display()))?;
        Ok(manifest)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample_manifest() -> ToolManifest {
        ToolManifest {
            name: "phase-diagram".into(),
            version: "1.0.0".into(),
            description: "Compute binary phase diagrams via CALPHAD".into(),
            author: Some("MARC27".into()),
            commands: vec![ToolCommand {
                name: "compute".into(),
                description: "Run phase diagram calculation".into(),
                args: vec![
                    CommandArg {
                        name: "system".into(),
                        arg_type: "string".into(),
                        required: true,
                        default: None,
                        description: Some("Chemical system, e.g. Al-Cu".into()),
                    },
                    CommandArg {
                        name: "temperature".into(),
                        arg_type: "number".into(),
                        required: false,
                        default: Some(serde_json::json!(300.0)),
                        description: Some("Temperature in Kelvin".into()),
                    },
                ],
            }],
            requires: vec!["pycalphad>=0.10".into()],
        }
    }

    fn sample_entry() -> ToolEntry {
        ToolEntry {
            manifest: sample_manifest(),
            path: PathBuf::from("/tmp/tools/phase-diagram"),
            installed_at: Some(Utc::now()),
        }
    }

    #[test]
    fn register_and_retrieve() {
        let mut reg = ToolRegistry::new();
        assert!(reg.list().is_empty());

        reg.register(sample_entry());
        assert_eq!(reg.list().len(), 1);

        let entry = reg.get("phase-diagram").expect("tool should be present");
        assert_eq!(entry.manifest.version, "1.0.0");
        assert_eq!(entry.manifest.requires, vec!["pycalphad>=0.10"]);
    }

    #[test]
    fn find_command_by_name() {
        let mut reg = ToolRegistry::new();
        reg.register(sample_entry());

        let (entry, cmd) = reg
            .find_command("phase-diagram", "compute")
            .expect("command should exist");
        assert_eq!(entry.manifest.name, "phase-diagram");
        assert_eq!(cmd.args.len(), 2);
        assert!(cmd.args[0].required);
        assert!(!cmd.args[1].required);

        assert!(reg.find_command("phase-diagram", "nonexistent").is_none());
        assert!(reg.find_command("nonexistent", "compute").is_none());
    }

    #[test]
    fn scan_picks_up_manifests() {
        let tmp = tempfile::tempdir().expect("create temp dir");

        // Create two tool directories with manifests.
        for name in &["tool-a", "tool-b"] {
            let tool_dir = tmp.path().join(name);
            fs::create_dir_all(&tool_dir).unwrap();
            let manifest = serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "description": format!("Test tool {name}"),
                "commands": [],
            });
            fs::write(tool_dir.join("manifest.json"), manifest.to_string()).unwrap();
        }

        // Also create a directory WITHOUT a manifest — should be ignored.
        fs::create_dir_all(tmp.path().join("not-a-tool")).unwrap();

        let mut reg = ToolRegistry::new();
        let found = reg.scan_directory(tmp.path()).expect("scan should succeed");
        assert_eq!(found, 2);
        assert_eq!(reg.list().len(), 2);
        assert!(reg.get("tool-a").is_some());
        assert!(reg.get("tool-b").is_some());
    }

    #[test]
    fn remove_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(sample_entry());
        assert!(reg.get("phase-diagram").is_some());

        assert!(reg.remove("phase-diagram"));
        assert!(reg.get("phase-diagram").is_none());
        assert!(reg.list().is_empty());

        // Removing again returns false.
        assert!(!reg.remove("phase-diagram"));
    }

    #[test]
    fn register_replaces_existing() {
        let mut reg = ToolRegistry::new();
        reg.register(sample_entry());

        // Register again with a new version.
        let mut updated = sample_entry();
        updated.manifest.version = "2.0.0".into();
        reg.register(updated);

        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.get("phase-diagram").unwrap().manifest.version, "2.0.0");
    }

    // -- Edge cases and error paths -------------------------------------------

    #[test]
    fn get_nonexistent_returns_none() {
        let reg = ToolRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let mut reg = ToolRegistry::new();
        assert!(!reg.remove("ghost"));
    }

    #[test]
    fn find_command_nonexistent_tool() {
        let reg = ToolRegistry::new();
        assert!(reg.find_command("no-tool", "no-cmd").is_none());
    }

    #[test]
    fn scan_empty_directory() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let mut reg = ToolRegistry::new();
        let found = reg.scan_directory(tmp.path()).unwrap();
        assert_eq!(found, 0);
        assert!(reg.list().is_empty());
    }

    #[test]
    fn scan_nonexistent_directory_errors() {
        let mut reg = ToolRegistry::new();
        let result = reg.scan_directory(Path::new("/nonexistent/path/tools"));
        assert!(result.is_err());
    }

    #[test]
    fn scan_skips_invalid_json_manifests() {
        let tmp = tempfile::tempdir().unwrap();

        // Valid tool.
        let good_dir = tmp.path().join("good-tool");
        fs::create_dir_all(&good_dir).unwrap();
        fs::write(
            good_dir.join("manifest.json"),
            serde_json::json!({
                "name": "good-tool",
                "version": "1.0.0",
                "description": "Works fine",
                "commands": [],
            })
            .to_string(),
        )
        .unwrap();

        // Invalid JSON.
        let bad_dir = tmp.path().join("bad-tool");
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(bad_dir.join("manifest.json"), "NOT VALID JSON{{{").unwrap();

        // Missing required fields.
        let incomplete_dir = tmp.path().join("incomplete-tool");
        fs::create_dir_all(&incomplete_dir).unwrap();
        fs::write(
            incomplete_dir.join("manifest.json"),
            serde_json::json!({"name": "incomplete"}).to_string(),
        )
        .unwrap();

        let mut reg = ToolRegistry::new();
        let found = reg.scan_directory(tmp.path()).unwrap();
        assert_eq!(found, 1); // Only the valid one.
        assert!(reg.get("good-tool").is_some());
    }

    #[test]
    fn scan_replaces_existing_on_rescan() {
        let tmp = tempfile::tempdir().unwrap();
        let tool_dir = tmp.path().join("my-tool");
        fs::create_dir_all(&tool_dir).unwrap();

        // Version 1.
        fs::write(
            tool_dir.join("manifest.json"),
            serde_json::json!({
                "name": "my-tool",
                "version": "1.0.0",
                "description": "v1",
                "commands": [],
            })
            .to_string(),
        )
        .unwrap();

        let mut reg = ToolRegistry::new();
        reg.scan_directory(tmp.path()).unwrap();
        assert_eq!(reg.get("my-tool").unwrap().manifest.version, "1.0.0");

        // Update to version 2.
        fs::write(
            tool_dir.join("manifest.json"),
            serde_json::json!({
                "name": "my-tool",
                "version": "2.0.0",
                "description": "v2",
                "commands": [],
            })
            .to_string(),
        )
        .unwrap();

        reg.scan_directory(tmp.path()).unwrap();
        assert_eq!(reg.get("my-tool").unwrap().manifest.version, "2.0.0");
        assert_eq!(reg.list().len(), 1); // Not duplicated.
    }

    #[test]
    fn manifest_serde_roundtrip() {
        let m = sample_manifest();
        let json = serde_json::to_string(&m).unwrap();
        let parsed: ToolManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, m.name);
        assert_eq!(parsed.version, m.version);
        assert_eq!(parsed.commands.len(), m.commands.len());
        assert_eq!(parsed.requires, m.requires);
    }

    #[test]
    fn manifest_minimal_json_deserializes() {
        let json = r#"{"name":"min","version":"0.1","description":"Minimal"}"#;
        let m: ToolManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.name, "min");
        assert!(m.commands.is_empty());
        assert!(m.requires.is_empty());
        assert!(m.author.is_none());
    }

    #[test]
    fn command_arg_defaults() {
        let json = r#"{"name":"x","arg_type":"string"}"#;
        let arg: CommandArg = serde_json::from_str(json).unwrap();
        assert!(!arg.required); // default false
        assert!(arg.default.is_none());
        assert!(arg.description.is_none());
    }

    #[test]
    fn multiple_tools_different_names() {
        let mut reg = ToolRegistry::new();
        for i in 0..50 {
            let mut entry = sample_entry();
            entry.manifest.name = format!("tool-{i}");
            reg.register(entry);
        }
        assert_eq!(reg.list().len(), 50);
        assert!(reg.get("tool-0").is_some());
        assert!(reg.get("tool-49").is_some());
        assert!(reg.get("tool-50").is_none());
    }

    #[test]
    fn unicode_tool_names() {
        let mut reg = ToolRegistry::new();
        let mut entry = sample_entry();
        entry.manifest.name = "相图计算器🔬".into();
        reg.register(entry);
        assert!(reg.get("相图计算器🔬").is_some());
        assert!(reg.remove("相图计算器🔬"));
    }
}
