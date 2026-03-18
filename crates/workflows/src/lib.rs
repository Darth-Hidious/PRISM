use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

const BUILTIN_FORGE_YAML: &str = include_str!("../../../app/workflows/builtin/forge.yaml");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowArgument {
    pub name: String,
    #[serde(default = "default_string_type")]
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub env: String,
    #[serde(default)]
    pub is_flag: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowStep {
    pub id: String,
    pub action: String,
    #[serde(flatten)]
    pub config: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowSpec {
    pub name: String,
    pub description: String,
    pub command_name: String,
    pub source_path: String,
    #[serde(default = "default_mode")]
    pub default_mode: String,
    #[serde(default)]
    pub arguments: Vec<WorkflowArgument>,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
    #[serde(default)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowStepResult {
    pub id: String,
    pub action: String,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRunResult {
    pub workflow: String,
    pub mode: String,
    pub context: BTreeMap<String, serde_json::Value>,
    pub steps: Vec<WorkflowStepResult>,
}

#[derive(Debug, Clone)]
pub struct WorkflowCommandRequest {
    pub name: String,
    pub execute: bool,
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct WorkflowManifest {
    #[serde(default)]
    kind: String,
    name: Option<String>,
    command_name: Option<String>,
    description: Option<String>,
    #[serde(default = "default_mode")]
    default_mode: String,
    #[serde(default)]
    arguments: Vec<WorkflowArgument>,
    #[serde(default)]
    steps: Vec<WorkflowStep>,
}

fn default_string_type() -> String {
    "string".to_string()
}

fn default_mode() -> String {
    "dry_run".to_string()
}

fn builtin_workflows() -> Result<Vec<WorkflowSpec>> {
    Ok(vec![load_workflow_from_str(
        BUILTIN_FORGE_YAML,
        "builtin:forge.yaml",
    )?])
}

pub fn workflow_search_paths(project_root: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let root = project_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    paths.push(root.join(".prism").join("workflows"));
    if let Some(home) = env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".prism").join("workflows"));
    }
    paths
}

pub fn discover_workflows(project_root: Option<&Path>) -> Result<BTreeMap<String, WorkflowSpec>> {
    let mut specs = BTreeMap::new();
    for spec in builtin_workflows()? {
        specs.insert(spec.name.clone(), spec);
    }

    for directory in workflow_search_paths(project_root) {
        if !directory.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read workflow directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if ext != "yml" && ext != "yaml" {
                continue;
            }
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read workflow file {}", path.display()))?;
            let spec = load_workflow_from_str(&text, &path.display().to_string())?;
            specs.insert(spec.name.clone(), spec);
        }
    }

    Ok(specs)
}

pub fn find_workflow<'a>(
    specs: &'a BTreeMap<String, WorkflowSpec>,
    name: &str,
) -> Option<&'a WorkflowSpec> {
    specs
        .get(name)
        .or_else(|| specs.values().find(|spec| spec.command_name == name))
}

pub fn parse_workflow_command_args(args: &[String]) -> Result<WorkflowCommandRequest> {
    if args.is_empty() {
        bail!("workflow command name is required");
    }

    let name = args[0].clone();
    let mut values = BTreeMap::new();
    let mut execute = false;
    let mut index = 1;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--execute" {
            execute = true;
            index += 1;
            continue;
        }
        if !arg.starts_with("--") {
            bail!("unexpected positional argument: {arg}");
        }
        let key = arg.trim_start_matches("--").replace('-', "_");
        if index + 1 < args.len() && !args[index + 1].starts_with("--") {
            values.insert(key, args[index + 1].clone());
            index += 2;
        } else {
            values.insert(key, "true".to_string());
            index += 1;
        }
    }

    Ok(WorkflowCommandRequest {
        name,
        execute,
        values,
    })
}

