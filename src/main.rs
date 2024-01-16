use std::collections::{
    BTreeMap,
    BTreeSet,
};

use bytesize::ByteSize;
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
    core::ObjectMeta,
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

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("failed to create kubernetes client: {0}")]
    CreateClient(kube::Error),

    #[error("failed to list pods: {0}")]
    ListPods(kube::Error),
}

#[derive(Debug, Ord, PartialOrd, PartialEq, Eq)]
struct TopResult {
    pod_name: String,
    container_name: String,
    cpu: u64,
    memory: u64,
}

#[derive(Debug, Ord, PartialOrd, PartialEq, Eq, Clone, Copy)]
struct Memory(u64);

#[derive(Debug, Ord, PartialOrd, PartialEq, Eq, Clone, Copy)]
struct Cpu(u64);

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

async fn resource_requests(
    namespaces: Vec<String>,
    all_namespaces: bool,
    threshold: Option<u64>,
) -> Result<()> {
    #[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq)]
    struct Output {
        requests_cpu: Option<Cpu>,
        cpu_usage: Option<Cpu>,
        limits_cpu: Option<Cpu>,
        requests_memory: Option<Memory>,
        limits_memory: Option<Memory>,
        memory_usage: Option<Memory>,
        pod_name: String,
        container_name: String,
        namespace: String,
    }

    let pods = get_pods(namespaces, all_namespaces).await?;

    let output = pods
        .into_iter()
        .filter(|pod| pod.status.is_some())
        .filter(|pod| pod.status.as_ref().unwrap().phase == Some("Running".to_string()))
        .flat_map(|pod| {
            pod.spec
                .expect("missing spec")
                .containers
                .into_iter()
                .filter(|container| container.resources.is_some())
                .map(move |container| Output {
                    pod_name: pod
                        .metadata
                        .name
                        .as_ref()
                        .expect("missing pod name")
                        .clone(),

                    container_name: container.name,
                    namespace: pod
                        .metadata
                        .namespace
                        .as_ref()
                        .expect("missing namespace")
                        .clone(),

                    requests_cpu: container
                        .resources
                        .as_ref()
                        .expect("missing resources")
                        .requests
                        .as_ref()
                        .and_then(|requests| requests.get("cpu"))
                        .map(quantity_to_number)
                        .map(Cpu),

                    requests_memory: container
                        .resources
                        .as_ref()
                        .expect("missing resources")
                        .requests
                        .as_ref()
                        .and_then(|requests| requests.get("memory"))
                        .map(quantity_to_number)
                        .map(Memory),

                    limits_cpu: container
                        .resources
                        .as_ref()
                        .expect("missing resources")
                        .limits
                        .as_ref()
                        .and_then(|limits| limits.get("cpu"))
                        .map(quantity_to_number)
                        .map(Cpu),

                    limits_memory: container
                        .resources
                        .expect("missing resources")
                        .limits
                        .as_ref()
                        .and_then(|limits| limits.get("memory"))
                        .map(quantity_to_number)
                        .map(Memory),

                    cpu_usage: None,
                    memory_usage: None,
                })
        })
        .collect::<BTreeSet<Output>>();

    let mut tops = BTreeMap::new();
    for pod in &output {
        let top = get_pod_resource_usage(&pod.namespace, &pod.pod_name)
            .await
            .unwrap();
        tops.insert(pod.pod_name.clone(), top);
    }

    let output = output
        .into_iter()
        .map(|pod| {
            let usage = tops.get(&pod.pod_name).unwrap();
            let container_usage = usage
                .containers
                .iter()
                .find(|container| container.name == pod.container_name);

            Output {
                cpu_usage: container_usage
                    .map(|container| Cpu(quantity_to_number(&container.usage.cpu))),
                memory_usage: container_usage
                    .map(|container| Memory(quantity_to_number(&container.usage.memory))),
                ..pod
            }
        })
        .filter(|pod| {
            // Check if cpu_usage is higher than requests_cpu
            if let Some(cpu_usage) = pod.cpu_usage {
                if let Some(requests_cpu) = pod.requests_cpu {
                    if cpu_usage.0 > requests_cpu.0 {
                        return true;
                    }
                }
            }

            // Check if cpu_usage is below the requests_cpu threshold
            if let Some(threshold) = threshold {
                if let Some(cpu_usage) = pod.cpu_usage {
                    if let Some(requests_cpu) = pod.requests_cpu {
                        let diff = requests_cpu.0.saturating_sub(cpu_usage.0);

                        return diff > threshold;
                    }
                }
            };

            true
        })
        .collect::<BTreeSet<_>>();

    let out = serde_json::to_string_pretty(&output)?;

    println!("{out}");

    Ok(())
}

