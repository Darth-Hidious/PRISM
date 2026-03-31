//! Hardware, dataset, model, and service detection for PRISM nodes.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use prism_proto::{DatasetInfo, GpuInfo, ModelInfo, NodeCapabilities, NodeService};
use sysinfo::{Disks, System};

/// Run a full local probe and return expanded capabilities.
pub fn probe_local_capabilities() -> NodeCapabilities {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    let mut system = System::new_all();
    system.refresh_all();

    let ram_gb = (system.total_memory() / 1024 / 1024 / 1024).max(1);

    let disks = Disks::new_with_refreshed_list();
    let disk_bytes: u64 = disks.list().iter().map(|d| d.total_space()).sum();
    let disk_gb = (disk_bytes / 1024 / 1024 / 1024).max(1);
    let avail_bytes: u64 = disks.list().iter().map(|d| d.available_space()).sum();
    let storage_available_gb = (avail_bytes / 1024 / 1024 / 1024) as u32;

    let labels = BTreeMap::from([
        ("os".to_string(), env::consts::OS.to_string()),
        ("arch".to_string(), env::consts::ARCH.to_string()),
    ]);

    let gpus = detect_gpus();
    let container_runtime = detect_container_runtime();
    let has_docker = matches!(container_runtime.as_deref(), Some("docker"));
    let datasets = detect_datasets();
    let models = detect_models();
    let services = detect_services(&gpus, storage_available_gb, &models);

    NodeCapabilities {
        gpus,
        cpu_cores,
        ram_gb,
        disk_gb,
        software: detect_software(),
        container_runtime,
        docker: has_docker,
        scheduler: detect_scheduler(),
        labels,
        storage_available_gb,
        datasets,
        models,
        services,
        visibility: "private".to_string(),
        price_per_hour_usd: None,
        public_key: None,
    }
}

/// Async version that also checks Ollama.
pub async fn probe_local_capabilities_async() -> NodeCapabilities {
    let mut caps = probe_local_capabilities();
    let ollama_services = detect_ollama().await;
    caps.services.extend(ollama_services);
    caps
}

fn detect_gpus() -> Vec<GpuInfo> {
    env::var("PRISM_NODE_GPU_JSON")
        .ok()
        .and_then(|value| serde_json::from_str::<Vec<GpuInfo>>(&value).ok())
        .unwrap_or_default()
}

fn detect_software() -> Vec<String> {
    let mut software = Vec::new();
    let probes = [
        ("docker", "docker"),
        ("podman", "podman"),
        ("apptainer", "apptainer"),
        ("singularity", "singularity"),
        ("slurm", "sbatch"),
        ("pbs", "qsub"),
        ("lammps", "lmp"),
        ("vasp", "vasp_std"),
        ("python", "python3"),
        ("ollama", "ollama"),
    ];

    for (label, executable) in probes {
        if binary_exists(executable) {
            software.push(label.to_string());
        }
    }

    if let Ok(extra) = env::var("PRISM_NODE_SOFTWARE") {
        for item in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !software.iter().any(|existing| existing == item) {
                software.push(item.to_string());
            }
        }
    }

    software.sort();
    software
}

fn detect_container_runtime() -> Option<String> {
    for candidate in ["docker", "podman", "apptainer", "singularity"] {
        if binary_exists(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn detect_scheduler() -> Option<String> {
    if binary_exists("sbatch") {
        return Some("slurm".to_string());
    }
    if binary_exists("qsub") {
        return Some("pbs".to_string());
    }
    None
}

// --- Dataset detection ---

const DATASET_EXTENSIONS: &[&str] = &[
    "csv", "json", "jsonl", "parquet", "cif", "xyz", "hdf5", "h5",
];

fn detect_datasets() -> Vec<DatasetInfo> {
    let mut search_dirs = vec![];

    // Standard paths
    for base in ["./data", "~/data", "/data", "~/.prism/data"] {
        search_dirs.push(expand_tilde(base));
    }

    // Custom paths from env
    if let Ok(extra) = env::var("PRISM_DATA_PATHS") {
        for p in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            search_dirs.push(PathBuf::from(p));
        }
    }

    let mut datasets = Vec::new();
    for dir in search_dirs {
        if dir.is_dir() {
            scan_for_datasets(&dir, &mut datasets);
        }
    }

    datasets.sort_by(|a, b| a.path.cmp(&b.path));
    datasets.dedup_by(|a, b| a.path == b.path);
    datasets
}

fn scan_for_datasets(dir: &Path, out: &mut Vec<DatasetInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if DATASET_EXTENSIONS.contains(&ext) {
                    if let Some(info) = dataset_info_from_file(&path, ext) {
                        out.push(info);
                    }
                }
            }
        } else if path.is_dir() {
            // Check if directory contains dataset files (one level deep)
            let has_data = std::fs::read_dir(&path)
                .ok()
                .map(|entries| {
                    entries.flatten().any(|e| {
                        e.path()
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| DATASET_EXTENSIONS.contains(&ext))
                    })
                })
                .unwrap_or(false);

            if has_data {
                let size_bytes = dir_size(&path);
                let name = sanitize_name(
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown"),
                );
                out.push(DatasetInfo {
                    name,
                    path: path.display().to_string(),
                    size_gb: bytes_to_gb(size_bytes),
                    entries: None,
                    format: Some("directory".to_string()),
                });
            }
        }
    }
}

