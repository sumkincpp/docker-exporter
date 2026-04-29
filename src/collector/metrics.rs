use prometheus::core::Collector as PromCollector;
use prometheus::proto::MetricFamily;
use prometheus::{
    Counter, GaugeVec, Histogram, HistogramOpts, Opts, Registry, exponential_buckets,
};

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
pub(super) struct ContainerMetricSet {
    pub(super) cpu_usage: GaugeVec,
    pub(super) cpu_capacity: GaugeVec,
    pub(super) memory_usage: GaugeVec,
    pub(super) restart_count: GaugeVec,
    pub(super) running_state: GaugeVec,
    pub(super) start_time: GaugeVec,
    pub(super) total_bytes_in: GaugeVec,
    pub(super) total_bytes_out: GaugeVec,
    pub(super) total_bytes_read: GaugeVec,
    pub(super) total_bytes_written: GaugeVec,
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
pub(super) struct VolumeMetricSet {
    pub(super) size: GaugeVec,
    pub(super) ref_count: GaugeVec,
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
pub(super) struct ImageMetricSet {
    pub(super) container_count: GaugeVec,
    pub(super) size: GaugeVec,
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

pub(super) struct Metrics {
    registry: Registry,
    pub(super) probe_duration: Histogram,
    pub(super) probe_failures: Counter,
    container: ContainerMetricSet,
    volume: VolumeMetricSet,
    image: ImageMetricSet,
}

impl Metrics {
    pub(super) fn new() -> Self {
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

    pub(super) fn gather(&self) -> Vec<MetricFamily> {
        self.registry.gather()
    }

    pub(super) fn container(&self) -> ContainerMetricSet {
        self.container.clone()
    }

    pub(super) fn volume(&self) -> VolumeMetricSet {
        self.volume.clone()
    }

    pub(super) fn image(&self) -> ImageMetricSet {
        self.image.clone()
    }
}
