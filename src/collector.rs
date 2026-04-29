use crate::Config;
use crate::docker;
use log::debug;
use prometheus::core::Collector as PromCollector;
use prometheus::proto::MetricFamily;
use prometheus::{
    Counter, GaugeVec, Histogram, HistogramOpts, Opts, Registry, exponential_buckets,
};
use std::collections::{HashMap, HashSet};

const CONTAINER_LABELS: &[&str] = &["name"];
const VOLUME_LABELS: &[&str] = &["name", "anonymous", "driver", "compose_project", "service"];
const IMAGE_LABELS: &[&str] = &["tag"];

fn register_metric<M>(registry: &Registry, metric: M) -> M
where
    M: PromCollector + Clone + 'static,
{
    registry.register(Box::new(metric.clone())).unwrap();
    metric
}

#[derive(Clone)]
struct ContainerMetricSet {
    cpu_usage: GaugeVec,
    cpu_capacity: GaugeVec,
    memory_usage: GaugeVec,
    restart_count: GaugeVec,
    running_state: GaugeVec,
    start_time: GaugeVec,
    total_bytes_in: GaugeVec,
    total_bytes_out: GaugeVec,
    total_bytes_read: GaugeVec,
    total_bytes_written: GaugeVec,
}

impl ContainerMetricSet {
    fn new(registry: &Registry) -> Self {
        Self {
            cpu_usage: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_cpu_used_total",
                        "Accumulated CPU usage of a container, in unspecified units, averaged for all logical CPUs usable by the container.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            cpu_capacity: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_cpu_capacity_total",
                        "All potential CPU usage available to a container, in unspecified units, averaged for all logical CPUs usable by the container. Start point of measurement is undefined - only relative values should be used in analytics.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            memory_usage: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_memory_used_bytes",
                        "Memory usage of a container.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            restart_count: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_restart_count",
                        "Number of times the runtime has restarted this container without explicit user action, since the container was last started.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            running_state: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_running_state",
                        "Whether the container is running (1), restarting (0.5) or stopped (0).",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            start_time: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_start_time_seconds",
                        "Timestamp indicating when the container was started. Does not get reset by automatic restarts.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            total_bytes_in: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_network_in_bytes",
                        "Total bytes received by the container's network interfaces.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            total_bytes_out: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_network_out_bytes",
                        "Total bytes sent by the container's network interfaces.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            total_bytes_read: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_disk_read_bytes",
                        "Total bytes read from disk by a container.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
            total_bytes_written: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_container_disk_write_bytes",
                        "Total bytes written to disk by a container.",
                    ),
                    CONTAINER_LABELS,
                )
                .unwrap(),
            ),
        }
    }
}

#[derive(Clone)]
struct VolumeMetricSet {
    size: GaugeVec,
    ref_count: GaugeVec,
}

impl VolumeMetricSet {
    fn new(registry: &Registry) -> Self {
        Self {
            size: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new("docker_volume_size", "Size of a volume in bytes."),
                    VOLUME_LABELS,
                )
                .unwrap(),
            ),
            ref_count: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_volume_container_count",
                        "The number of containers using a volume.",
                    ),
                    VOLUME_LABELS,
                )
                .unwrap(),
            ),
        }
    }
}

#[derive(Clone)]
struct ImageMetricSet {
    container_count: GaugeVec,
    size: GaugeVec,
}

impl ImageMetricSet {
    fn new(registry: &Registry) -> Self {
        Self {
            container_count: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new(
                        "docker_image_container_count",
                        "The number of containers based on an image.",
                    ),
                    IMAGE_LABELS,
                )
                .unwrap(),
            ),
            size: register_metric(
                registry,
                GaugeVec::new(
                    Opts::new("docker_image_size", "The size of on an image in bytes."),
                    IMAGE_LABELS,
                )
                .unwrap(),
            ),
        }
    }
}

