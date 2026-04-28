use crate::Config;
use crate::docker;
use log::debug;
use prometheus::{Counter, Histogram, exponential_buckets, register_counter, register_histogram};
use std::collections::{HashMap, HashSet};

mod trackers {
    use crate::docker;
    use log::debug;
    use prometheus::{Gauge, Opts, labels, opts, register_gauge};

    macro_rules! unregister {
        ($($COLLECTOR: expr),+) => {{
            $(
                prometheus::unregister(Box::new($COLLECTOR.clone())).unwrap_or(());
            )+
        }};
    }

    pub struct ContainerTracker {
        pub id: String,
        cpu_usage: Gauge,
        cpu_capacity: Gauge,
        memory_usage: Gauge,
        restart_count: Gauge,
        running_state: Gauge,
        start_time: Gauge,
        total_bytes_in: Gauge,
        total_bytes_out: Gauge,
        total_bytes_read: Gauge,
        total_bytes_written: Gauge,
    }

    impl ContainerTracker {
        pub fn new(c: docker::Container) -> ContainerTracker {
            let name = Self::get_display_name(&c);
            let cpu_usage = register_gauge!(opts!("docker_container_cpu_used_total", "Accumulated CPU usage of a container, in unspecified units, averaged for all logical CPUs usable by the container.", labels! { "name" => name })).unwrap();
            let cpu_capacity = register_gauge!(opts!("docker_container_cpu_capacity_total", "All potential CPU usage available to a container, in unspecified units, averaged for all logical CPUs usable by the container. Start point of measurement is undefined - only relative values should be used in analytics.", labels! { "name" => name })).unwrap();
            let memory_usage = register_gauge!(opts!(
                "docker_container_memory_used_bytes",
                "Memory usage of a container.",
                labels! { "name" => name }
            ))
            .unwrap();
            let restart_count = register_gauge!(opts!("docker_container_restart_count", "Number of times the runtime has restarted this container without explicit user action, since the container was last started.", labels! { "name" => name })).unwrap();
            let running_state = register_gauge!(opts!(
                "docker_container_running_state",
                "Whether the container is running (1), restarting (0.5) or stopped (0).",
                labels! { "name" => name }
            ))
            .unwrap();
            let start_time = register_gauge!(opts!("docker_container_start_time_seconds", "Timestamp indicating when the container was started. Does not get reset by automatic restarts.", labels! { "name" => name })).unwrap();
            let total_bytes_in = register_gauge!(opts!(
                "docker_container_network_in_bytes",
                "Total bytes received by the container's network interfaces.",
                labels! { "name" => name }
            ))
            .unwrap();
            let total_bytes_out = register_gauge!(opts!(
                "docker_container_network_out_bytes",
                "Total bytes sent by the container's network interfaces.",
                labels! { "name" => name }
            ))
            .unwrap();
            let total_bytes_read = register_gauge!(opts!(
                "docker_container_disk_read_bytes",
                "Total bytes read from disk by a container.",
                labels! { "name" => name }
            ))
            .unwrap();
            let total_bytes_written = register_gauge!(opts!(
                "docker_container_disk_write_bytes",
                "Total bytes written to disk by a container.",
                labels! { "name" => name }
            ))
            .unwrap();

            ContainerTracker {
                id: c.Id,
                cpu_usage,
                cpu_capacity,
                memory_usage,
                restart_count,
                running_state,
                start_time,
                total_bytes_in,
                total_bytes_out,
                total_bytes_read,
                total_bytes_written,
            }
        }

        fn get_display_name(c: &docker::Container) -> &str {
            match c.Names.first() {
                Some(name) if name.trim().len() > 1 => name.trim_start_matches('/'),
                _ => &c.Id[..12],
            }
        }

        pub async fn update<D: docker::DockerClient>(&self, docker: &D) -> Option<()> {
            let inspect = docker.inspect_container(&self.id).await?;

            self.running_state.set(if inspect.State.Running {
                1.
            } else if inspect.State.Restarting {
                0.5
            } else {
                0.
            });
            self.restart_count.set(inspect.RestartCount as f64);

            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&inspect.State.StartedAt) {
                let t = d.timestamp();

                if t > 0 {
                    self.start_time.set(t as f64);
                }
            }

