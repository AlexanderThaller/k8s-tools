use std::collections::{BTreeMap, BTreeSet, HashMap};

use eyre::Result;
use k8s_openapi::api::core::v1::{Container, Pod};
use log::warn;
use serde::Serialize;

use crate::api::{self, get_pod_owner, get_pod_resource_usage, get_pods, Cpu, Memory, Owner};

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default)]
struct Total {
    namespaces: Vec<TotalNamespace>,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default)]
struct Output {
    total: Total,
    pods: BTreeSet<PodOutput>,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct Resources {
    cpu_usage: Option<Cpu>,
    cpu_usage_milliseconds: Option<u64>,

    memory_usage: Option<Memory>,
    memory_usage_bytes: Option<u64>,

    requests_cpu: Option<Cpu>,
    requests_cpu_milliseconds: Option<u64>,
    requests_memory: Option<Memory>,
    requests_memory_bytes: Option<u64>,

    limits_cpu: Option<Cpu>,
    limits_cpu_milliseconds: Option<u64>,
    limits_memory: Option<Memory>,
    limits_memory_bytes: Option<u64>,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct TotalNamespace {
    namespace: String,
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
        .filter(|pod| pod.status.as_ref().unwrap().phase == Some("Running".to_string()))
        .flat_map(pod_to_output)
        .collect::<BTreeSet<PodOutput>>();

    let mut tops = BTreeMap::new();
    for pod in &output {
        let top = get_pod_resource_usage(&pod.namespace, &pod.pod_name)
            .await
            .unwrap();
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

                let cpu_usage = container_usage.map(|container| (&container.usage.cpu).into());
                let memory_usage =
                    container_usage.map(|container| (&container.usage.memory).into());

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
            if let Some(cpu_usage) = pod.resources.cpu_usage {
                if let Some(requests_cpu) = pod.resources.requests_cpu {
                    if cpu_usage > requests_cpu {
                        return true;
                    }
                }
            }

            // Check if cpu_usage is below the requests_cpu threshold
            if !no_check_higher {
                if let Some(threshold) = threshold {
                    if let Some(cpu_usage) = pod.resources.cpu_usage {
                        if let Some(requests_cpu) = pod.resources.requests_cpu {
                            let diff = requests_cpu.saturating_sub(cpu_usage);

                            return diff > threshold.into();
                        }
                    }
                }
            };

            true
        })
        .collect::<BTreeSet<_>>();

    let total: HashMap<&str, TotalNamespace> =
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

    let output = Output {
        total: Total {
            namespaces: total.values().cloned().collect(),
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

fn pod_to_output(pod: Pod) -> impl Iterator<Item = PodOutput> {
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
}

fn generate_pod_output(
    pod_name: String,
    namespace: String,
    owner: Option<Owner>,
    container: Container,
) -> PodOutput {
    let requests_cpu = container
        .resources
        .as_ref()
        .expect("missing resources")
        .requests
        .as_ref()
        .and_then(|requests| requests.get("cpu"))
        .map(Cpu::from);

    let requests_cpu_milliseconds = requests_cpu.map(api::Cpu::to_milliseconds);

    let requests_memory = container
        .resources
        .as_ref()
        .expect("missing resources")
        .requests
        .as_ref()
        .and_then(|requests| requests.get("memory"))
        .map(Memory::from);

    let requests_memory_bytes = requests_memory.map(api::Memory::to_bytes);

    let limits_cpu = container
        .resources
        .as_ref()
        .expect("missing resources")
        .limits
        .as_ref()
        .and_then(|limits| limits.get("cpu"))
        .map(Cpu::from);

    let limits_cpu_milliseconds = limits_cpu.map(api::Cpu::to_milliseconds);

    let limits_memory = container
        .resources
        .expect("missing resources")
        .limits
        .as_ref()
        .and_then(|limits| limits.get("memory"))
        .map(Memory::from);

    let limits_memory_bytes = limits_memory.map(api::Memory::to_bytes);

    PodOutput {
        namespace,
        pod_name,
        container_name: container.name,
        owner,

        resources: Resources {
            limits_cpu,
            limits_cpu_milliseconds,
            limits_memory,
            limits_memory_bytes,
            requests_cpu,
            requests_cpu_milliseconds,
            requests_memory,
            requests_memory_bytes,

            cpu_usage: None,
            cpu_usage_milliseconds: None,
            memory_usage: None,
            memory_usage_bytes: None,
        },
    }
}

impl std::ops::Add<&Resources> for &Resources {
    type Output = Resources;

    fn add(self, rhs: &Resources) -> Self::Output {
        fn add_option<T>(a: Option<T>, b: Option<T>) -> Option<T>
        where
            T: std::ops::Add<Output = T>,
        {
            match (a, b) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }

        Resources {
            cpu_usage: add_option(self.cpu_usage, rhs.cpu_usage),
            cpu_usage_milliseconds: add_option(
                self.cpu_usage_milliseconds,
                rhs.cpu_usage_milliseconds,
            ),
            memory_usage: add_option(self.memory_usage, rhs.memory_usage),
            memory_usage_bytes: add_option(self.memory_usage_bytes, rhs.memory_usage_bytes),
            requests_cpu: add_option(self.requests_cpu, rhs.requests_cpu),
            requests_cpu_milliseconds: add_option(
                self.requests_cpu_milliseconds,
                rhs.requests_cpu_milliseconds,
            ),
            requests_memory: add_option(self.requests_memory, rhs.requests_memory),
            requests_memory_bytes: add_option(
                self.requests_memory_bytes,
                rhs.requests_memory_bytes,
            ),
            limits_cpu: add_option(self.limits_cpu, rhs.limits_cpu),
            limits_cpu_milliseconds: add_option(
                self.limits_cpu_milliseconds,
                rhs.limits_cpu_milliseconds,
            ),
            limits_memory: add_option(self.limits_memory, rhs.limits_memory),
            limits_memory_bytes: add_option(self.limits_memory_bytes, rhs.limits_memory_bytes),
        }
    }
}

impl Resources {
    fn set_cpu_usage(mut self, cpu_usage: Option<Cpu>) -> Self {
        self.cpu_usage_milliseconds = cpu_usage.map(api::Cpu::to_milliseconds);
        self.cpu_usage = cpu_usage;

        self
    }

    fn set_memory_usage(mut self, memory_usage: Option<Memory>) -> Self {
        self.memory_usage_bytes = memory_usage.map(api::Memory::to_bytes);
        self.memory_usage = memory_usage;

        self
    }
}
