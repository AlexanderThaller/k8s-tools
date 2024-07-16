use bytesize::ByteSize;
use eyre::eyre;
use eyre::{Context, Result};
use k8s_openapi::{
    api::{apps::v1::ReplicaSet, batch::v1::Job, core::v1::Pod},
    apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::OwnerReference},
};
use kube::{api::ListParams, core::ObjectMeta, Api, Client};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("failed to create kubernetes client: {0}")]
    CreateClient(kube::Error),

    #[error("failed to list pods: {0}")]
    ListPods(kube::Error),
}

#[derive(Debug, Ord, PartialOrd, PartialEq, Eq, Clone, Copy, Default)]
pub(crate) struct Memory(u64);

#[derive(Debug, Ord, PartialOrd, PartialEq, Eq, Clone, Copy, Default)]
pub(crate) struct Cpu(u64);

#[derive(serde::Deserialize, Clone, Debug)]
pub(crate) struct PodMetricsContainer {
    pub(crate) name: String,
    pub(crate) usage: PodMetricsContainerUsage,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub(crate) struct PodMetricsContainerUsage {
    pub(crate) cpu: Quantity,
    pub(crate) memory: Quantity,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub(crate) struct PodMetrics {
    pub(crate) metadata: ObjectMeta,
    #[allow(unused)]
    pub(crate) timestamp: String,
    #[allow(unused)]
    pub(crate) window: String,
    pub(crate) containers: Vec<PodMetricsContainer>,
}

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Serialize, Clone)]
pub(crate) struct Owner {
    pub(crate) name: String,
    pub(crate) kind: String,
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

pub(crate) async fn get_pods(namespaces: Vec<String>, all_namespaces: bool) -> Result<Vec<Pod>> {
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

pub(crate) fn get_sync<T>(namespace: &str, name: &str) -> Result<T>
where
    T: k8s_openapi::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + k8s_openapi::Metadata<Ty = ObjectMeta>,
{
    tokio::task::block_in_place(|| {
        let handle = tokio::runtime::Handle::current();
        handle.block_on(get::<T>(namespace, name))
    })
}

pub(crate) async fn get<T>(namespace: &str, name: &str) -> Result<T>
where
    T: k8s_openapi::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + k8s_openapi::Metadata<Ty = ObjectMeta>,
{
    let client = Client::try_default()
        .await
        .map_err(ApiError::CreateClient)?;

    let api: Api<T> = Api::namespaced(client, namespace);
    let lp = ListParams::default().fields(&format!("metadata.name={name}"));

    let mut out = api
        .list(&lp)
        .await
        .with_context(|| "failed to get from kubernetes api")?
        .items;

    assert!(out.len() == 1, "expected 1 replica set got {}", out.len());

    Ok(out.remove(0))
}

pub(crate) fn get_pod_owner(pod: &Pod) -> Option<Owner> {
    pod.metadata
        .owner_references
        .as_ref()
        .and_then(|owner_references| {
            owner_references
                .iter()
                .find(|owner_reference| owner_reference.controller.unwrap_or(false))
                .map(|owner_reference| match owner_reference.kind.as_str() {
                    "ReplicaSet" => {
                        let replica_set = get_sync::<ReplicaSet>(
                            pod.metadata
                                .namespace
                                .as_ref()
                                .expect("failed to get namespace"),
                            &owner_reference.name,
                        )
                        .expect("failed to get replica set");

                        extract_owner(&replica_set)
                            .unwrap_or(owner_reference)
                            .clone()
                    }

                    "Job" => {
                        let job = get_sync::<Job>(
                            pod.metadata
                                .namespace
                                .as_ref()
                                .expect("failed to get namespace"),
                            &owner_reference.name,
                        )
                        .expect("failed to get job");

                        extract_owner(&job).unwrap_or(owner_reference).clone()
                    }

                    _ => owner_reference.clone(),
                })
                .map(|owner_reference| Owner {
                    name: owner_reference.name,
                    kind: owner_reference.kind,
                })
        })
}

pub(crate) fn extract_owner<T>(object: &T) -> Option<&OwnerReference>
where
    T: k8s_openapi::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + k8s_openapi::Metadata<Ty = ObjectMeta>,
{
    object
        .metadata()
        .owner_references
        .as_ref()
        .and_then(|owner_references| {
            owner_references
                .iter()
                .find(|owner_reference| owner_reference.controller.unwrap_or(false))
        })
}

pub(crate) async fn get_pod_resource_usage(
    namespace: &str,
    pod: &str,
) -> Result<Option<PodMetrics>> {
    let client = Client::try_default()
        .await
        .map_err(ApiError::CreateClient)?;

    let api: Api<PodMetrics> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().fields(&format!("metadata.name={pod}"));

    let mut out = api
        .list(&lp)
        .await
        .expect("failed to list pod metrics")
        .items;

    if out.len() != 1 {
        return Ok(None);
    }

    Ok(Some(out.remove(0)))
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

impl TryFrom<&Quantity> for Cpu {
    type Error = eyre::Error;

    fn try_from(value: &Quantity) -> Result<Self, Self::Error> {
        let number = quantity_to_number(value).wrap_err("failed to convert quantity to number")?;

        Ok(Self(number))
    }
}

impl TryFrom<&Quantity> for Memory {
    type Error = eyre::Error;

    fn try_from(value: &Quantity) -> Result<Self, Self::Error> {
        let number = quantity_to_number(value).wrap_err("failed to convert quantity to number")?;

        Ok(Self(number))
    }
}

fn quantity_to_number(input: &Quantity) -> Result<u64> {
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

    let number = if suffix.is_empty() {
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
                return Err(eyre!("invalid suffix {suffix}"));
            }
        }
    };

    Ok(number)
}

impl Memory {
    pub(crate) fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl Cpu {
    pub(crate) fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl From<u64> for Cpu {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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

            let output = super::quantity_to_number(&input).unwrap();
            assert_eq!(expected, output);
        }
    }
}

impl std::ops::Add<Cpu> for Cpu {
    type Output = Cpu;

    fn add(self, rhs: Cpu) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Add<Memory> for Memory {
    type Output = Memory;

    fn add(self, rhs: Memory) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Cpu {
    pub(crate) fn to_milliseconds(self) -> u64 {
        self.0
    }
}

impl Memory {
    pub(crate) fn to_bytes(self) -> u64 {
        self.0
    }
}
