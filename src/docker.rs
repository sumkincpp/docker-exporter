mod contract {
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct Container {
        pub id: String,
        pub names: Vec<String>,
    }

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct ContainerState {
        pub running: bool,
        pub restarting: bool,
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub started_at: String,
    }

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct ContainerInspect {
        pub state: ContainerState,
        pub restart_count: u32,
    }

    #[derive(Clone, Deserialize)]
    pub struct MemoryStats {
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub stats: HashMap<String, u64>,
        #[serde(default)]
        pub usage: u64,
    }

    #[derive(Clone, Default, Deserialize)]
    pub struct CpuUsage {
        pub total_usage: u64,
    }

    #[derive(Clone, Deserialize)]
    pub struct CpuStats {
        pub cpu_usage: CpuUsage,
        #[serde(default)]
        pub system_cpu_usage: u64,
    }

    #[derive(Clone, Deserialize)]
    pub struct Network {
        pub rx_bytes: u64,
        pub tx_bytes: u64,
    }

    #[derive(Clone, Deserialize)]
    pub struct BlkioServiceBytesStat {
        pub op: String,
        pub value: u64,
    }

    #[derive(Clone, Default, Deserialize)]
    pub struct BlkioStats {
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub io_service_bytes_recursive: Vec<BlkioServiceBytesStat>,
    }

    #[derive(Clone, Deserialize)]
    pub struct ContainerStats {
        pub cpu_stats: CpuStats,
        pub memory_stats: MemoryStats,
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub networks: HashMap<String, Network>,
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub blkio_stats: BlkioStats,
    }

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct Image {
        pub id: String,
        pub containers: u32,
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub repo_tags: Vec<String>,
        pub size: u64,
    }

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct VolumeUsage {
        pub ref_count: u32,
        pub size: u64,
    }

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct Volume {
        pub name: String,
        pub driver: String,
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub labels: HashMap<String, String>,
        pub usage_data: VolumeUsage,
    }

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct DataUsage {
        pub images: Vec<Image>,
        pub containers: Vec<Container>,
        #[serde(deserialize_with = "deserialize_null_default", default)]
        pub volumes: Vec<Volume>,
    }

    fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        T: Default + Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        let opt = Option::deserialize(deserializer)?;
        Ok(opt.unwrap_or_default())
    }
}

use hyper::{Body, Client, body};
use hyperlocal::{UnixClientExt, UnixConnector, Uri};
use log::error;
use std::time::Duration;
use tokio::select;
use tokio::time;

pub use contract::*;

const SOCKET_PATH: &str = "/var/run/docker.sock";
const API_VERSION: &str = "/v1.44";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONTAINERS_ENDPOINT: &str = "/v1.44/containers/json?all=true";
const DATA_USAGE_ENDPOINT: &str = "/v1.44/system/df";

pub trait DockerClient: Send + Sync {
    async fn list_containers(&self) -> Option<Vec<Container>>;
    async fn inspect_container(&self, id: &str) -> Option<ContainerInspect>;
    async fn get_container_stats(&self, id: &str) -> Option<ContainerStats>;
    async fn get_data_usage(&self) -> Option<DataUsage>;
}

pub struct UnixSocketClient {
    socket_path: String,
    client: Client<UnixConnector, Body>,
}

impl UnixSocketClient {
    pub fn new(socket_path: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.into(),
            client: Client::unix(),
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> Option<T> {
        let response = select! {
            () = time::sleep(REQUEST_TIMEOUT) => {
                error!("{endpoint} timed out.");
                return None;
            }
            response = self.client.get(Uri::new(&self.socket_path, endpoint).into()) => match response {
                Ok(response) => response,
                Err(error) => {
                    error!("{endpoint} {error}");
                    return None;
                }
            }
        };

        let status = response.status();
        let body = match body::to_bytes(response).await {
            Ok(body) => body,
            Err(error) => {
                error!("{endpoint} {error}");
                return None;
            }
        };

        if !status.is_success() {
            error!(
                "{endpoint} HTTP {status} - {}",
                String::from_utf8_lossy(&body)
            );
            return None;
        }

        match serde_json::from_slice::<T>(&body) {
            Ok(data) => Some(data),
            Err(error) => {
                error!(
                    "{endpoint} deserialization error {error} - {}",
                    String::from_utf8_lossy(&body)
                );
                None
            }
        }
    }
}

impl Default for UnixSocketClient {
    fn default() -> Self {
        Self::new(SOCKET_PATH)
    }
}

impl DockerClient for UnixSocketClient {
    async fn list_containers(&self) -> Option<Vec<Container>> {
        self.get(CONTAINERS_ENDPOINT).await
    }

    async fn inspect_container(&self, id: &str) -> Option<ContainerInspect> {
        let endpoint = format!("{API_VERSION}/containers/{id}/json");
        self.get(&endpoint).await
    }

    async fn get_container_stats(&self, id: &str) -> Option<ContainerStats> {
        let endpoint = format!("{API_VERSION}/containers/{id}/stats?stream=false");
        self.get(&endpoint).await
    }

    async fn get_data_usage(&self) -> Option<DataUsage> {
        self.get(DATA_USAGE_ENDPOINT).await
    }
}