pub fn build_initial_context(
    spec: &WorkflowSpec,
    values: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut context = BTreeMap::new();
    for (key, value) in values {
        context.insert(key.clone(), serde_json::Value::String(value.clone()));
    }

    for argument in &spec.arguments {
        let missing = match context.get(&argument.name) {
            Some(value) => value == "" || value.is_null(),
            None => true,
        };
        if missing {
            if !argument.env.is_empty() {
                if let Ok(value) = env::var(&argument.env) {
                    if !value.is_empty() {
                        context.insert(argument.name.clone(), serde_json::Value::String(value));
                        continue;
                    }
                }
            }
            if let Some(default) = &argument.default {
                context.insert(argument.name.clone(), default.clone());
                continue;
            }
        }
        if argument.required
            && context
                .get(&argument.name)
                .is_none_or(|value| value.is_null() || value == "")
            && !argument.is_flag
        {
            bail!("missing required workflow argument: {}", argument.name);
        }
        context
            .entry(argument.name.clone())
            .or_insert(serde_json::Value::Null);
    }

    context
        .entry("workflow_name".to_string())
        .or_insert_with(|| serde_json::Value::String(spec.name.clone()));
    context
        .entry("command_name".to_string())
        .or_insert_with(|| serde_json::Value::String(spec.command_name.clone()));
    context
        .entry("now_iso".to_string())
        .or_insert_with(|| serde_json::Value::String(Utc::now().to_rfc3339()));
    Ok(context)
}

pub async fn execute_workflow(
    spec: &WorkflowSpec,
    values: &BTreeMap<String, String>,
    execute: bool,
) -> Result<WorkflowRunResult> {
    let mut context = build_initial_context(spec, values)?;
    let mut result = WorkflowRunResult {
        workflow: spec.name.clone(),
        mode: if execute { "execute" } else { "dry_run" }.to_string(),
        context: context.clone(),
        steps: Vec::new(),
    };

    let client = reqwest::Client::new();
    for step in &spec.steps {
        let step_result = match step.action.as_str() {
            "set" => run_set_step(step, &mut context, !execute)?,
            "message" => run_message_step(step, &mut context, !execute)?,
            "http" => run_http_step(step, &mut context, !execute, &client).await?,
            other => bail!("unsupported workflow step action: {other}"),
        };
        result.steps.push(step_result);
    }
    result.context = context;
    Ok(result)
}

fn run_set_step(
    step: &WorkflowStep,
    context: &mut BTreeMap<String, serde_json::Value>,
    dry_run: bool,
) -> Result<WorkflowStepResult> {
    let values = render_value(
        step.config
            .get("values")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
        context,
    )?;
    context.insert(step.id.clone(), values.clone());
    if let Some(map) = values.as_object() {
        for (key, value) in map {
            context.insert(key.clone(), value.clone());
        }
    }
    Ok(WorkflowStepResult {
        id: step.id.clone(),
        action: step.action.clone(),
        status: if dry_run { "planned" } else { "completed" }.to_string(),
        summary: format!(
            "set {} value(s)",
            values.as_object().map(|m| m.len()).unwrap_or(1)
        ),
        data: serde_json::json!({ "values": values }),
    })
}

fn run_message_step(
    step: &WorkflowStep,
    context: &mut BTreeMap<String, serde_json::Value>,
    dry_run: bool,
) -> Result<WorkflowStepResult> {
    let text = render_value(
        step.config
            .get("text")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String(String::new())),
        context,
    )?;
    if !dry_run {
        context.insert(step.id.clone(), serde_json::json!({ "message": text }));
    }
    Ok(WorkflowStepResult {
        id: step.id.clone(),
        action: step.action.clone(),
        status: if dry_run { "planned" } else { "completed" }.to_string(),
        summary: interpolated_display(&text),
        data: serde_json::json!({ "message": text }),
    })
}