            if !inspect.State.Running {
                self.memory_usage.set(0.);
                return Some(());
            }

            let stats = docker.get_container_stats(&self.id).await?;
            self.cpu_usage
                .set(stats.cpu_stats.cpu_usage.total_usage as f64);
            self.cpu_capacity
                .set(stats.cpu_stats.system_cpu_usage as f64);

            let tmp = stats
                .memory_stats
                .stats
                .get("total_inactive_file")
                .copied()
                .or_else(|| stats.memory_stats.stats.get("inactive_file").copied())
                .unwrap_or_default();

            self.memory_usage
                .set((stats.memory_stats.usage - tmp) as f64);

            self.total_bytes_in
                .set(stats.networks.iter().map(|kvp| kvp.1.rx_bytes).sum::<u64>() as f64);
            self.total_bytes_out
                .set(stats.networks.iter().map(|kvp| kvp.1.tx_bytes).sum::<u64>() as f64);

            self.total_bytes_read.set(
                stats
                    .blkio_stats
                    .io_service_bytes_recursive
                    .iter()
                    .filter_map(|s| {
                        if s.op.eq_ignore_ascii_case("read") {
                            Some(s.value)
                        } else {
                            None
                        }
                    })
                    .sum::<u64>() as f64,
            );
            self.total_bytes_written.set(
                stats
                    .blkio_stats
                    .io_service_bytes_recursive
                    .iter()
                    .filter_map(|s| {
                        if s.op.eq_ignore_ascii_case("write") {
                            Some(s.value)
                        } else {
                            None
                        }
                    })
                    .sum::<u64>() as f64,
            );

            Some(())
        }
    }

    impl Drop for ContainerTracker {
        fn drop(&mut self) {
            debug!("Dropping container tracker {}", self.id);
            unregister!(
                self.cpu_usage,
                self.cpu_capacity,
                self.memory_usage,
                self.restart_count,
                self.running_state,
                self.start_time,
                self.total_bytes_in,
                self.total_bytes_out,
                self.total_bytes_read,
                self.total_bytes_written
            );
        }
    }

    pub struct VolumeTracker {
        pub name: String,
        size: Gauge,
        ref_count: Gauge,
    }

    impl VolumeTracker {
        pub fn new(v: docker::Volume) -> VolumeTracker {
            let volume_labels = Self::metric_labels(&v);
            let size = register_gauge!(
                Opts::new("docker_volume_size", "Size of a volume in bytes.")
                    .const_labels(volume_labels.clone())
            )
            .unwrap();
            let ref_count = register_gauge!(
                Opts::new(
                    "docker_volume_container_count",
                    "The number of containers using a volume."
                )
                .const_labels(volume_labels)
            )
            .unwrap();

            let s = VolumeTracker {
                name: v.Name,
                size,
                ref_count,
            };

            Self::update(&s, v.UsageData);
            s
        }

        pub fn update(&self, v: docker::VolumeUsage) {
            self.size.set(v.Size as f64);
            self.ref_count.set(v.RefCount as f64);
        }

        fn metric_labels(v: &docker::Volume) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::from([
                ("name".to_owned(), v.Name.clone()),
                (
                    "anonymous".to_owned(),
                    if v.Labels.contains_key("com.docker.volume.anonymous") {
                        "true"
                    } else {
                        "false"
                    }
                    .to_owned(),
                ),
                ("driver".to_owned(), v.Driver.clone()),
                (
                    "compose_project".to_owned(),
                    v.Labels
                        .get("com.docker.compose.project")
                        .cloned()
                        .unwrap_or_default(),
                ),
                (
                    "service".to_owned(),
                    v.Labels
                        .get("com.docker.compose.service")
                        .cloned()
                        .unwrap_or_default(),
                ),
            ])
        }
    }

    impl Drop for VolumeTracker {
        fn drop(&mut self) {
            debug!("Dropping volume tracker {}", self.name);
            unregister!(self.size, self.ref_count);
        }
    }

    pub struct ImageTracker {
        pub id: String,
        container_count: Gauge,
        size: Gauge,
    }

    impl ImageTracker {
        pub fn new(i: docker::Image) -> ImageTracker {
            let tag = i
                .RepoTags
                .iter()
                .find(|x| !x.contains("<none>"))
                .unwrap_or(&i.Id);
            let container_count = register_gauge!(opts!(
                "docker_image_container_count",
                "The number of containers based on an image.",
                labels! { "tag" => tag }
            ))
            .unwrap();
            let size = register_gauge!(opts!(
                "docker_image_size",
                "The size of on an image in bytes.",
                labels! { "tag" => tag }
            ))
            .unwrap();

            let s = ImageTracker {
                id: i.Id,
                container_count,
                size,
            };

            Self::update(&s, i.Containers, i.Size);
            s
        }

        pub fn update(&self, container_count: u32, size: u64) {
            self.container_count.set(container_count as f64);
            self.size.set(size as f64);
        }
    }

    impl Drop for ImageTracker {
        fn drop(&mut self) {
            debug!("Dropping image tracker {}", self.id);
            unregister!(self.size, self.container_count);
        }
    }
}

