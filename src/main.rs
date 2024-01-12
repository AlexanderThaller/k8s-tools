use eyre::Result;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::ListParams,
    Api,
    Client,
};

#[allow(unused)]
#[derive(Debug)]
struct Output {
    pod_name: String,
    container_name: String,
    liveness_probe: Option<String>,
    readiness_probe: Option<String>,
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
    let client = Client::try_default()
        .await
        .map_err(ApiError::CreateClient)?;

    let namespaces = ["default", "kube-system"];

    let pods: Vec<Api<Pod>> = namespaces
        .iter()
        .map(|namespace| Api::namespaced(client.clone(), namespace))
        .collect();

    let lp = ListParams::default();
    let mut pods_list = Vec::new();

    for pod in pods {
        pods_list.extend(pod.list(&lp).await.map_err(ApiError::ListPods)?);
    }

    let pods_list: Vec<_> = pods_list
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

    for pod in pods_list {
        println!("{:#?}", pod);
    }

    Ok(())
}
