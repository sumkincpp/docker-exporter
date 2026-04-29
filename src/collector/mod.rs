use crate::Config;
use crate::docker;
use log::{debug, error};
use prometheus::proto::MetricFamily;
use std::collections::HashMap;
use std::hash::Hash;

mod metrics;
mod trackers;

use metrics::Metrics;
use trackers::{ContainerTracker, ImageTracker, VolumeTracker};

#[cfg(test)]
mod tests;

type CollectedData = (
    Vec<docker::Container>,
    Vec<docker::Volume>,
    Vec<docker::Image>,
);

fn retain_active<K, V, I>(trackers: &mut HashMap<K, V>, active_keys: I)
where
    K: Eq + Hash,
    I: IntoIterator<Item = K>,
{
    let active = active_keys
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    trackers.retain(|key, _| active.contains(key));
}

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

        let (containers, volumes, images) = match self.collect_data(config).await {
            Ok(data) => data,
            Err(error) => {
                self.metrics.probe_failures.inc();
                error!("{error}");
                return false;
            }
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

    async fn collect_data(&self, config: &Config) -> docker::DockerResult<CollectedData> {
        if config.collect_image_metrics || config.collect_volume_metrics {
            let usage = self.docker.get_data_usage().await?;
            return Ok((usage.containers, usage.volumes, usage.images));
        }

        let containers = self.docker.list_containers().await?;
        Ok((containers, Vec::new(), Vec::new()))
    }

    async fn sync_container_trackers(&mut self, containers: Vec<docker::Container>) -> usize {
        retain_active(
            &mut self.container_trackers,
            containers.iter().map(|container| container.id.clone()),
        );

        for container in containers {
            let id = container.id.clone();

            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.container_trackers.entry(id.clone())
            {
                debug!("Adding container tracker {id}");
                entry.insert(ContainerTracker::new(container, self.metrics.container()));
            }
        }

        let update_results = futures::future::join_all(
            self.container_trackers
                .values()
                .map(|tracker| tracker.update(&self.docker)),
        )
        .await;

        update_results
            .into_iter()
            .fold(0, |failures, result| match result {
                Ok(()) => failures,
                Err(error) => {
                    error!("{error}");
                    failures + 1
                }
            })
    }

    fn sync_volume_trackers(&mut self, volumes: Vec<docker::Volume>) {
        retain_active(
            &mut self.volume_trackers,
            volumes.iter().map(|volume| volume.name.clone()),
        );

        for volume in volumes {
            let name = volume.name.clone();

            if let Some(tracker) = self.volume_trackers.get(&name) {
                tracker.update(&volume);
                continue;
            }

            debug!("Adding volume tracker {name}");
            self.volume_trackers
                .insert(name, VolumeTracker::new(volume, self.metrics.volume()));
        }
    }

    fn sync_image_trackers(&mut self, images: Vec<docker::Image>) {
        retain_active(
            &mut self.image_trackers,
            images.iter().map(|image| image.id.clone()),
        );

        for image in images {
            let id = image.id.clone();

            if let Some(tracker) = self.image_trackers.get(&id) {
                tracker.update(&image);
                continue;
            }

            debug!("Adding image tracker {id}");
            self.image_trackers
                .insert(id, ImageTracker::new(image, self.metrics.image()));
        }
    }
}