struct Metrics {
    registry: Registry,
    probe_duration: Histogram,
    probe_failures: Counter,
    container: ContainerMetricSet,
    volume: VolumeMetricSet,
    image: ImageMetricSet,
}

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();
        let probe_duration = register_metric(
            &registry,
            Histogram::with_opts(
                HistogramOpts::new(
                    "docker_probe_duration_seconds",
                    "How long it takes to query Docker for the complete data set.",
                )
                .buckets(exponential_buckets(1.0, 2.0, 7).unwrap()),
            )
            .unwrap(),
        );
        let probe_failures = register_metric(
            &registry,
            Counter::new(
                "docker_probe_failures_total",
                "The number of times any individual Docker query failed (because of a timeout or other reasons).",
            )
            .unwrap(),
        );

        Self {
            container: ContainerMetricSet::new(&registry),
            volume: VolumeMetricSet::new(&registry),
            image: ImageMetricSet::new(&registry),
            registry,
            probe_duration,
            probe_failures,
        }
    }

    fn gather(&self) -> Vec<MetricFamily> {
        self.registry.gather()
    }
}

mod trackers {
    use crate::docker;
    use log::debug;

    use super::{ContainerMetricSet, ImageMetricSet, VolumeMetricSet};

    pub struct ContainerTracker {
        pub id: String,
        name: String,
        metrics: ContainerMetricSet,
    }

    impl ContainerTracker {
        pub fn new(c: docker::Container, metrics: ContainerMetricSet) -> Self {
            let name = Self::display_name(&c).to_owned();

            Self {
                id: c.id,
                name,
                metrics,
            }
        }

        fn display_name(c: &docker::Container) -> &str {
            match c.names.first() {
                Some(name) if name.trim().len() > 1 => name.trim_start_matches('/'),
                _ => &c.id[..12],
            }
        }

        fn labels(&self) -> [&str; 1] {
            [&self.name]
        }

        pub async fn update<D: docker::DockerClient>(&self, docker: &D) -> Option<()> {
            let inspect = docker.inspect_container(&self.id).await?;
            let labels = self.labels();

            self.metrics
                .running_state
                .with_label_values(&labels)
                .set(if inspect.state.running {
                    1.
                } else if inspect.state.restarting {
                    0.5
                } else {
                    0.
                });
            self.metrics
                .restart_count
                .with_label_values(&labels)
                .set(inspect.restart_count as f64);

            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&inspect.state.started_at) {
                let t = d.timestamp();
                if t > 0 {
                    self.metrics
                        .start_time
                        .with_label_values(&labels)
                        .set(t as f64);
                }
            }

            if !inspect.state.running {
                self.metrics.memory_usage.with_label_values(&labels).set(0.);
                return Some(());
            }

            let stats = docker.get_container_stats(&self.id).await?;
            self.metrics
                .cpu_usage
                .with_label_values(&labels)
                .set(stats.cpu_stats.cpu_usage.total_usage as f64);
            self.metrics
                .cpu_capacity
                .with_label_values(&labels)
                .set(stats.cpu_stats.system_cpu_usage as f64);

            let inactive_file = stats
                .memory_stats
                .stats
                .get("total_inactive_file")
                .copied()
                .or_else(|| stats.memory_stats.stats.get("inactive_file").copied())
                .unwrap_or_default();

