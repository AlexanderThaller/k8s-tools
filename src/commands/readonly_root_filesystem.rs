use std::collections::BTreeSet;

use eyre::{
    bail,
    Result,
};
use k8s_openapi::api::core::v1::Pod;

use crate::api::get_pods;

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq)]
pub(crate) struct NoReadOnlyRootFilesystem {
    namespace: String,
    pod_name: String,
    container_name: String,
}

pub(crate) async fn readonly_root_filesystem(
    namespaces: Vec<String>,
    all_namespaces: bool,
) -> Result<()> {
    let pods = get_pods(namespaces, all_namespaces).await?;

    let pods = pods
        .iter()
        .flat_map(|pod| all_pod_containers_read_only(pod).unwrap())
        .collect::<Vec<_>>();

    dbg!(pods);

    todo!()
}

fn all_pod_containers_read_only(pod: &Pod) -> Result<BTreeSet<NoReadOnlyRootFilesystem>> {
    if pod.spec.is_none() {
        bail!("Pod has no spec");
    }

    let spec = pod.spec.as_ref().unwrap();

    let containers_not_read_only = spec
        .containers
        .iter()
        .filter(|container| {
            if let Some(security_context) = &container.security_context {
                security_context.read_only_root_filesystem.unwrap_or(true)
            } else {
                true
            }
        })
        .map(|container| NoReadOnlyRootFilesystem {
            namespace: pod.metadata.namespace.as_ref().unwrap().to_string(),
            pod_name: pod.metadata.name.as_ref().unwrap().to_string(),
            container_name: container.name.clone(),
        })
        .collect();

    Ok(containers_not_read_only)
}
