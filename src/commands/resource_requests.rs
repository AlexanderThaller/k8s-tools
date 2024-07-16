use std::collections::{BTreeMap, BTreeSet, HashMap};

use eyre::{Context, Result};
use k8s_openapi::api::core::v1::{Container, Pod};
use log::{info, warn};
use serde::Serialize;

use crate::api::{self, get_pod_owner, get_pod_resource_usage, get_pods, Cpu, Memory, Owner};

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default)]
struct Total {
    namespaces: Vec<TotalNamespace>,
    owners: Vec<TotalOwner>,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default)]
struct Output {
    total: Total,
    pods: BTreeSet<PodOutput>,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct Resources {
    usage: ResourcePair,
    requests: ResourcePair,
    limits: ResourcePair,
    difference: UsageDifference,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct UsageDifference {
    requests: ResourcePair,
    limits: ResourcePair,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct ResourcePair {
    cpu: Option<Cpu>,
    cpu_milliseconds: Option<u64>,
    memory: Option<Memory>,
    memory_bytes: Option<u64>,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct TotalNamespace {
    namespace: String,
    resources: Resources,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct TotalOwner {
    owner: Owner,
    resources: Resources,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq)]
struct PodOutput {
    pod_name: String,
    container_name: String,
    namespace: String,
    owner: Option<Owner>,

    resources: Resources,
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn resource_requests(
    namespaces: Vec<String>,
    all_namespaces: bool,
    threshold: Option<u64>,
    no_check_higher: bool,
) -> Result<()> {
    let pods = get_pods(namespaces, all_namespaces).await?;

    let output = pods
        .into_iter()
        .filter(|pod| pod.status.is_some())
        .filter(|pod| {
            let is_running =
                pod.status.as_ref().map(|status| status.phase.as_deref()) == Some(Some("Running"));

            if !is_running {
                if let Some(name) = &pod.metadata.name {
                    info!("Ignoring not running pod: {name}");
                }
            }

            is_running
        })
        .flat_map(pod_to_output)
        .flatten()
        .collect::<BTreeSet<PodOutput>>();

    let mut tops = BTreeMap::new();
    for pod in &output {
        let top = get_pod_resource_usage(&pod.namespace, &pod.pod_name)
            .await
            .with_context(|| "failed to get pod resource usage")?;

        tops.insert(pod.pod_name.clone(), top);
    }

    let pods = output
        .into_iter()
        .map(|pod| {
            let usage = tops.get(&pod.pod_name).expect("failed to get usage");

            if let Some(usage) = usage {
                let container_usage = usage
                    .containers
                    .iter()
                    .find(|container| container.name == pod.container_name);

                let cpu_usage = container_usage.map(|container| {
                    (&container.usage.cpu)
                        .try_into()
                        .expect("failed to convert cpu usage")
                });

                let memory_usage = container_usage.map(|container| {
                    (&container.usage.memory)
                        .try_into()
                        .expect("failed to convert memory usage")
                });

                let resources = pod
                    .resources
                    .set_cpu_usage(cpu_usage)
                    .set_memory_usage(memory_usage);

                PodOutput { resources, ..pod }
            } else {
                warn!("Failed to get usage for pod: {}", pod.pod_name);
                pod
            }
        })
        .filter(|pod| {
            // Check if cpu_usage is higher than requests_cpu
            if let Some(cpu_usage) = pod.resources.usage.cpu {
                if let Some(requests_cpu) = pod.resources.requests.cpu {
                    if cpu_usage > requests_cpu {
                        return true;
                    }
                }
            }

            // Check if cpu_usage is below the requests_cpu threshold
            if !no_check_higher {
                if let Some(threshold) = threshold {
                    if let Some(cpu_usage) = pod.resources.usage.cpu {
                        if let Some(requests_cpu) = pod.resources.requests.cpu {
                            let diff = requests_cpu.saturating_sub(cpu_usage);

                            return diff > threshold.into();
                        }
                    }
                }
            };

            true
        })
        .collect::<BTreeSet<_>>();

    let total_namespaces: HashMap<&str, TotalNamespace> =
        pods.iter().fold(HashMap::default(), |mut total, pod| {
            let entry = total
                .entry(&pod.namespace)
                .or_insert_with(|| TotalNamespace {
                    namespace: pod.namespace.clone(),
                    ..Default::default()
                });

            *entry += pod;
            total
        });

    let total_owners: HashMap<&str, TotalOwner> =
        pods.iter().fold(HashMap::default(), |mut total, pod| {
            if let Some(owner) = &pod.owner {
                let entry = total.entry(&pod.namespace).or_insert_with(|| TotalOwner {
                    owner: owner.clone(),
                    ..Default::default()
                });

                *entry += pod;
            }

            total
        });

    let output = Output {
        total: Total {
            namespaces: total_namespaces.values().cloned().collect(),
            owners: total_owners.values().cloned().collect(),
        },

        pods,
    };

    let out = serde_json::to_string_pretty(&output)?;

    println!("{out}");

    Ok(())
}

impl std::ops::AddAssign<&PodOutput> for TotalNamespace {
    fn add_assign(&mut self, rhs: &PodOutput) {
        let new = &self.resources + &rhs.resources;
        self.resources = new;
    }
}

impl std::ops::AddAssign<&PodOutput> for TotalOwner {
    fn add_assign(&mut self, rhs: &PodOutput) {
        let new = &self.resources + &rhs.resources;
        self.resources = new;
    }
}

fn pod_to_output(pod: Pod) -> Result<Vec<PodOutput>> {
    let owner = get_pod_owner(&pod);

    let metadata = pod.metadata;
    let name = metadata.name.expect("missing pod name");
    let namespace = metadata.namespace.expect("missing pod name");
    let spec = pod.spec.expect("missing pod spec");
    let containers = spec.containers;

    containers
        .into_iter()
        .filter(|container| container.resources.is_some())
        .map(move |container| {
            generate_pod_output(name.clone(), namespace.clone(), owner.clone(), container)
        })
        .collect()
}

fn generate_pod_output(
    pod_name: String,
    namespace: String,
    owner: Option<Owner>,
    container: Container,
) -> Result<PodOutput> {
    let requests_cpu = container
        .resources
        .as_ref()
        .expect("missing resources")
        .requests
        .as_ref()
        .and_then(|requests| requests.get("cpu"))
        .map(Cpu::try_from)
        .transpose()
        .context("failed to convert cpu requests")?;

    let requests_cpu_milliseconds = requests_cpu.map(api::Cpu::to_milliseconds);

    let requests_memory = container
        .resources
        .as_ref()
        .expect("missing resources")
        .requests
        .as_ref()
        .and_then(|requests| requests.get("memory"))
        .map(Memory::try_from)
        .transpose()
        .context("failed to convert memory requests")?;

    let requests_memory_bytes = requests_memory.map(api::Memory::to_bytes);

    let limits_cpu = container
        .resources
        .as_ref()
        .expect("missing resources")
        .limits
        .as_ref()
        .and_then(|limits| limits.get("cpu"))
        .map(Cpu::try_from)
        .transpose()
        .context("failed to convert cpu limits")?;

    let limits_cpu_milliseconds = limits_cpu.map(api::Cpu::to_milliseconds);

    let limits_memory = container
        .resources
        .expect("missing resources")
        .limits
        .as_ref()
        .and_then(|limits| limits.get("memory"))
        .map(Memory::try_from)
        .transpose()
        .context("failed to convert memory limits")?;

    let limits_memory_bytes = limits_memory.map(api::Memory::to_bytes);

    Ok(PodOutput {
        namespace,
        pod_name,
        container_name: container.name,
        owner,

        resources: Resources {
            limits: ResourcePair {
                cpu: limits_cpu,
                cpu_milliseconds: limits_cpu_milliseconds,
                memory: limits_memory,
                memory_bytes: limits_memory_bytes,
            },

            requests: ResourcePair {
                cpu: requests_cpu,
                cpu_milliseconds: requests_cpu_milliseconds,
                memory: requests_memory,
                memory_bytes: requests_memory_bytes,
            },

            usage: ResourcePair {
                cpu: None,
                cpu_milliseconds: None,
                memory: None,
                memory_bytes: None,
            },

            difference: UsageDifference {
                requests: ResourcePair {
                    cpu: None,
                    cpu_milliseconds: None,
                    memory: None,
                    memory_bytes: None,
                },

                limits: ResourcePair {
                    cpu: None,
                    cpu_milliseconds: None,
                    memory: None,
                    memory_bytes: None,
                },
            },
        },
    })
}

impl std::ops::Add<&Resources> for &Resources {
    type Output = Resources;

    fn add(self, rhs: &Resources) -> Self::Output {
        Resources {
            usage: &self.usage + &rhs.usage,
            requests: &self.requests + &rhs.requests,
            limits: &self.limits + &rhs.limits,
            difference: &self.difference + &rhs.difference,
        }
    }
}

impl Resources {
    fn set_cpu_usage(mut self, cpu_usage: Option<Cpu>) -> Self {
        self.usage.cpu_milliseconds = cpu_usage.map(api::Cpu::to_milliseconds);
        self.usage.cpu = cpu_usage;

        self.difference.requests = &self.requests - &self.usage;
        self.difference.limits = &self.limits - &self.usage;

        self
    }

    fn set_memory_usage(mut self, memory_usage: Option<Memory>) -> Self {
        self.usage.memory_bytes = memory_usage.map(api::Memory::to_bytes);
        self.usage.memory = memory_usage;

        self.difference.requests = &self.requests - &self.usage;
        self.difference.limits = &self.limits - &self.usage;

        self
    }
}

impl std::ops::Add<&ResourcePair> for &ResourcePair {
    type Output = ResourcePair;

    fn add(self, rhs: &ResourcePair) -> Self::Output {
        ResourcePair {
            cpu: self.cpu.zip(rhs.cpu).map(|(left, right)| left + right),

            cpu_milliseconds: self
                .cpu_milliseconds
                .zip(rhs.cpu_milliseconds)
                .map(|(left, right)| left + right),

            memory: self
                .memory
                .zip(rhs.memory)
                .map(|(left, right)| left + right),

            memory_bytes: self
                .memory_bytes
                .zip(rhs.memory_bytes)
                .map(|(left, right)| left + right),
        }
    }
}

impl std::ops::Sub<&ResourcePair> for &ResourcePair {
    type Output = ResourcePair;

    fn sub(self, rhs: &ResourcePair) -> Self::Output {
        ResourcePair {
            cpu: self
                .cpu
                .zip(rhs.cpu)
                .map(|(left, right)| left.saturating_sub(right)),

            cpu_milliseconds: self
                .cpu_milliseconds
                .zip(rhs.cpu_milliseconds)
                .map(|(left, right)| left.saturating_sub(right)),

            memory: self
                .memory
                .zip(rhs.memory)
                .map(|(left, right)| left.saturating_sub(right)),

            memory_bytes: self
                .memory_bytes
                .zip(rhs.memory_bytes)
                .map(|(left, right)| left.saturating_sub(right)),
        }
    }
}

impl std::ops::Add<&UsageDifference> for &UsageDifference {
    type Output = UsageDifference;

    fn add(self, rhs: &UsageDifference) -> Self::Output {
        UsageDifference {
            requests: &self.requests + &rhs.requests,
            limits: &self.limits + &rhs.limits,
        }
    }
}
