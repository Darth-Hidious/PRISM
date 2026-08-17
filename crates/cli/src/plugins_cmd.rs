// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! `prism plugins` — ONE inventory across every extension plane.
//!
//! STANDARD PLUGIN CONTRACT (see `app/plugins/registry.py` for the Python
//! plane's reference implementation): every plane DECLARES (id + source),
//! DISCOVERS from fixed local locations, FAILS loudly-named-isolated, and is
//! LISTABLE. This command is the LIST half, aggregated: the same inventory a
//! TUI user reaches with `/plugins list` and the agent reaches with the
//! `plugins` tool (both spawn this command; there is deliberately ONE
//! implementation, so the three surfaces can never drift).
//!
//! Everything here is local and offline: no network, no compute submission.
//! Listing the PYTHON plane executes plugin discovery in a child interpreter
//! (the same precedent as `prism tools`, which spawns the tool server to list
//! tools) — a plugin's `register()` must run for "loaded/failed" to be an
//! honest answer rather than a guess about files.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Result;
use clap::Subcommand;
use serde_json::{Value, json};

#[derive(Debug, Subcommand)]
pub enum PluginsCommands {
    /// List every extension plane's inventory: loaded and failed plugins,
    /// configured MCP servers, skills, workflows, policies, and ontologies.
    /// `--json` emits one machine-readable document.
    List {
        /// Emit JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
}

pub async fn handle(command: PluginsCommands, python_bin: &str, project_root: &Path) -> Result<()> {
    match command {
        PluginsCommands::List { json } => list(json, python_bin, project_root).await,
    }
}

/// Python-plane status: spawn `python -m app.plugins.status` and parse its
/// JSON. A missing/broken interpreter is REPORTED as the plane's failure,
/// never silently omitted — a plane that cannot answer is not "empty".
async fn python_plane(python_bin: &str, project_root: &Path) -> Value {
    let output = tokio::process::Command::new(python_bin)
        .arg("-m")
        .arg("app.plugins.status")
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => match serde_json::from_slice::<Value>(&out.stdout) {
            Ok(status) => status,
            Err(error) => json!({
                "plane": "python",
                "error": format!("plugin status produced unparseable output: {error}")
            }),
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            json!({
                "plane": "python",
                "error": format!(
                    "plugin status exited with {}: {}",
                    out.status,
                    stderr.trim().chars().take(400).collect::<String>()
                )
            })
        }
        Err(error) => json!({
            "plane": "python",
            "error": format!("failed to run `{python_bin} -m app.plugins.status`: {error}")
        }),
    }
}

/// MCP plane: configured servers from `~/.prism/mcp.json`, with statically
/// detectable failures named. Live connection state belongs to the running
/// agent session (the manager connects at startup and records per-server
/// failures); a fresh CLI process reports the CONFIG honestly instead of
/// pretending to know it.
fn mcp_plane() -> Value {
    mcp_plane_at(&prism_agent::mcp::default_config_path())
}

fn mcp_plane_at(path: &Path) -> Value {
    match prism_agent::mcp::load_config(path) {
        Ok(servers) if servers.is_empty() => json!({
            "plane": "mcp",
            "configured": [],
            "note": format!("no servers configured ({})", path.display()),
        }),
        Ok(servers) => {
            let entries: Vec<Value> = servers
                .iter()
                .map(|server| {
                    let mut entry = json!({
                        "name": server.name,
                        "transport": server.transport,
                        "source": path.display().to_string(),
                    });
                    // Statically detectable refusals — the same checks
                    // `McpManager::connect_one` applies before spawning.
                    if server.transport != "stdio" {
                        entry["will_fail"] = json!(format!(
                            "transport '{}' is not supported yet — only 'stdio'",
                            server.transport
                        ));
                    } else if server.command.is_empty() {
                        entry["will_fail"] =
                            json!("stdio MCP server needs a 'command'".to_string());
                    }
                    entry
                })
                .collect();
            json!({
                "plane": "mcp",
                "configured": entries,
                "note": "connection state is established by the agent session at startup; \
                         per-server failures are recorded by the MCP manager there",
            })
        }
        Err(error) => json!({
            "plane": "mcp",
            "error": format!("malformed MCP config {}: {error:#}", path.display()),
        }),
    }
}