async fn run_http_step(
    step: &WorkflowStep,
    context: &mut BTreeMap<String, serde_json::Value>,
    dry_run: bool,
    client: &reqwest::Client,
) -> Result<WorkflowStepResult> {
    let method = step
        .config
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();
    let url = render_value(
        step.config
            .get("url")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String(String::new())),
        context,
    )?;
    let headers = render_value(
        step.config
            .get("headers")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
        context,
    )?;
    let body = render_value(
        step.config
            .get("body")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        context,
    )?;
    let expect_status = step
        .config
        .get("expect_status")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([200, 201, 202]));
    let expect_statuses = expected_statuses(&expect_status)?;

    if dry_run {
        return Ok(WorkflowStepResult {
            id: step.id.clone(),
            action: step.action.clone(),
            status: "planned".to_string(),
            summary: format!("{} {}", method, url.as_str().unwrap_or_default()),
            data: serde_json::json!({
                "method": method,
                "url": url,
                "headers": headers,
                "body": body,
            }),
        });
    }

    let method = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("invalid HTTP method for workflow step {}", step.id))?;
    let mut request = client.request(method, url.as_str().unwrap_or_default());
    if let Some(map) = headers.as_object() {
        for (key, value) in map {
            if let Some(value) = value.as_str() {
                request = request.header(key, value);
            }
        }
    }
    if !body.is_null() {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status();
    let status_code = status.as_u16();
    if !expect_statuses.contains(&status_code) {
        bail!(
            "workflow step {} expected status {:?} but got {}",
            step.id,
            expect_statuses,
            status_code
        );
    }
    let headers_map = response
        .headers()
        .iter()
        .map(|(key, value)| {
            (
                key.to_string(),
                serde_json::Value::String(value.to_str().unwrap_or_default().to_string()),
            )
        })
        .collect::<serde_json::Map<String, serde_json::Value>>();
    let text = response.text().await?;
    let parsed_body =
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "text": text }));
    let stored = serde_json::json!({
        "status_code": status_code,
        "headers": headers_map,
        "body": parsed_body,
    });
    context.insert(step.id.clone(), stored.clone());
    context.insert("last_response".to_string(), stored.clone());
    Ok(WorkflowStepResult {
        id: step.id.clone(),
        action: step.action.clone(),
        status: "completed".to_string(),
        summary: summarize_http_response(status_code, &parsed_body),
        data: serde_json::json!({
            "request": {
                "method": step.config.get("method").cloned().unwrap_or_else(|| serde_json::Value::String("GET".to_string())),
                "url": url,
            },
            "response": stored,
        }),
    })
}

fn expected_statuses(value: &serde_json::Value) -> Result<Vec<u16>> {
    match value {
        serde_json::Value::Number(num) => Ok(vec![num
            .as_u64()
            .ok_or_else(|| anyhow!("invalid status code"))?
            as u16]),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_u64()
                    .map(|code| code as u16)
                    .ok_or_else(|| anyhow!("invalid status code"))
            })
            .collect(),
        _ => bail!("expect_status must be an integer or list of integers"),
    }
}

fn summarize_http_response(status_code: u16, parsed: &serde_json::Value) -> String {
    if let Some(id) = parsed.get("id") {
        return format!("HTTP {} id={}", status_code, json_display(id));
    }
    if let Some(count) = parsed.get("count") {
        return format!("HTTP {} count={}", status_code, json_display(count));
    }
    format!("HTTP {}", status_code)
}

fn json_display(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn interpolated_display(value: &serde_json::Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        json_display(value)
    }
}

pub fn load_workflow_from_str(text: &str, source_path: &str) -> Result<WorkflowSpec> {
    let raw_yaml: serde_yaml::Value = serde_yaml::from_str(text)
        .with_context(|| format!("failed to parse workflow YAML from {source_path}"))?;
    let manifest: WorkflowManifest = serde_yaml::from_str(text)
        .with_context(|| format!("failed to decode workflow manifest from {source_path}"))?;
    let kind = if manifest.kind.is_empty() {
        "workflow"
    } else {
        manifest.kind.as_str()
    };
    if kind != "workflow" {
        bail!("{source_path} is not a workflow manifest");
    }
    let name = manifest.name.unwrap_or_else(|| source_path.to_string());
    let command_name = manifest
        .command_name
        .unwrap_or_else(|| name.replace('_', "-"));
    let description = manifest
        .description
        .unwrap_or_else(|| format!("Run workflow '{name}'"));
    let raw = serde_json::to_value(raw_yaml)?;
    Ok(WorkflowSpec {
        name,
        description,
        command_name,
        source_path: source_path.to_string(),
        default_mode: manifest.default_mode,
        arguments: manifest.arguments,
        steps: manifest.steps,
        raw,
    })
}

