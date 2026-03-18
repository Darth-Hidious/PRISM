use anyhow::Result;
use clap::{Parser, Subcommand};
use prism_node::daemon::DaemonOptions;
use prism_runtime::PlatformEndpoints;
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
    /// Stop a running node daemon.
    Down,
    /// Start the node daemon — connect to platform, register, and wait for jobs.
    Up {
        #[arg(long, env = "PRISM_NODE_NAME")]
        name: Option<String>,
        #[arg(long, default_value = "private")]
        visibility: String,
        #[arg(long)]
        price: Option<f64>,
        #[arg(long, value_delimiter = ',')]
        data_paths: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        model_paths: Vec<String>,
        #[arg(long)]
        no_compute: bool,
        #[arg(long)]
        no_storage: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let endpoints = PlatformEndpoints::from_env();

    match cli.command {
        Command::Down => {
            let paths = prism_runtime::PrismPaths::discover()?;
            prism_node::daemon::stop_daemon(&paths)?;
        }
        Command::Probe => {
            let capabilities = prism_node::detect::probe_local_capabilities_async().await;
            println!("{}", serde_json::to_string_pretty(&capabilities)?);
        }
        Command::Up {
            name,
            visibility,
            price,
            data_paths,
            model_paths,
            no_compute,
            no_storage,
        } => {
            // Inject extra paths into env before probing
            if !data_paths.is_empty() {
                let existing = std::env::var("PRISM_DATA_PATHS").unwrap_or_default();
                let combined = if existing.is_empty() {
                    data_paths.join(",")
                } else {
                    format!("{},{}", existing, data_paths.join(","))
                };
                std::env::set_var("PRISM_DATA_PATHS", combined);
            }
            if !model_paths.is_empty() {
                let existing = std::env::var("PRISM_MODEL_PATHS").unwrap_or_default();
                let combined = if existing.is_empty() {
                    model_paths.join(",")
                } else {
                    format!("{},{}", existing, model_paths.join(","))
                };
                std::env::set_var("PRISM_MODEL_PATHS", combined);
            }

            let paths = prism_runtime::PrismPaths::discover()?;
            let options = DaemonOptions {
                name: name.unwrap_or_else(|| {
                    sysinfo::System::host_name().unwrap_or_else(|| "prism-node".to_string())
                }),
                visibility,
                price_per_hour_usd: price,
                no_compute,
                no_storage,
            };

            prism_node::daemon::run_daemon(&endpoints, &paths, options).await?;
        }
    }

    Ok(())
}
