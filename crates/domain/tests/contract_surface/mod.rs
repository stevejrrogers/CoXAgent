//! CXA-B254 single source of truth for the domain wire-surface contract
//! (shared by [`contract_guard_b287`](../contract_guard_b287.rs) — the guard —
//! and [`contract_docs_b288`](../contract_docs_b288.rs) — the doc generator).
//!
//! Part 3 (CXA-B288) lifted these tables out of the guard test verbatim so the
//! generated documentation reads THE SAME data the guards enforce: doc and
//! guard cannot drift because they are one list, not two.

/// One registered enum: every variant with the wire tag serde produces today.
pub struct EnumContract {
    pub ty: &'static str,
    /// `(Rust variant name, wire tag)` — the full, exhaustive inventory.
    pub variants: &'static [(&'static str, &'static str)],
}

pub const ENUM_SURFACE: &[EnumContract] = &[
    EnumContract {
        ty: "TicketType",
        variants: &[("Feature", "feature"), ("Bug", "bug"), ("Chore", "chore")],
    },
    EnumContract {
        ty: "Status",
        variants: &[
            ("Pending", "pending"),
            ("InProgress", "in_progress"),
            ("OnHold", "on_hold"),
            ("Documented", "documented"),
            ("Verified", "verified"),
        ],
    },
    EnumContract {
        ty: "Role",
        variants: &[
            ("Ba", "ba"),
            ("Po", "po"),
            ("Sa", "sa"),
            ("Sm", "sm"),
            ("DevBug", "dev_bug"),
            ("DevFeature", "dev_feature"),
            ("Reviewer", "reviewer"),
            ("System", "system"),
        ],
    },
    EnumContract {
        ty: "Priority",
        variants: &[("Low", "low"), ("Medium", "medium"), ("High", "high")],
    },
    EnumContract {
        ty: "Complexity",
        variants: &[("Small", "small"), ("Medium", "medium"), ("Large", "large")],
    },
    EnumContract {
        ty: "TestCaseStatus",
        variants: &[("Pending", "pending"), ("Passed", "passed"), ("Failed", "failed")],
    },
    EnumContract {
        ty: "RecordKind",
        variants: &[("Verdict", "verdict"), ("EvidenceRefresh", "evidence_refresh")],
    },
    EnumContract {
        ty: "CoverageStatus",
        variants: &[
            ("Covered", "covered"),
            ("PartiallyCovered", "partially_covered"),
            ("NotTested", "not_tested"),
        ],
    },
    EnumContract {
        ty: "DebtSignalKind",
        variants: &[
            ("LintRegression", "lint_regression"),
            ("DeadCodeSuspects", "dead_code_suspects"),
            ("MissingModuleDoc", "missing_module_doc"),
        ],
    },
    EnumContract {
        ty: "GoalStatus",
        variants: &[("Active", "active"), ("Retired", "retired")],
    },
];

/// Types whose fields are NOT all `#[serde(default)]`: the map value is the
/// exact list of fields a partial payload must carry. THE documented
/// whitelist — anything missing here makes `{}` fail.
pub const REQUIRED_FIELD_WHITELIST: &[(&str, &[&str])] = &[
    ("TestCase", &["description"]),
    ("VerdictRecord", &["status"]), // `at`/kind/note/image all default
    ("CoverageEntry", &["criterion", "status"]),
    ("WipCheckpoint", &["ref_name", "sha", "note", "recorded_at"]),
    ("TechnicalDesign", &["approach", "files", "api_contract", "data_changes", "test_plan"]),
    ("UxDesign", &["user_flow", "screens", "component_states", "responsive_notes"]),
    ("ArtifactVersion", &["project", "artifact", "version"]),
];

/// Types intentionally opting INTO strict decoding (`deny_unknown_fields`).
/// Empty today: the repo-wide policy is tolerant loading. If you flip a DTO
/// to strict, register it here so both guard and doc prove the behaviour.
pub const STRICT_UNKNOWN_KEY_TYPES: &[&str] = &[];

/// Golden wire fixtures checked in under [`fixtures`](../fixtures/) by part 1
/// (CXA-B286), as `(type name, fixture path relative to crates/domain/tests)`.
/// A type absent here simply documents "no golden fixture yet".
pub const GOLDEN_FIXTURES: &[(&str, &str)] =
    &[("ArtifactVersion", "fixtures/artifact_version_cxa_api.json")];