/// Skills plane: authored JSON + human Markdown procedures. Local file reads
/// only — a skill body is never EXECUTED to list it.
fn skills_plane() -> Value {
    let policy = prism_agent::skills::SkillSurfacePolicy::default();
    let authored: Vec<String> = prism_agent::skills::load_all()
        .into_iter()
        .map(|s| s.name)
        .collect();
    let human = prism_agent::skills::discover_human_skills(&policy);
    let human_names: Vec<String> = human.skills.iter().map(|s| s.name.clone()).collect();
    let failures: Vec<String> = human
        .errors
        .iter()
        .map(|error| format!("{}: {}", error.path.display(), error.message))
        .collect();
    json!({
        "plane": "skills",
        "authored": authored,
        "human": human_names,
        "failed": failures,
    })
}

/// Workflows plane: builtin + discovered YAML specs. Discovery already logs
/// and skips malformed files one by one; counts include the builtins.
fn workflows_plane(project_root: &Path) -> Value {
    match prism_workflows::discover_workflows(Some(project_root)) {
        Ok(specs) => json!({
            "plane": "workflows",
            "loaded": specs.keys().collect::<Vec<_>>(),
        }),
        Err(error) => json!({
            "plane": "workflows",
            "error": format!("workflow discovery failed: {error:#}"),
        }),
    }
}

/// Policies plane: Rego files in the two user locations. Presence listing
/// only — compiling every policy to list it is the engine's job at
/// enforcement time; a file that fails to compile is refused loudly there.
fn policies_plane(project_root: &Path) -> Value {
    let global = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".prism/policies");
    let project = project_root.join(".prism/policies");
    let mut loaded: Vec<String> = Vec::new();
    for dir in [global, project] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rego") {
                loaded.push(path.display().to_string());
            }
        }
    }
    json!({ "plane": "policies", "discovered": loaded })
}

/// Ontologies plane: the builtin registry plus the project catalog. Project
/// artifacts are LOADED and validated — a broken artifact is a named failure,
/// not a silently missing row.
fn ontologies_plane(project_root: &Path) -> Value {
    let mut registered: Vec<String> = prism_ingest::ontologies::OntologyRegistry::builtin()
        .ids()
        .iter()
        .map(|id| id.to_string())
        .collect();
    let catalog = project_root.join(prism_ingest::ontologies::PROJECT_ONTOLOGY_DIR);
    let mut project: Vec<Value> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&catalog) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ttl") {
                continue;
            }
            match prism_ingest::induction::load_validated(&path) {
                Ok(ontology) => {
                    let id = ontology.domain.clone();
                    registered.push(format!("{id} (project artifact)"));
                    project.push(json!({
                        "id": id,
                        "status": ontology.status.as_str(),
                        "path": path.display().to_string(),
                    }));
                }
                Err(error) => failed.push(json!({
                    "path": path.display().to_string(),
                    "error": format!("{error:#}"),
                })),
            }
        }
    }
    json!({
        "plane": "ontologies",
        "registered": registered,
        "project_artifacts": project,
        "failed": failed,
    })
}

