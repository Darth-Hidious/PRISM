use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use prism_proto::{GpuInfo, NodeCapabilities};
use prism_runtime::PlatformEndpoints;
use sysinfo::{Disks, System};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "prism-node")]
#[command(about = "Rust runtime for turning a PRISM-installed machine into a MARC27 compute node")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Probe local capabilities without connecting to the platform.
    Probe,
    /// Print the initial runtime config for a future long-lived node daemon.
    Run {
        #[arg(long, env = "PRISM_NODE_NAME", default_value = "prism-node")]
        name: String,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let endpoints = PlatformEndpoints::from_env();

    match cli.command {
        Command::Probe => {
            let capabilities = probe_local_capabilities();
            println!("{}", serde_json::to_string_pretty(&capabilities)?);
        }
        Command::Run { name } => {
            let capabilities = probe_local_capabilities();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "name": name,
                    "connect": endpoints.node_ws,
                    "capabilities": capabilities,
                    "status": "scaffolded",
                    "next_step": "implement websocket registration, heartbeat loop, and executor adapters",
                }))?
            );
        }
    }

    Ok(())
}

fn probe_local_capabilities() -> NodeCapabilities {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    let mut system = System::new_all();
    system.refresh_all();

    let ram_gb = (system.total_memory() / 1024 / 1024).max(1);

    let disks = Disks::new_with_refreshed_list();
    let disk_bytes: u64 = disks.list().iter().map(|d| d.total_space()).sum();
    let disk_gb = (disk_bytes / 1024 / 1024 / 1024).max(1);

    let labels = BTreeMap::from([
        ("os".to_string(), env::consts::OS.to_string()),
        ("arch".to_string(), env::consts::ARCH.to_string()),
    ]);

    NodeCapabilities {
        gpus: detect_gpus(),
        cpu_cores,
        ram_gb,
        disk_gb,
        software: detect_software(),
        container_runtime: detect_container_runtime(),
        scheduler: detect_scheduler(),
        labels,
    }
}

fn detect_gpus() -> Vec<GpuInfo> {
    let raw = env::var("PRISM_NODE_GPU_JSON").ok();
    raw.and_then(|value| serde_json::from_str::<Vec<GpuInfo>>(&value).ok())
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

fn binary_exists(binary: &str) -> bool {
    let path_entries: Vec<_> = env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).collect())
        .unwrap_or_default();

    path_entries
        .into_iter()
        .any(|dir| path_contains_binary(&dir, binary))
}

fn path_contains_binary(dir: &Path, binary: &str) -> bool {
    let plain = dir.join(binary);
    if plain.is_file() {
        return true;
    }

    if cfg!(windows) {
        for ext in ["exe", "cmd", "bat"] {
            let with_ext = dir.join(format!("{binary}.{ext}"));
            if with_ext.is_file() {
                return true;
            }
        }
    }

    false
}

#[allow(dead_code)]
fn _ensure_pathbuf(_path: PathBuf) {}