fn dataset_info_from_file(path: &Path, ext: &str) -> Option<DatasetInfo> {
    let meta = std::fs::metadata(path).ok()?;
    let name = sanitize_name(
        path.file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown"),
    );
    Some(DatasetInfo {
        name,
        path: path.display().to_string(),
        size_gb: bytes_to_gb(meta.len()),
        entries: estimate_entries(path, ext),
        format: Some(ext.to_string()),
    })
}

fn estimate_entries(path: &Path, ext: &str) -> Option<u64> {
    use std::io::{BufRead, BufReader};

    match ext {
        "csv" | "jsonl" => {
            // Count lines via buffered reader — no OOM on large files
            let file = std::fs::File::open(path).ok()?;
            let reader = BufReader::new(file);
            let count = reader.lines().count() as u64;
            Some(if ext == "csv" {
                count.saturating_sub(1)
            } else {
                count
            })
        }
        _ => None,
    }
}

// --- Model detection ---

const MODEL_EXTENSIONS: &[&str] = &["pt", "pth", "onnx", "safetensors", "h5", "pkl", "joblib"];

fn detect_models() -> Vec<ModelInfo> {
    let mut search_dirs = vec![];

    for base in ["./models", "~/models", "~/.prism/models"] {
        search_dirs.push(expand_tilde(base));
    }

    if let Ok(extra) = env::var("PRISM_MODEL_PATHS") {
        for p in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            search_dirs.push(PathBuf::from(p));
        }
    }

    let mut models = Vec::new();
    for dir in search_dirs {
        if dir.is_dir() {
            scan_for_models(&dir, &mut models, 0);
        }
    }

    models.sort_by(|a, b| a.path.cmp(&b.path));
    models.dedup_by(|a, b| a.path == b.path);
    models
}

/// Max depth for model directory recursion (e.g. HuggingFace layout: models/org/model/).
const MAX_MODEL_SCAN_DEPTH: u32 = 3;

fn scan_for_models(dir: &Path, out: &mut Vec<ModelInfo>, depth: u32) {
    if depth > MAX_MODEL_SCAN_DEPTH {
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if MODEL_EXTENSIONS.contains(&ext) {
                    if let Some(info) = model_info_from_file(&path, ext) {
                        out.push(info);
                    }
                }
            }
        } else if path.is_dir() {
            scan_for_models(&path, out, depth + 1);
        }
    }
}

fn model_info_from_file(path: &Path, ext: &str) -> Option<ModelInfo> {
    let meta = std::fs::metadata(path).ok()?;
    let name = sanitize_name(
        path.file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown"),
    );
    Some(ModelInfo {
        name,
        path: path.display().to_string(),
        format: Some(format_label(ext).to_string()),
        size_gb: Some(bytes_to_gb(meta.len())),
    })
}

fn format_label(ext: &str) -> &str {
    match ext {
        "pt" | "pth" => "pytorch",
        "onnx" => "onnx",
        "safetensors" => "safetensors",
        "h5" => "keras",
        "pkl" | "joblib" => "sklearn",
        other => other,
    }
}

// --- Service detection ---