use trackers::*;

type CollectedData = (
    Vec<docker::Container>,
    Vec<docker::Volume>,
    Vec<docker::Image>,
);

pub struct Collector<D> {
    docker: D,
    probe_duration: Histogram,
    probe_failures: Counter,
    container_trackers: HashMap<String, ContainerTracker>,
    volume_trackers: HashMap<String, VolumeTracker>,
    image_trackers: HashMap<String, ImageTracker>,
}

impl<D: docker::DockerClient> Collector<D> {
    pub fn new(docker: D) -> Collector<D> {
        let buckets = exponential_buckets(1.0, 2.0, 7).unwrap();
        let probe_duration = register_histogram!(
            "docker_probe_duration_seconds",
            "How long it takes to query Docker for the complete data set.",
            buckets
        )
        .unwrap();
        let probe_failures = register_counter!("docker_probe_failures_total", "The number of times any individual Docker query failed (because of a timeout or other reasons).").unwrap();

        Collector {
            docker,
            probe_duration,
            probe_failures,
            container_trackers: HashMap::new(),
            volume_trackers: HashMap::new(),
            image_trackers: HashMap::new(),
        }
    }

    pub async fn update(&mut self, config: &Config) -> bool {
        let _timer = self.probe_duration.start_timer();

        let Some((containers, volumes, images)) = self.collect_data(config).await else {
            self.probe_failures.inc();
            return false;
        };

        let failed_container_updates = self.sync_container_trackers(containers).await;
        if failed_container_updates > 0 {
            self.probe_failures.inc_by(failed_container_updates as f64);
        }

        if config.collect_volume_metrics {
            self.sync_volume_trackers(volumes);
        }

        if config.collect_image_metrics {
            self.sync_image_trackers(images);
        }

        true
    }

    async fn collect_data(&self, config: &Config) -> Option<CollectedData> {
        if config.collect_image_metrics || config.collect_volume_metrics {
            return self
                .docker
                .get_data_usage()
                .await
                .map(|x| (x.Containers, x.Volumes, x.Images));
        }

        // List only containers when we're not collecting images or volumes - it's faster.
        self.docker
            .list_containers()
            .await
            .map(|x| (x, Vec::new(), Vec::new()))
    }

    async fn sync_container_trackers(&mut self, containers: Vec<docker::Container>) -> usize {
        let active_ids = containers
            .iter()
            .map(|container| container.Id.clone())
            .collect::<HashSet<_>>();
        self.container_trackers
            .retain(|id, _| active_ids.contains(id));

        for container in containers {
            let id = container.Id.clone();
            self.container_trackers
                .entry(id.clone())
                .or_insert_with(|| {
                    debug!("Adding container tracker {id}");
                    ContainerTracker::new(container)
                });
        }

        // Containers that are removed after the list call, but before the update below, are cleaned up next time.
        let update_results = futures::future::join_all(
            self.container_trackers
                .values()
                .map(|tracker| tracker.update(&self.docker)),
        )
        .await;

        update_results
            .into_iter()
            .filter(|result| result.is_none())
            .count()
    }

