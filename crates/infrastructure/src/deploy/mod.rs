//! Deploy adapters.

pub mod docker_compose;
pub mod scoped_tests;

pub use docker_compose::DockerComposeDeploy;