fn detect_services(
    gpus: &[GpuInfo],
    storage_available_gb: u32,
    models: &[ModelInfo],
) -> Vec<NodeService> {
    let mut services = Vec::new();

    if !gpus.is_empty() {
        services.push(NodeService {
            kind: "compute".to_string(),
            name: "GPU Compute".to_string(),
            status: "ready".to_string(),
            endpoint: None,
            model: None,
        });
    }

    if storage_available_gb > 100 {
        services.push(NodeService {
            kind: "storage".to_string(),
            name: "Local Storage".to_string(),
            status: "ready".to_string(),
            endpoint: None,
            model: None,
        });
    }

    for m in models {
        services.push(NodeService {
            kind: "inference".to_string(),
            name: format!("Inference: {}", m.name),
            status: "ready".to_string(),
            endpoint: None,
            model: Some(m.name.clone()),
        });
    }

    // Every node can bridge data
    services.push(NodeService {
        kind: "data_bridge".to_string(),
        name: "Data Bridge".to_string(),
        status: "ready".to_string(),
        endpoint: None,
        model: None,
    });

    services
}

/// Detect Ollama and running models (async — requires network).
async fn detect_ollama() -> Vec<NodeService> {
    let mut services = Vec::new();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok();

    let Some(client) = client else {
        return services;
    };

    let Ok(resp) = client.get("http://localhost:11434/api/tags").send().await else {
        return services;
    };

    let Ok(data) = resp.json::<serde_json::Value>().await else {
        return services;
    };

    if let Some(models) = data.get("models").and_then(|m| m.as_array()) {
        for model in models {
            let name = model
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            services.push(NodeService {
                kind: "llm".to_string(),
                name: format!("Ollama: {name}"),
                status: "ready".to_string(),
                endpoint: Some("http://localhost:11434".to_string()),
                model: Some(name.to_string()),
            });
        }
    }

    if services.is_empty() {
        // Ollama is running but no models loaded
        services.push(NodeService {
            kind: "llm".to_string(),
            name: "Ollama".to_string(),
            status: "unavailable".to_string(),
            endpoint: Some("http://localhost:11434".to_string()),
            model: None,
        });
    }

    services
}

// --- Helpers ---

fn binary_exists(binary: &str) -> bool {
    let path_entries: Vec<_> = env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).collect())
        .unwrap_or_default();

    path_entries.into_iter().any(|dir| {
        let plain = dir.join(binary);
        if plain.is_file() {
            return true;
        }
        if cfg!(windows) {
            for ext in ["exe", "cmd", "bat"] {
                if dir.join(format!("{binary}.{ext}")).is_file() {
                    return true;
                }
            }
        }
        false
    })
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn dirs_home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn bytes_to_gb(bytes: u64) -> f64 {
    (bytes as f64) / (1024.0 * 1024.0 * 1024.0)
}

