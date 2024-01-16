use clap::{
    Parser,
    Subcommand,
};
use commands::{
    missing_health_probes::missing_health_probes,
    resource_requests::resource_requests,
};
use eyre::Result;

mod api;
mod commands;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Get pods in the current namespace that have missing health (liveness,
    /// readiness) probes.
    MissingHealthProbes {
        /// Check the given namespaces if not defined the current one will be
        /// used.
        #[arg(
            name = "namespaces",
            long,
            required = false,
            conflicts_with = "all-namespaces"
        )]
        namespaces: Vec<String>,

        /// Check all namespaces.
        #[arg(
            name = "all-namespaces",
            long,
            required = false,
            conflicts_with = "namespaces"
        )]
        all_namespaces: bool,
    },

    /// Get the resource requests for pods in the current namespace.
    ResourceRequests {
        /// Check the given namespaces if not defined the current one will be
        /// used.
        #[arg(
            name = "namespaces",
            long,
            required = false,
            conflicts_with = "all-namespaces"
        )]
        namespaces: Vec<String>,

        /// Check all namespaces.
        #[arg(
            name = "all-namespaces",
            long,
            required = false,
            conflicts_with = "namespaces"
        )]
        all_namespaces: bool,

        /// Threshold for displaying containers. Will calculate the difference
        /// between the request and the current cpu usage if thats
        /// bigger than the threshold the container will be displayed.
        /// When not specified will print all pods.
        #[arg(name = "threshold", long, required = false)]
        threshold: Option<u64>,
    },
}

#[derive(Debug, Ord, PartialOrd, PartialEq, Eq)]
struct TopResult {
    pod_name: String,
    container_name: String,
    cpu: u64,
    memory: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::MissingHealthProbes {
            namespaces,
            all_namespaces,
        } => missing_health_probes(namespaces, all_namespaces).await,
        Command::ResourceRequests {
            namespaces,
            all_namespaces,
            threshold,
        } => resource_requests(namespaces, all_namespaces, threshold).await,
    }
}
