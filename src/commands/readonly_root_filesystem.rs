use std::collections::BTreeSet;

use eyre::{bail, Result};
use k8s_openapi::api::core::v1::Pod;
use serde::Serialize;

use crate::api::{get_pod_owner, get_pods, Owner};

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Serialize)]
pub(crate) struct NoReadOnlyRootFilesystem {
    namespace: String,
    owner: Option<Owner>,
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
        .flat_map(|pod| {
            all_pod_containers_read_only(pod).expect("failed to get all pod containers")
        })
        .collect::<Vec<_>>();

    let output = serde_json::to_string_pretty(&pods)?;

    println!("{output}");

    Ok(())
}

fn all_pod_containers_read_only(pod: &Pod) -> Result<BTreeSet<NoReadOnlyRootFilesystem>> {
    if pod.spec.is_none() {
        bail!("Pod has no spec");
    }

    let spec = pod.spec.as_ref().expect("failed to get spec");

    let containers_not_read_only = spec
        .containers
        .iter()
        .filter(|container| {
            if let Some(security_context) = &container.security_context {
                !security_context.read_only_root_filesystem.unwrap_or(false)
            } else {
                true
            }
        })
        .map(|container| NoReadOnlyRootFilesystem {
            namespace: pod
                .metadata
                .namespace
                .as_ref()
                .expect("failed to get namespace")
                .to_string(),

            pod_name: pod
                .metadata
                .name
                .as_ref()
                .expect("failed to get name")
                .to_string(),

            container_name: container.name.clone(),

            owner: get_pod_owner(pod),
        })
        .collect();

    Ok(containers_not_read_only)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {
    use std::collections::BTreeSet;

    use crate::commands::readonly_root_filesystem::NoReadOnlyRootFilesystem;

    #[test]
    fn readonly_pod() {
        let pod = k8s_openapi::api::core::v1::Pod {
            metadata: kube::api::ObjectMeta {
                namespace: Some("test".to_string()),
                name: Some("pod".to_string()),
                ..Default::default()
            },

            spec: Some(k8s_openapi::api::core::v1::PodSpec {
                containers: vec![
                    k8s_openapi::api::core::v1::Container {
                        name: "readonly".to_string(),
                        security_context: Some(k8s_openapi::api::core::v1::SecurityContext {
                            read_only_root_filesystem: Some(true),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    k8s_openapi::api::core::v1::Container {
                        name: "readwrite-explicit".to_string(),
                        security_context: Some(k8s_openapi::api::core::v1::SecurityContext {
                            read_only_root_filesystem: Some(false),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    k8s_openapi::api::core::v1::Container {
                        name: "readwrite".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };

        let expected = vec![
            NoReadOnlyRootFilesystem {
                namespace: "test".to_string(),
                owner: None,
                pod_name: "pod".to_string(),
                container_name: "readwrite-explicit".to_string(),
            },
            NoReadOnlyRootFilesystem {
                namespace: "test".to_string(),
                owner: None,
                pod_name: "pod".to_string(),
                container_name: "readwrite".to_string(),
            },
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        let output = super::all_pod_containers_read_only(&pod).unwrap();

        assert_eq!(expected, output);
    }
}