fn dir_size(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Sanitize a name for safe transmission — no path traversal.
fn sanitize_name(name: &str) -> String {
    name.replace(['/', '\\'], "_")
        .replace("..", "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_traversal() {
        let result = sanitize_name("../../etc/passwd");
        assert!(!result.contains('/'));
        assert!(!result.contains(".."));
        assert_eq!(sanitize_name("my-model_v2.3"), "my-model_v2.3");
    }

    #[test]
    fn probe_returns_capabilities() {
        let caps = probe_local_capabilities();
        assert!(caps.cpu_cores > 0);
        assert!(caps.ram_gb > 0);
        assert!(caps.disk_gb > 0);
        assert!(caps.services.iter().any(|s| s.kind == "data_bridge"));
    }

    #[test]
    fn format_label_maps_extensions() {
        assert_eq!(format_label("pt"), "pytorch");
        assert_eq!(format_label("pth"), "pytorch");
        assert_eq!(format_label("onnx"), "onnx");
        assert_eq!(format_label("safetensors"), "safetensors");
        assert_eq!(format_label("h5"), "keras");
        assert_eq!(format_label("pkl"), "sklearn");
        assert_eq!(format_label("joblib"), "sklearn");
        assert_eq!(format_label("xyz"), "xyz"); // unknown passthrough
    }

    #[test]
    fn sanitize_name_preserves_valid_chars() {
        assert_eq!(sanitize_name("model-v2_final.pt"), "model-v2_final.pt");
    }

    #[test]
    fn sanitize_name_removes_special_chars() {
        assert_eq!(sanitize_name("bad name!@#$%"), "badname");
    }

    #[test]
    fn sanitize_name_handles_path_traversal() {
        let result = sanitize_name("../../../etc/passwd");
        assert!(!result.contains('/'));
        assert!(!result.contains(".."));
    }

    #[test]
    fn sanitize_name_empty_input() {
        assert_eq!(sanitize_name(""), "");
    }

    #[test]
    fn bytes_to_gb_conversion() {
        assert!((bytes_to_gb(1_073_741_824) - 1.0).abs() < 0.001); // 1 GiB
        assert!((bytes_to_gb(0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn expand_tilde_no_tilde() {
        assert_eq!(
            expand_tilde("/absolute/path"),
            PathBuf::from("/absolute/path")
        );
    }

    #[test]
    fn expand_tilde_relative() {
        assert_eq!(expand_tilde("./relative"), PathBuf::from("./relative"));
    }

    #[test]
    fn detect_datasets_in_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("alloys.csv"), "comp,hardness\nNbMoTaW,542\n").unwrap();
        std::fs::write(data_dir.join("phases.json"), "{}").unwrap();
        std::fs::write(data_dir.join("readme.txt"), "not a dataset").unwrap();

        let mut datasets = Vec::new();
        scan_for_datasets(&data_dir, &mut datasets);
        // Should find csv and json, not txt.
        assert_eq!(datasets.len(), 2);
        assert!(datasets.iter().any(|d| d.format.as_deref() == Some("csv")));
        assert!(datasets.iter().any(|d| d.format.as_deref() == Some("json")));
    }

    #[test]
    fn detect_models_in_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let model_dir = tmp.path().join("models");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("predictor.pt"), b"fake pytorch model").unwrap();
        std::fs::write(model_dir.join("notes.txt"), b"not a model").unwrap();

        let mut models = Vec::new();
        scan_for_models(&model_dir, &mut models, 0);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].format.as_deref(), Some("pytorch"));
    }

    #[test]
    fn estimate_entries_csv() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "header\nrow1\nrow2\nrow3\n").unwrap();
        let count = estimate_entries(tmp.path(), "csv");
        assert_eq!(count, Some(3)); // 4 lines - 1 header = 3
    }

    #[test]
    fn estimate_entries_jsonl() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "{}\n{}\n{}\n").unwrap();
        let count = estimate_entries(tmp.path(), "jsonl");
        assert_eq!(count, Some(3));
    }

    #[test]
    fn estimate_entries_unknown_format_returns_none() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        assert!(estimate_entries(tmp.path(), "parquet").is_none());
    }

    #[test]
    fn detect_services_no_gpus_no_storage() {
        let services = detect_services(&[], 50, &[]);
        // Only data_bridge
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].kind, "data_bridge");
    }

    #[test]
    fn detect_services_with_gpus_and_storage() {
        let gpus = vec![GpuInfo {
            gpu_type: "A100".into(),
            count: 2,
            vram_gb: 80,
        }];
        let services = detect_services(&gpus, 200, &[]);
        assert!(services.iter().any(|s| s.kind == "compute"));
        assert!(services.iter().any(|s| s.kind == "storage"));
        assert!(services.iter().any(|s| s.kind == "data_bridge"));
    }

    #[test]
    fn detect_services_with_models() {
        let models = vec![ModelInfo {
            name: "predictor".into(),
            path: "/models/predictor.pt".into(),
            format: Some("pytorch".into()),
            size_gb: Some(0.5),
        }];
        let services = detect_services(&[], 50, &models);
        assert!(services
            .iter()
            .any(|s| s.kind == "inference" && s.model.as_deref() == Some("predictor")));
    }

    #[test]
    fn probe_has_os_and_arch_labels() {
        let caps = probe_local_capabilities();
        assert!(caps.labels.contains_key("os"));
        assert!(caps.labels.contains_key("arch"));
    }

    #[test]
    fn probe_default_visibility_is_private() {
        let caps = probe_local_capabilities();
        assert_eq!(caps.visibility, "private");
    }

    #[test]
    fn probe_no_public_key_by_default() {
        let caps = probe_local_capabilities();
        assert!(caps.public_key.is_none());
    }
}