fn render_value(
    value: serde_json::Value,
    context: &BTreeMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::String(text) => render_string(&text, context),
        serde_json::Value::Array(items) => {
            let rendered = items
                .into_iter()
                .map(|item| render_value(item, context))
                .collect::<Result<Vec<_>>>()?;
            Ok(serde_json::Value::Array(rendered))
        }
        serde_json::Value::Object(map) => {
            let mut rendered = serde_json::Map::new();
            for (key, value) in map {
                rendered.insert(key, render_value(value, context)?);
            }
            Ok(serde_json::Value::Object(rendered))
        }
        other => Ok(other),
    }
}

fn render_string(
    text: &str,
    context: &BTreeMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    if !text.contains("{{") {
        return Ok(serde_json::Value::String(text.to_string()));
    }
    if let Some(path) = whole_template(text) {
        return resolve_path(context, path);
    }

    let mut rendered = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let (before, after_start) = rest.split_at(start);
        rendered.push_str(before);
        let after_start = &after_start[2..];
        let end = after_start
            .find("}}")
            .ok_or_else(|| anyhow!("unterminated workflow template in '{text}'"))?;
        let key = after_start[..end].trim();
        let resolved = resolve_path(context, key)?;
        rendered.push_str(&interpolated_display(&resolved));
        rest = &after_start[end + 2..];
    }
    rendered.push_str(rest);
    Ok(serde_json::Value::String(rendered))
}

fn whole_template(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
        let inner = &trimmed[2..trimmed.len() - 2];
        if !inner.contains("{{") && !inner.contains("}}") {
            return Some(inner.trim());
        }
    }
    None
}

fn resolve_path(
    context: &BTreeMap<String, serde_json::Value>,
    path: &str,
) -> Result<serde_json::Value> {
    let mut current = context
        .get(path.split('.').next().unwrap_or_default())
        .cloned()
        .ok_or_else(|| anyhow!("unknown workflow context path: {path}"))?;
    for segment in path.split('.').skip(1) {
        current = match current {
            serde_json::Value::Object(map) => map
                .get(segment)
                .cloned()
                .ok_or_else(|| anyhow!("unknown workflow context path: {path}"))?,
            _ => bail!("unknown workflow context path: {path}"),
        };
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_builtin_forge() {
        let specs = discover_workflows(None).unwrap();
        let forge = find_workflow(&specs, "forge").unwrap();
        assert_eq!(forge.command_name, "forge");
        assert!(!forge.steps.is_empty());
    }

    #[test]
    fn parses_external_workflow_alias_args() {
        let parsed = parse_workflow_command_args(&[
            "forge".to_string(),
            "--paper".to_string(),
            "arxiv:2106.09685".to_string(),
            "--dataset".to_string(),
            "materials-project".to_string(),
            "--target".to_string(),
            "runpod:A100".to_string(),
            "--execute".to_string(),
        ])
        .unwrap();
        assert_eq!(parsed.name, "forge");
        assert!(parsed.execute);
        assert_eq!(parsed.values.get("paper").unwrap(), "arxiv:2106.09685");
    }

    #[test]
    fn dry_run_set_step_updates_context() {
        let spec = load_workflow_from_str(
            r#"
kind: workflow
name: test
command_name: test
arguments:
  - name: paper
    required: true
steps:
  - id: first
    action: set
    values:
      paper: "{{paper}}"
  - id: second
    action: message
    text: "paper={{paper}}"
"#,
            "inline:test",
        )
        .unwrap();
        let mut values = BTreeMap::new();
        values.insert("paper".to_string(), "abc".to_string());
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(execute_workflow(&spec, &values, false))
            .unwrap();
        assert_eq!(result.steps[0].status, "planned");
        assert_eq!(result.steps[1].summary, "paper=abc");
    }
}
