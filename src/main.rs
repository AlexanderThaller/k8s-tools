use std::collections::BTreeSet;

use clap::{
    Parser,
    Subcommand,
};
use eyre::Result;
use k8s_openapi::{
    api::core::v1::Pod,
    apimachinery::pkg::api::resource::Quantity,
};
use kube::{
    api::ListParams,
    Api,
    Client,
};
use serde::Serialize;

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

    // Get the resource requests for pods in the current namespace.
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
    },
}

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("failed to create kubernetes client: {0}")]
    CreateClient(kube::Error),

    #[error("failed to list pods: {0}")]
    ListPods(kube::Error),
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
        } => resource_requests(namespaces, all_namespaces).await,
    }
}

async fn get_pods(namespaces: Vec<String>, all_namespaces: bool) -> Result<Vec<Pod>> {
    let client = Client::try_default()
        .await
        .map_err(ApiError::CreateClient)?;

    let apis = if all_namespaces {
        vec![Api::all(client)]
    } else if namespaces.is_empty() {
        vec![Api::default_namespaced(client)]
    } else {
        namespaces
            .iter()
            .map(|namespace| Api::namespaced(client.clone(), namespace))
            .collect()
    };

    let lp = ListParams::default();
    let mut pods = Vec::new();

    for api in apis {
        pods.extend(api.list(&lp).await.map_err(ApiError::ListPods)?);
    }

    Ok(pods)
}

async fn missing_health_probes(namespaces: Vec<String>, all_namespaces: bool) -> Result<()> {
    #[derive(Debug, Serialize)]
    struct Output {
        pod_name: String,
        container_name: String,
        liveness_probe: Option<String>,
        readiness_probe: Option<String>,
    }

    let pods = get_pods(namespaces, all_namespaces).await?;

    let pods: Vec<_> = pods
        .iter()
        .filter(|pod| pod.status.is_some())
        .filter(|pod| pod.status.as_ref().unwrap().phase == Some("Running".to_string()))
        .filter(|pod| pod.spec.is_some())
        .filter(|pod| {
            !pod.spec
                .as_ref()
                .unwrap()
                .containers
                .iter()
                .any(|container| {
                    container.liveness_probe.is_some() || container.readiness_probe.is_some()
                })
        })
        .flat_map(|pod| {
            pod.spec
                .as_ref()
                .unwrap()
                .containers
                .iter()
                .map(|container| {
                    (
                        container.name.clone(),
                        container
                            .liveness_probe
                            .as_ref()
                            .map(|probe| format!("{:?}", probe)),
                        container
                            .readiness_probe
                            .as_ref()
                            .map(|probe| format!("{:?}", probe)),
                    )
                })
                .map(|(container_name, liveness_probe, readiness_probe)| Output {
                    pod_name: pod.metadata.name.as_ref().unwrap().clone(),
                    container_name,
                    liveness_probe,
                    readiness_probe,
                })
        })
        .collect();

    let out = serde_json::to_string_pretty(&pods)?;

    println!("{out}");

    Ok(())
}

async fn resource_requests(namespaces: Vec<String>, all_namespaces: bool) -> Result<()> {
    #[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq)]
    struct Output {
        requests_cpu: u64,
        pod_name: String,
        container_name: String,
    }

    let pods = get_pods(namespaces, all_namespaces).await?;

    let output = pods
        .into_iter()
        .flat_map(|pod| {
            pod.spec
                .expect("missing spec")
                .containers
                .into_iter()
                .filter(|container| container.resources.is_some())
                .filter(|container| container.resources.as_ref().unwrap().requests.is_some())
                .map(move |container| Output {
                    pod_name: pod
                        .metadata
                        .name
                        .as_ref()
                        .expect("missing pod name")
                        .clone(),

                    container_name: container.name,

                    requests_cpu: quantity_to_number(
                        container
                            .resources
                            .expect("missing resources")
                            .requests
                            .expect("missing requests")
                            .remove("cpu")
                            .expect("missing cpu"),
                    ),
                })
        })
        .collect::<BTreeSet<Output>>();

    let out = serde_json::to_string_pretty(&output)?;

    println!("{out}");

    Ok(())
}

fn quantity_to_number(input: Quantity) -> u64 {
    let mut number = String::new();
    let mut suffix: Option<char> = None;

    for ch in input.0.chars() {
        if ch.is_numeric() {
            number.push(ch)
        } else {
            suffix = Some(ch)
        }
    }

    let number = number.parse().expect("failed to parse number");

    if let Some(s) = suffix {
        match s {
            'm' => number,
            'k' => number * 1000 * 1000,

            _ => panic!("invalid suffix {s}"),
        }
    } else {
        number * 1000
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::api::resource::Quantity;

    #[test]
    fn quantity_to_number() {
        let input: Quantity = Quantity("1500m".to_string());
        dbg!(&input);

        let expected = 1500;
        let output = super::quantity_to_number(input);

        assert_eq!(expected, output);
    }
}
