//#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![warn(rust_2018_idioms, unused_lifetimes, missing_debug_implementations)]
#![warn(clippy::dbg_macro)]
#![warn(clippy::panic)]
#![warn(clippy::pedantic)]
#![warn(clippy::unwrap_used)]

use clap::{Parser, Subcommand};
use commands::{
    missing_health_probes::missing_health_probes,
    readonly_root_filesystem::readonly_root_filesystem, resource_requests::resource_requests,
};
use eyre::Result;
use log::LevelFilter;

mod api;
mod commands;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The log level to run under.
    #[arg(long, env, default_value = "info")]
    pub log_level: LevelFilter,

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

        /// Disable checking for higher cpu usage than the request.
        #[arg(name = "no-check-higher", long, required = false)]
        no_check_higher: bool,
    },

    /// Check if pods are running with a read-only root filesystem.
    ReadOnlyRootFilesystem {
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    std::env::set_var("RUST_LOG", args.log_level.as_str());
    pretty_env_logger::try_init_timed()?;

    match args.command {
        Command::MissingHealthProbes {
            namespaces,
            all_namespaces,
        } => missing_health_probes(namespaces, all_namespaces).await,

        Command::ResourceRequests {
            namespaces,
            all_namespaces,
            threshold,
            no_check_higher,
        } => resource_requests(namespaces, all_namespaces, threshold, no_check_higher).await,

        Command::ReadOnlyRootFilesystem {
            namespaces,
            all_namespaces,
        } => readonly_root_filesystem(namespaces, all_namespaces).await,
    }
}
