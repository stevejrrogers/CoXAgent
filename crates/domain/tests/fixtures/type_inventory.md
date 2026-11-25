# Type inventory — public serde DTO/domain types (CXA-B286 part 1/3)

Every public `Serialize + Deserialize` type in the domain layer
(`crates/domain/src`), grouped by module, with its round-trip test status.
Mirrored by `crates/domain/tests/dto_serde_round_trip_b254.rs::DOMAIN_SERDE_TYPES`,
which fails when this file drifts. Parts 2–3 of CXA-B254 flip `pending → covered`.

Count: **32** domain types (`coxagent-contracts`: BusEnvelope, JobSpec — tracked for part 2).

| Module | Type | Test status |
|---|---|---|
| artifact | `artifact::ArtifactVersion` | pending |
| coverage | `coverage::CoverageStatus` | pending |
| coverage | `coverage::CoverageEntry` | pending |
| debt | `debt::DebtSignalKind` | pending |
| debt | `debt::DebtSignal` | pending |
| events | `events::EventKind` | pending |
| events | `events::DomainEvent` | pending |
| goal | `goal::GoalStatus` | pending |
| goal | `goal::Goal` | pending |
| ids | `ids::TicketId` | pending |
| ids | `ids::GoalId` | pending |
| ids | `ids::WorkerId` | pending |
| kinds | `kinds::TicketType` | pending |
| kinds | `kinds::Priority` | pending |
| kinds | `kinds::Complexity` | pending |
| kinds | `kinds::InterventionKind` | pending |
| kinds | `kinds::Status` | pending |
| kinds | `kinds::Role` | pending |
| panel_terminal_state | `panel_terminal_state::EmptyReason` | pending |
| panel_terminal_state | `panel_terminal_state::FillHint` | pending |
| test_case | `test_case::TestCaseStatus` | pending |
| test_case | `test_case::RecordKind` | pending |
| test_case | `test_case::CaseEvidence` | pending |
| test_case | `test_case::VerdictRecord` | pending |
| test_case | `test_case::TestCase` | pending |
| ticket | `ticket::TechnicalDesign` | pending |
| ticket | `ticket::UxDesign` | pending |
| ticket | `ticket::Design` | pending |
| ticket | `ticket::Ticket` | pending |
| version | `version::Bump` | pending |
| version | `version::SemVer` | pending |
| wip_checkpoint | `wip_checkpoint::WipCheckpoint` | pending |