async fn list(json_output: bool, python_bin: &str, project_root: &Path) -> Result<()> {
    let planes = json!([
        python_plane(python_bin, project_root).await,
        mcp_plane(),
        skills_plane(),
        workflows_plane(project_root),
        policies_plane(project_root),
        ontologies_plane(project_root),
    ]);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&planes)?);
        return Ok(());
    }

    println!("PRISM plugin inventory (all extension planes)");
    println!("  agent tool: `plugins` · TUI: /plugins list · CLI: prism plugins list");
    for plane in planes.as_array().expect("built as an array") {
        println!();
        let name = plane["plane"].as_str().unwrap_or("?");
        match name {
            "python" => {
                println!("python plugins:");
                if let Some(error) = plane["error"].as_str() {
                    println!("  ERROR: {error}");
                    continue;
                }
                let empty_map = serde_json::Map::new();
                let loaded = plane["loaded"].as_object().unwrap_or(&empty_map);
                let failed = plane["failed"].as_object().unwrap_or(&empty_map);
                if loaded.is_empty() && failed.is_empty() {
                    println!("  (none discovered)");
                }
                for (plugin, source) in loaded {
                    println!("  loaded: {plugin}  [{source}]");
                }
                for (plugin, reason) in failed {
                    println!("  FAILED: {plugin}  [{reason}]");
                }
            }
            "mcp" => {
                println!("mcp servers (~/.prism/mcp.json):");
                if let Some(error) = plane["error"].as_str() {
                    println!("  ERROR: {error}");
                    continue;
                }
                let configured = plane["configured"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if configured.is_empty() {
                    println!("  (none configured)");
                }
                for server in configured {
                    let name = server["name"].as_str().unwrap_or("?");
                    let transport = server["transport"].as_str().unwrap_or("?");
                    match server["will_fail"].as_str() {
                        Some(reason) => {
                            println!("  WILL FAIL: {name} ({transport}) — {reason}");
                        }
                        None => println!("  configured: {name} ({transport})"),
                    }
                }
                if let Some(note) = plane["note"].as_str() {
                    println!("  note: {note}");
                }
            }
            "skills" => {
                println!("skills:");
                let authored = plane["authored"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let human = plane["human"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                let failed = plane["failed"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                if authored.is_empty() && human.is_empty() && failed.is_empty() {
                    println!("  (none discovered)");
                }
                for skill in authored {
                    println!("  authored: {}", skill.as_str().unwrap_or("?"));
                }
                for skill in human {
                    println!("  human:    {}", skill.as_str().unwrap_or("?"));
                }
                for failure in failed {
                    println!("  FAILED:   {}", failure.as_str().unwrap_or("?"));
                }
            }
            "workflows" => {
                println!("workflows:");
                if let Some(error) = plane["error"].as_str() {
                    println!("  ERROR: {error}");
                    continue;
                }
                let loaded = plane["loaded"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                if loaded.is_empty() {
                    println!("  (none discovered)");
                }
                for workflow in loaded {
                    println!("  loaded: {}", workflow.as_str().unwrap_or("?"));
                }
            }
            "policies" => {
                println!("policies (rego):");
                let discovered = plane["discovered"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if discovered.is_empty() {
                    println!("  (none discovered)");
                }
                for policy in discovered {
                    println!("  discovered: {}", policy.as_str().unwrap_or("?"));
                }
                println!("  note: compile status is enforced at evaluation time (fail-closed)");
            }
            "ontologies" => {
                println!("ontologies:");
                let registered = plane["registered"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let failed = plane["failed"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                for ontology in registered {
                    println!("  registered: {}", ontology.as_str().unwrap_or("?"));
                }
                for failure in failed {
                    let path = failure["path"].as_str().unwrap_or("?");
                    let error = failure["error"].as_str().unwrap_or("?");
                    println!("  FAILED:     {path} — {error}");
                }
            }
            other => println!("{other}: (unknown plane shape)"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MCP plane's statically detectable failures must be NAMED in the
    /// inventory — an unsupported transport or a missing command is a server
    /// that WILL fail at connect time, and a list that showed it as healthy
    /// config would be a lie by omission.
    #[test]
    fn mcp_plane_names_statically_detectable_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{ "servers": [
                { "name": "remote-http", "transport": "http", "url": "https://x" },
                { "name": "no-command", "transport": "stdio" },
                { "name": "fine", "command": "npx" }
            ] }"#,
        )
        .unwrap();

        let plane = mcp_plane_at(&path);
        let configured = plane["configured"].as_array().unwrap();
        assert!(
            configured[0]["will_fail"]
                .as_str()
                .unwrap()
                .contains("not supported")
        );
        assert!(
            configured[1]["will_fail"]
                .as_str()
                .unwrap()
                .contains("command")
        );
        assert!(configured[2]["will_fail"].is_null(), "healthy config row");
    }

    /// The policies plane lists DISCOVERED files from both locations and
    /// never invents compile status (that is enforced fail-closed at
    /// evaluation time, and the output says so).
    #[test]
    fn policies_plane_discovers_rego_files_in_both_locations() {
        let project = tempfile::tempdir().unwrap();
        let project_policies = project.path().join(".prism/policies");
        std::fs::create_dir_all(&project_policies).unwrap();
        std::fs::write(project_policies.join("allow-read.rego"), "package x\n").unwrap();
        std::fs::write(project_policies.join("notes.txt"), "not a policy\n").unwrap();

        let plane = policies_plane(project.path());
        let discovered: Vec<String> = plane["discovered"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        assert!(
            discovered
                .iter()
                .any(|path| path.ends_with("allow-read.rego")),
            "{discovered:?}"
        );
        assert!(
            !discovered.iter().any(|path| path.ends_with("notes.txt")),
            "non-rego files are not policies"
        );
    }

    /// The ontologies plane: builtins always listed; a BROKEN project
    /// artifact is a named failure, never a silently missing row.
    #[test]
    fn ontologies_plane_names_broken_artifacts() {
        let project = tempfile::tempdir().unwrap();
        let catalog = project.path().join(".prism/ontologies");
        std::fs::create_dir_all(&catalog).unwrap();
        std::fs::write(catalog.join("broken.ttl"), "this is not turtle @ ; ;").unwrap();

        let plane = ontologies_plane(project.path());
        let registered = plane["registered"].as_array().unwrap();
        assert!(
            registered
                .iter()
                .any(|value| value.as_str() == Some("emmo")),
            "builtins are always listed"
        );
        let failed = plane["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0]["error"].as_str().unwrap().len() > 10);
    }
}
