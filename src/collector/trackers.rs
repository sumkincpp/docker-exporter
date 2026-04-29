use crate::docker;
use log::debug;

use super::metrics::{ContainerMetricSet, ImageMetricSet, VolumeMetricSet};

pub(super) struct ContainerTracker {
    pub(super) id: String,
    name: String,
    metrics: ContainerMetricSet,
}

impl ContainerTracker {
    pub(super) fn new(c: docker::Container, metrics: ContainerMetricSet) -> Self {
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

    pub(super) async fn update<D: docker::DockerClient>(
        &self,
        docker: &D,
    ) -> docker::DockerResult<()> {
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
            return Ok(());
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
                    .filter_map(|stat| stat.op.eq_ignore_ascii_case("read").then_some(stat.value))
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
                    .filter_map(|stat| stat.op.eq_ignore_ascii_case("write").then_some(stat.value))
                    .sum::<u64>() as f64,
            );

        Ok(())
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

pub(super) struct VolumeTracker {
    pub(super) name: String,
    anonymous: String,
    driver: String,
    compose_project: String,
    service: String,
    metrics: VolumeMetricSet,
}

impl VolumeTracker {
    pub(super) fn new(v: docker::Volume, metrics: VolumeMetricSet) -> Self {
        let usage_data = v.usage_data.clone();
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

        tracker.update_usage(&usage_data);
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

    pub(super) fn update(&self, volume: &docker::Volume) {
        self.update_usage(&volume.usage_data);
    }

    fn update_usage(&self, usage_data: &docker::VolumeUsage) {
        let labels = self.labels();
        self.metrics
            .size
            .with_label_values(&labels)
            .set(usage_data.size as f64);
        self.metrics
            .ref_count
            .with_label_values(&labels)
            .set(usage_data.ref_count as f64);
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

pub(super) struct ImageTracker {
    pub(super) id: String,
    tag: String,
    metrics: ImageMetricSet,
}

impl ImageTracker {
    pub(super) fn new(i: docker::Image, metrics: ImageMetricSet) -> Self {
        let container_count = i.containers;
        let size = i.size;
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

        tracker.update_values(container_count, size);
        tracker
    }

    fn labels(&self) -> [&str; 1] {
        [&self.tag]
    }

    pub(super) fn update(&self, image: &docker::Image) {
        self.update_values(image.containers, image.size);
    }

    fn update_values(&self, container_count: u32, size: u64) {
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