            self.metrics
                .memory_usage
                .with_label_values(&labels)
                .set((stats.memory_stats.usage - inactive_file) as f64);
            self.metrics.total_bytes_in.with_label_values(&labels).set(
                stats
                    .networks
                    .values()
                    .map(|network| network.rx_bytes)
                    .sum::<u64>() as f64,
            );
            self.metrics.total_bytes_out.with_label_values(&labels).set(
                stats
                    .networks
                    .values()
                    .map(|network| network.tx_bytes)
                    .sum::<u64>() as f64,
            );
            self.metrics
                .total_bytes_read
                .with_label_values(&labels)
                .set(
                    stats
                        .blkio_stats
                        .io_service_bytes_recursive
                        .iter()
                        .filter_map(|stat| {
                            stat.op.eq_ignore_ascii_case("read").then_some(stat.value)
                        })
                        .sum::<u64>() as f64,
                );
            self.metrics
                .total_bytes_written
                .with_label_values(&labels)
                .set(
                    stats
                        .blkio_stats
                        .io_service_bytes_recursive
                        .iter()
                        .filter_map(|stat| {
                            stat.op.eq_ignore_ascii_case("write").then_some(stat.value)
                        })
                        .sum::<u64>() as f64,
                );

            Some(())
        }
    }

    impl Drop for ContainerTracker {
        fn drop(&mut self) {
            let labels = self.labels();
            debug!("Dropping container tracker {}", self.id);
            let _ = self.metrics.cpu_usage.remove_label_values(&labels);
            let _ = self.metrics.cpu_capacity.remove_label_values(&labels);
            let _ = self.metrics.memory_usage.remove_label_values(&labels);
            let _ = self.metrics.restart_count.remove_label_values(&labels);
            let _ = self.metrics.running_state.remove_label_values(&labels);
            let _ = self.metrics.start_time.remove_label_values(&labels);
            let _ = self.metrics.total_bytes_in.remove_label_values(&labels);
            let _ = self.metrics.total_bytes_out.remove_label_values(&labels);
            let _ = self.metrics.total_bytes_read.remove_label_values(&labels);
            let _ = self
                .metrics
                .total_bytes_written
                .remove_label_values(&labels);
        }
    }

    pub struct VolumeTracker {
        pub name: String,
        anonymous: String,
        driver: String,
        compose_project: String,
        service: String,
        metrics: VolumeMetricSet,
    }

    impl VolumeTracker {
        pub fn new(v: docker::Volume, metrics: VolumeMetricSet) -> Self {
            let tracker = Self {
                name: v.name,
                anonymous: if v.labels.contains_key("com.docker.volume.anonymous") {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                },
                driver: v.driver,
                compose_project: v
                    .labels
                    .get("com.docker.compose.project")
                    .cloned()
                    .unwrap_or_default(),
                service: v
                    .labels
                    .get("com.docker.compose.service")
                    .cloned()
                    .unwrap_or_default(),
                metrics,
            };

            tracker.update(v.usage_data);
            tracker
        }

        fn labels(&self) -> [&str; 5] {
            [
                &self.name,
                &self.anonymous,
                &self.driver,
                &self.compose_project,
                &self.service,
            ]
        }

        pub fn update(&self, usage: docker::VolumeUsage) {
            let labels = self.labels();
            self.metrics
                .size
                .with_label_values(&labels)
                .set(usage.size as f64);
            self.metrics
                .ref_count
                .with_label_values(&labels)
                .set(usage.ref_count as f64);
        }
    }

    impl Drop for VolumeTracker {
        fn drop(&mut self) {
            let labels = self.labels();
            debug!("Dropping volume tracker {}", self.name);
            let _ = self.metrics.size.remove_label_values(&labels);
            let _ = self.metrics.ref_count.remove_label_values(&labels);
        }
    }

    pub struct ImageTracker {
        pub id: String,
        tag: String,
        metrics: ImageMetricSet,
    }

    impl ImageTracker {
        pub fn new(i: docker::Image, metrics: ImageMetricSet) -> Self {
            let tracker = Self {
                tag: i
                    .repo_tags
                    .iter()
                    .find(|tag| !tag.contains("<none>"))
                    .cloned()
                    .unwrap_or_else(|| i.id.clone()),
                id: i.id,
                metrics,
            };

            tracker.update(i.containers, i.size);
            tracker
        }

        fn labels(&self) -> [&str; 1] {
            [&self.tag]
        }

        pub fn update(&self, container_count: u32, size: u64) {
            let labels = self.labels();
            self.metrics
                .container_count
                .with_label_values(&labels)
                .set(container_count as f64);
            self.metrics
                .size
                .with_label_values(&labels)
                .set(size as f64);
        }
    }

    impl Drop for ImageTracker {
        fn drop(&mut self) {
            let labels = self.labels();
            debug!("Dropping image tracker {}", self.id);
            let _ = self.metrics.container_count.remove_label_values(&labels);
            let _ = self.metrics.size.remove_label_values(&labels);
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
    metrics: Metrics,
    container_trackers: HashMap<String, ContainerTracker>,
    volume_trackers: HashMap<String, VolumeTracker>,
    image_trackers: HashMap<String, ImageTracker>,
}

impl<D: docker::DockerClient> Collector<D> {
    pub fn new(docker: D) -> Self {
        Self {
            docker,
            metrics: Metrics::new(),
            container_trackers: HashMap::new(),
            volume_trackers: HashMap::new(),
            image_trackers: HashMap::new(),
        }
    }

    pub fn gather(&self) -> Vec<MetricFamily> {
        self.metrics.gather()
    }

    pub async fn update(&mut self, config: &Config) -> bool {
        let _timer = self.metrics.probe_duration.start_timer();

        let Some((containers, volumes, images)) = self.collect_data(config).await else {
            self.metrics.probe_failures.inc();
            return false;
        };

        let failed_container_updates = self.sync_container_trackers(containers).await;
        if failed_container_updates > 0 {
            self.metrics
                .probe_failures
                .inc_by(failed_container_updates as f64);
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
                .map(|usage| (usage.containers, usage.volumes, usage.images));
        }

        self.docker
            .list_containers()
            .await
            .map(|containers| (containers, Vec::new(), Vec::new()))
    }

    async fn sync_container_trackers(&mut self, containers: Vec<docker::Container>) -> usize {
        let active_ids = containers
            .iter()
            .map(|container| container.id.clone())
            .collect::<HashSet<_>>();
        self.container_trackers
            .retain(|id, _| active_ids.contains(id));

        for container in containers {
            let id = container.id.clone();
            self.container_trackers
                .entry(id.clone())
                .or_insert_with(|| {
                    debug!("Adding container tracker {id}");
                    ContainerTracker::new(container, self.metrics.container.clone())
                });
        }

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
            .map(|volume| volume.name.clone())
            .collect::<HashSet<_>>();
        self.volume_trackers
            .retain(|name, _| active_names.contains(name));

        for volume in volumes {
            let name = volume.name.clone();
            match self.volume_trackers.get(&name) {
                Some(tracker) => tracker.update(volume.usage_data),
                None => {
                    debug!("Adding volume tracker {name}");
                    self.volume_trackers.insert(
                        name,
                        VolumeTracker::new(volume, self.metrics.volume.clone()),
                    );
                }
            }
        }
    }

    fn sync_image_trackers(&mut self, images: Vec<docker::Image>) {
        let active_ids = images
            .iter()
            .map(|image| image.id.clone())
            .collect::<HashSet<_>>();
        self.image_trackers.retain(|id, _| active_ids.contains(id));

        for image in images {
            let id = image.id.clone();
            match self.image_trackers.get(&id) {
                Some(tracker) => tracker.update(image.containers, image.size),
                None => {
                    debug!("Adding image tracker {id}");
                    self.image_trackers
                        .insert(id, ImageTracker::new(image, self.metrics.image.clone()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus::{Encoder, TextEncoder};

    struct MockDockerClient {
        data_usage: Option<docker::DataUsage>,
    }

    impl docker::DockerClient for MockDockerClient {
        async fn list_containers(&self) -> Option<Vec<docker::Container>> {
            self.data_usage
                .as_ref()
                .map(|usage| usage.containers.clone())
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
}