    fn sync_volume_trackers(&mut self, volumes: Vec<docker::Volume>) {
        let active_names = volumes
            .iter()
            .map(|volume| volume.Name.clone())
            .collect::<HashSet<_>>();
        self.volume_trackers
            .retain(|name, _| active_names.contains(name));

        for volume in volumes {
            let name = volume.Name.clone();
            match self.volume_trackers.get(&name) {
                Some(tracker) => tracker.update(volume.UsageData),
                None => {
                    debug!("Adding volume tracker {name}");
                    self.volume_trackers
                        .insert(name, VolumeTracker::new(volume));
                }
            }
        }
    }

    fn sync_image_trackers(&mut self, images: Vec<docker::Image>) {
        let active_ids = images
            .iter()
            .map(|image| image.Id.clone())
            .collect::<HashSet<_>>();
        self.image_trackers.retain(|id, _| active_ids.contains(id));

        for image in images {
            let id = image.Id.clone();
            match self.image_trackers.get(&id) {
                Some(tracker) => tracker.update(image.Containers, image.Size),
                None => {
                    debug!("Adding image tracker {id}");
                    self.image_trackers.insert(id, ImageTracker::new(image));
                }
            }
        }
    }
}

impl<D> Drop for Collector<D> {
    fn drop(&mut self) {
        prometheus::unregister(Box::new(self.probe_duration.clone())).unwrap_or(());
        prometheus::unregister(Box::new(self.probe_failures.clone())).unwrap_or(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use once_cell::sync::Lazy;
    use prometheus::{Encoder, TextEncoder};
    use simplelog::Config as LogConfig;
    use std::sync::{Mutex, MutexGuard};

    struct MockDockerClient {
        data_usage: Option<docker::DataUsage>,
    }

    impl docker::DockerClient for MockDockerClient {
        async fn list_containers(&self) -> Option<Vec<docker::Container>> {
            self.data_usage
                .as_ref()
                .map(|usage| usage.Containers.clone())
        }

        async fn inspect_container(&self, _id: &str) -> Option<docker::ContainerInspect> {
            None
        }

        async fn get_container_stats(&self, _id: &str) -> Option<docker::ContainerStats> {
            None
        }

        async fn get_data_usage(&self) -> Option<docker::DataUsage> {
            self.data_usage.clone()
        }
    }

    static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    fn test_guard() -> MutexGuard<'static, ()> {
        let _ = simplelog::SimpleLogger::init(log::LevelFilter::Off, LogConfig::default());
        TEST_LOCK.lock().unwrap()
    }

    fn volume_metric_lines() -> Vec<String> {
        let mut buffer = Vec::new();
        TextEncoder::new()
            .encode(&prometheus::gather(), &mut buffer)
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
        let _guard = test_guard();
        let docker = MockDockerClient {
            data_usage: Some(docker::DataUsage {
                Containers: Vec::new(),
                Images: Vec::new(),
                Volumes: vec![
                    docker::Volume {
                        Name: "anon-volume".to_owned(),
                        Driver: "local".to_owned(),
                        Labels: std::collections::HashMap::from([(
                            "com.docker.volume.anonymous".to_owned(),
                            "".to_owned(),
                        )]),
                        UsageData: docker::VolumeUsage {
                            RefCount: 0,
                            Size: 0,
                        },
                    },
                    docker::Volume {
                        Name: "postgres_data".to_owned(),
                        Driver: "local".to_owned(),
                        Labels: std::collections::HashMap::from([
                            (
                                "com.docker.compose.project".to_owned(),
                                "billing".to_owned(),
                            ),
                            ("com.docker.compose.service".to_owned(), "db".to_owned()),
                        ]),
                        UsageData: docker::VolumeUsage {
                            RefCount: 1,
                            Size: 2048,
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

        let metrics = volume_metric_lines();
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
}
