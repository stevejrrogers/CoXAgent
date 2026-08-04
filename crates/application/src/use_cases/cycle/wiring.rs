// Part of the cycle module split by concern — see cycle/mod.rs.
#![allow(clippy::wildcard_imports)]
//! How the cycle wires its agent use cases: one builder per role, each
//! handing the sub-use-case the same ports, worker identity and context.

use super::*;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    pub(super) fn ba(&self) -> RunBaUseCase<S, E> {
        RunBaUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
            self.context.clone(),
        )
        .with_files(self.files.clone())
    }

    pub(super) fn sa(&self) -> RunSaUseCase<S, E> {
        RunSaUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
        .with_files(self.files.clone())
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_context(Some(self.context.clone()))
    }

    pub(super) fn milestones(&self) -> crate::use_cases::RunMilestonesUseCase<S, E> {
        crate::use_cases::RunMilestonesUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
            self.context.clone(),
        )
        .with_files(self.files.clone())
    }

    pub(super) fn design_system(&self) -> RunDesignSystemUseCase<S, E> {
        RunDesignSystemUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
    }

    pub(super) fn pd(&self) -> RunPdUseCase<S, E> {
        RunPdUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
        .with_files(self.files.clone())
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_context(Some(self.context.clone()))
    }

    pub(super) fn dev(&self, mode: DevMode) -> RunDevUseCase<S, E> {
        RunDevUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
            mode,
        )
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_verify(self.deploy.clone())
        .with_git(self.git.clone())
        .with_files(self.files.clone())
        .with_context(Some(self.context.clone()))
    }

    pub(super) fn test(&self) -> RunTestUseCase<S, E> {
        RunTestUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
        .with_files(self.files.clone())
        .with_context(Some(self.context.clone()))
    }

    pub(super) fn docs(&self) -> RunDocsUseCase<S, E> {
        RunDocsUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
        .with_files(self.files.clone())
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_git(self.git.clone())
        .with_context(Some(self.context.clone()))
    }

    pub(super) fn conformance(&self) -> RunConformanceUseCase<S> {
        RunConformanceUseCase::new(
            Arc::clone(&self.store),
            self.work_dir.clone(),
            self.config.architecture.clone(),
        )
        .with_files(self.files.clone())
    }

    /// The coverage-gap detection step (CXA-F007) — runs after DEV ships so the
    /// team learns where its untested code lives and the BA gets structured
    /// data to propose test-improvement chores.
    pub(super) fn coverage(&self) -> crate::use_cases::RunCoverageUseCase<S> {
        crate::use_cases::RunCoverageUseCase::new(
            Arc::clone(&self.store),
            self.work_dir.clone(),
            self.config.clone(),
        )
        .with_files(self.files.clone())
    }
}
