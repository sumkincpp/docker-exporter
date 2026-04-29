use super::*;
use prometheus::{Encoder, TextEncoder};

struct MockDockerClient {
    data_usage: Option<docker::DataUsage>,
}

impl docker::DockerClient for MockDockerClient {
    async fn list_containers(&self) -> docker::DockerResult<Vec<docker::Container>> {
        self.data_usage
            .as_ref()
            .map(|usage| usage.containers.clone())
            .ok_or_else(|| docker::DockerError::Transport {
                endpoint: "mock".to_owned(),
                error: "missing mocked containers".to_owned(),
            })
    }

    async fn inspect_container(&self, _id: &str) -> docker::DockerResult<docker::ContainerInspect> {
        Err(docker::DockerError::Transport {
            endpoint: "mock".to_owned(),
            error: "inspect not mocked".to_owned(),
        })
    }

    async fn get_container_stats(&self, _id: &str) -> docker::DockerResult<docker::ContainerStats> {
        Err(docker::DockerError::Transport {
            endpoint: "mock".to_owned(),
            error: "stats not mocked".to_owned(),
        })
    }

    async fn get_data_usage(&self) -> docker::DockerResult<docker::DataUsage> {
        self.data_usage
            .clone()
            .ok_or_else(|| docker::DockerError::Transport {
                endpoint: "mock".to_owned(),
                error: "missing mocked data usage".to_owned(),
            })
    }
}

fn volume_metric_lines<D: docker::DockerClient>(collector: &Collector<D>) -> Vec<String> {
    let mut buffer = Vec::new();
    TextEncoder::new()
        .encode(&collector.gather(), &mut buffer)
        .unwrap();
    String::from_utf8(buffer)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("docker_volume_"))
        .map(str::to_owned)
        .collect()
}

fn assert_metric_line(
    lines: &[String],
    metric_name: &str,
    volume_name: &str,
    value: &str,
    expected_labels: &[(&str, &str)],
) {
    let line = lines
        .iter()
        .find(|line| {
            line.starts_with(metric_name)
                && line.contains(format!("name=\"{volume_name}\"").as_str())
        })
        .unwrap_or_else(|| panic!("missing metric {metric_name} for volume {volume_name}"));

    for (key, expected) in expected_labels {
        assert!(
            line.contains(format!("{key}=\"{expected}\"").as_str()),
            "metric line missing {key}={expected}: {line}"
        );
    }

    assert!(
        line.ends_with(format!(" {value}").as_str()),
        "metric line has unexpected value: {line}"
    );
}

#[tokio::test]
async fn collector_uses_mocked_volume_metadata_without_socket_access() {
    let docker = MockDockerClient {
        data_usage: Some(docker::DataUsage {
            containers: Vec::new(),
            images: Vec::new(),
            volumes: vec![
                docker::Volume {
                    name: "anon-volume".to_owned(),
                    driver: "local".to_owned(),
                    labels: std::collections::HashMap::from([(
                        "com.docker.volume.anonymous".to_owned(),
                        "".to_owned(),
                    )]),
                    usage_data: docker::VolumeUsage {
                        ref_count: 0,
                        size: 0,
                    },
                },
                docker::Volume {
                    name: "postgres_data".to_owned(),
                    driver: "local".to_owned(),
                    labels: std::collections::HashMap::from([
                        (
                            "com.docker.compose.project".to_owned(),
                            "billing".to_owned(),
                        ),
                        ("com.docker.compose.service".to_owned(), "db".to_owned()),
                    ]),
                    usage_data: docker::VolumeUsage {
                        ref_count: 1,
                        size: 2048,
                    },
                },
            ],
        }),
    };
    let mut collector = Collector::new(docker);
    let config = Config {
        port: 9417,
        min_log_level: log::LevelFilter::Off,
        collect_image_metrics: false,
        collect_volume_metrics: true,
    };

    assert!(collector.update(&config).await);

    let metrics = volume_metric_lines(&collector);
    assert_metric_line(
        &metrics,
        "docker_volume_size",
        "anon-volume",
        "0",
        &[
            ("anonymous", "true"),
            ("driver", "local"),
            ("compose_project", ""),
            ("service", ""),
        ],
    );
    assert_metric_line(
        &metrics,
        "docker_volume_container_count",
        "anon-volume",
        "0",
        &[
            ("anonymous", "true"),
            ("driver", "local"),
            ("compose_project", ""),
            ("service", ""),
        ],
    );
    assert_metric_line(
        &metrics,
        "docker_volume_size",
        "postgres_data",
        "2048",
        &[
            ("anonymous", "false"),
            ("driver", "local"),
            ("compose_project", "billing"),
            ("service", "db"),
        ],
    );
    assert_metric_line(
        &metrics,
        "docker_volume_container_count",
        "postgres_data",
        "1",
        &[
            ("anonymous", "false"),
            ("driver", "local"),
            ("compose_project", "billing"),
            ("service", "db"),
        ],
    );
}
