use std::collections::{BTreeMap, BTreeSet, HashMap};

use eyre::Result;
use log::warn;
use serde::Serialize;

use crate::api::{get_pod_owner, get_pod_resource_usage, get_pods, Cpu, Memory, Owner};

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default)]
struct Output {
    total_namespace: Vec<TotalNamespace>,
    pods: BTreeSet<PodOutput>,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Default, Clone)]
struct TotalNamespace {
    requests_cpu: Option<Cpu>,
    cpu_usage: Option<Cpu>,
    limits_cpu: Option<Cpu>,
    requests_memory: Option<Memory>,
    limits_memory: Option<Memory>,
    memory_usage: Option<Memory>,
    namespace: String,
}

#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq)]
struct PodOutput {
    requests_cpu: Option<Cpu>,
    cpu_usage: Option<Cpu>,
    limits_cpu: Option<Cpu>,
    requests_memory: Option<Memory>,
    limits_memory: Option<Memory>,
    memory_usage: Option<Memory>,
    pod_name: String,
    container_name: String,
    namespace: String,
    owner: Option<Owner>,
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
        .flat_map(|pod| {
            let owner = get_pod_owner(&pod);

            pod.spec
                .expect("missing spec")
                .containers
                .into_iter()
                .filter(|container| container.resources.is_some())
                .map(move |container| PodOutput {
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
                        .map(Cpu::from),

                    requests_memory: container
                        .resources
                        .as_ref()
                        .expect("missing resources")
                        .requests
                        .as_ref()
                        .and_then(|requests| requests.get("memory"))
                        .map(Memory::from),

                    limits_cpu: container
                        .resources
                        .as_ref()
                        .expect("missing resources")
                        .limits
                        .as_ref()
                        .and_then(|limits| limits.get("cpu"))
                        .map(Cpu::from),

                    limits_memory: container
                        .resources
                        .expect("missing resources")
                        .limits
                        .as_ref()
                        .and_then(|limits| limits.get("memory"))
                        .map(Memory::from),

                    cpu_usage: None,
                    memory_usage: None,
                    owner: owner.clone(),
                })
        })
        .collect::<BTreeSet<PodOutput>>();

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
            let usage = tops.get(&pod.pod_name).expect("failed to get usage");

            if let Some(usage) = usage {
                let container_usage = usage
                    .containers
                    .iter()
                    .find(|container| container.name == pod.container_name);

                PodOutput {
                    cpu_usage: container_usage.map(|container| (&container.usage.cpu).into()),
                    memory_usage: container_usage.map(|container| (&container.usage.memory).into()),
                    ..pod
                }
            } else {
                warn!("Failed to get usage for pod: {}", pod.pod_name);
                pod
            }
        })
        .filter(|pod| {
            // Check if cpu_usage is higher than requests_cpu
            if let Some(cpu_usage) = pod.cpu_usage {
                if let Some(requests_cpu) = pod.requests_cpu {
                    if cpu_usage > requests_cpu {
                        return true;
                    }
                }
            }

            // Check if cpu_usage is below the requests_cpu threshold
            if !no_check_higher {
                if let Some(threshold) = threshold {
                    if let Some(cpu_usage) = pod.cpu_usage {
                        if let Some(requests_cpu) = pod.requests_cpu {
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
        output.iter().fold(HashMap::default(), |mut total, pod| {
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
        total_namespace: total.values().cloned().collect(),
        pods: output,
    };

    let out = serde_json::to_string_pretty(&output)?;

    println!("{out}");

    Ok(())
}

impl std::ops::AddAssign<&PodOutput> for TotalNamespace {
    fn add_assign(&mut self, rhs: &PodOutput) {
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

        self.requests_cpu = add_option(self.requests_cpu, rhs.requests_cpu);
        self.cpu_usage = add_option(self.cpu_usage, rhs.cpu_usage);
        self.limits_cpu = add_option(self.limits_cpu, rhs.limits_cpu);
        self.requests_memory = add_option(self.requests_memory, rhs.requests_memory);
        self.limits_memory = add_option(self.limits_memory, rhs.limits_memory);
        self.memory_usage = add_option(self.memory_usage, rhs.memory_usage);
    }
}
