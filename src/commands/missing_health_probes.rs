use eyre::Result;
use serde::Serialize;

use crate::api::{get_pod_owner, get_pods, Owner};

pub(crate) async fn missing_health_probes(
    namespaces: Vec<String>,
    all_namespaces: bool,
) -> Result<()> {
    #[derive(Debug, Serialize)]
    struct Output {
        pod_name: String,
        owner: Option<Owner>,
        container_name: String,
        liveness_probe: Option<String>,
        readiness_probe: Option<String>,
    }

    let pods = get_pods(namespaces, all_namespaces).await?;

    let pods: Vec<_> = pods
        .iter()
        .filter(|pod| pod.status.is_some())
        .filter(|pod| {
            pod.status.as_ref().expect("failed to get status").phase == Some("Running".to_string())
        })
        .filter(|pod| pod.spec.is_some())
        .filter(|pod| {
            !pod.spec
                .as_ref()
                .expect("failed to get spec")
                .containers
                .iter()
                .all(|container| {
                    container.liveness_probe.is_some() || container.readiness_probe.is_some()
                })
        })
        .flat_map(|pod| {
            pod.spec
                .as_ref()
                .expect("failed to get spec")
                .containers
                .iter()
                .map(|container| {
                    (
                        container.name.clone(),
                        container
                            .liveness_probe
                            .as_ref()
                            .map(|probe| format!("{probe:?}")),
                        container
                            .readiness_probe
                            .as_ref()
                            .map(|probe| format!("{probe:?}")),
                    )
                })
                .map(|(container_name, liveness_probe, readiness_probe)| Output {
                    pod_name: pod
                        .metadata
                        .name
                        .as_ref()
                        .expect("failed to get name")
                        .clone(),

                    owner: get_pod_owner(pod),
                    container_name,
                    liveness_probe,
                    readiness_probe,
                })
                .filter(|output| {
                    output.liveness_probe.is_none() && output.readiness_probe.is_none()
                })
        })
        .collect();

    let out = serde_json::to_string_pretty(&pods)?;

    println!("{out}");

    Ok(())
}
