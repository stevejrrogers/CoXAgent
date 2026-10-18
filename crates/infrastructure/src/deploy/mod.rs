//! Deploy adapters.

pub mod docker_compose;
pub mod reclaimable;
pub mod scoped_tests;

pub use docker_compose::{deploy_secrets_root, DockerComposeDeploy};
pub use reclaimable::{
    image_sweep_candidate, orphaned_compose_image, reclaimable_compose_project,
    reclaimable_raw_container, reclaimable_volume,
};