fn quantity_to_number(input: &Quantity) -> u64 {
    let mut number = String::new();
    let mut suffix = String::new();

    // accumulate number until char is not a number then use the rest as suffix
    // to make the memory stuff (MiB, GiB) work

    let number_acc = true;

    for ch in input.0.chars() {
        if number_acc {
            if ch.is_numeric() {
                number.push(ch);
            } else {
                suffix.push(ch);
            }
        } else {
            suffix.push(ch);
        }
    }

    let number = number.parse().expect("failed to parse number");

    if suffix.is_empty() {
        number * 1000
    } else {
        match suffix.as_str() {
            "n" => number / 1000 / 1000,
            "m" => number,
            "k" => number * 1000 * 1000,
            "Ki" => number * 1024,
            "Mi" => number * 1024 * 1024,
            "Gi" => number * 1024 * 1024 * 1024,

            _ => {
                dbg!(input);
                panic!("invalid suffix {suffix}");
            }
        }
    }
}

async fn get_pod_resource_usage(namespace: &str, pod: &str) -> Result<PodMetrics> {
    let client = Client::try_default()
        .await
        .map_err(ApiError::CreateClient)?;

    let api: Api<PodMetrics> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().fields(&format!("metadata.name={}", pod));

    let mut out = api.list(&lp).await.unwrap().items;

    if out.len() != 1 {
        panic!("expected 1 pod got {}", out.len());
    }

    Ok(out.remove(0))
}

impl Serialize for Cpu {
    fn serialize<S>(&self, serializer: S) -> std::prelude::v1::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("{}m", self.0))
    }
}

impl Serialize for Memory {
    fn serialize<S>(&self, serializer: S) -> std::prelude::v1::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&ByteSize(self.0).to_string_as(true))
    }
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct PodMetricsContainer {
    pub name: String,
    pub usage: PodMetricsContainerUsage,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct PodMetricsContainerUsage {
    pub cpu: Quantity,
    pub memory: Quantity,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct PodMetrics {
    pub metadata: ObjectMeta,
    pub timestamp: String,
    pub window: String,
    pub containers: Vec<PodMetricsContainer>,
}

impl k8s_openapi::Resource for PodMetrics {
    const GROUP: &'static str = "metrics.k8s.io";
    const KIND: &'static str = "PodMetrics";
    const VERSION: &'static str = "v1beta1";
    const API_VERSION: &'static str = "metrics.k8s.io/v1beta1";
    const URL_PATH_SEGMENT: &'static str = "pods";

    type Scope = k8s_openapi::NamespaceResourceScope;
}

impl k8s_openapi::Metadata for PodMetrics {
    type Ty = ObjectMeta;

    fn metadata(&self) -> &Self::Ty {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut Self::Ty {
        &mut self.metadata
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::api::resource::Quantity;

    #[test]
    fn quantity_to_number() {
        let testcases = vec![
            ("1500m", 1500),
            ("1k", 1_000_000),
            ("1", 1000),
            ("1Ki", 1024),
        ];

        for (input, expected) in testcases {
            let input: Quantity = Quantity(input.to_string());

            dbg!(&input);
            dbg!(&expected);

            let output = super::quantity_to_number(&input);
            assert_eq!(expected, output);
        }
    }
}
