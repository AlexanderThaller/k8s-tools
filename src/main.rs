use std::collections::{
    BTreeMap,
    BTreeSet,
};

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
use numfmt::{
    Formatter,
    Precision,
    Scales,
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
        let top = get_pod_resource_usage(&pod.pod_name).await.unwrap();
        tops.insert(pod.pod_name.clone(), top);
    }

    let output = output
        .into_iter()
        .map(|pod| {
            let top = tops.get(&pod.pod_name).unwrap();
            let container_top = top
                .iter()
                .find(|top| top.container_name == pod.container_name);

            Output {
                cpu_usage: container_top.map(|top| Cpu(top.cpu)),
                memory_usage: container_top.map(|top| Memory(top.memory)),
                ..pod
            }
        })
        .filter(|pod| {
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
            "m" => number,
            "k" => number * 1000 * 1000,
            "Ki" => number * 1024,
            "Mi" => number * 1024 * 1024,
            "Gi" => number * 1024 * 1024 * 1024,

            _ => panic!("invalid suffix {suffix}"),
        }
    }
}

async fn get_pod_resource_usage(pod: &str) -> Result<Vec<TopResult>> {
    // ⬢ [podman] ❯ kubectl top pod logstash-ls-1 --containers
    // POD             NAME          CPU(cores)   MEMORY(bytes)
    // logstash-ls-1   POD           0m           0Mi
    // logstash-ls-1   istio-proxy   7m           94Mi
    // logstash-ls-1   logstash      158m         1746Mi
    //

    let output = tokio::process::Command::new("kubectl")
        .args(["top", "pods", "--containers", pod])
        .output()
        .await
        .unwrap()
        .stdout;

    let output = String::from_utf8_lossy(&output);

    let out = output
        .lines()
        .skip(1)
        .map(|line| {
            let split = line.split_whitespace().collect::<Vec<_>>();
            let mut split = split.into_iter();

            let pod_name = split.next().expect("missing pod_name").to_string();
            let container_name = split.next().expect("missing container_name").to_string();
            let cpu = quantity_to_number(&Quantity(split.next().expect("missing cpu").into()));
            let memory =
                quantity_to_number(&Quantity(split.next().expect("missing memory").into()));

            TopResult {
                pod_name,
                container_name,
                cpu,
                memory,
            }
        })
        .collect();

    Ok(out)
}

impl Serialize for Cpu {
    fn serialize<S>(&self, serializer: S) -> std::prelude::v1::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut f = Formatter::new()
            .precision(Precision::Significance(2))
            .suffix("m")
            .unwrap();

        serializer.serialize_str(f.fmt2(self.0))
    }
}

impl Serialize for Memory {
    fn serialize<S>(&self, serializer: S) -> std::prelude::v1::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut f = Formatter::new()
            .scales(Scales::binary())
            .precision(Precision::Significance(2))
            .suffix("B")
            .unwrap();

        serializer.serialize_str(f.fmt2(self.0))
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
