pub mod analyze;
pub mod coexist;
pub mod detect;
pub mod loader;
pub mod schema;
pub mod shadow;
pub mod state;
pub mod switch;

pub use analyze::{
    analyze, format_plain, ConflictFinding, OverlapAnalysis, ToolAssessment, ToolState, UserChoice,
};
pub use coexist::{
    build_plan, has_foreign_marker, has_foreign_voice_rule, should_skip_command, CoexistencePlan,
    KNOWN_FOREIGN_MARKERS,
};
pub use detect::{
    run_verify_with_timeout, scan, CompanionTool, DetectedTool, Evidence, EvidenceKind, ScanEnv,
    ScanReport, VERIFY_TIMEOUT,
};
pub use loader::{
    load_builtin, load_with_signed_dir, HmacVerifier, LoadedProfiles, SignatureEnvelope,
    SignatureVerifier,
};
pub use schema::{
    validate_profile, Capabilities, CoexistSupport, ConflictRule, ConflictVerdict, OverlapDetect,
    OverlapLayer, OverlapProfile, PackageSpec, SourcedClaim, ValidationIssue, COMPANION_MCP_NAMES,
};
pub use shadow::{
    append_sample, format_shadow_report, load_samples, shadow_evaluate, summarize, ShadowSample,
    ShadowSummary, MIN_SHADOW_SAMPLES,
};
pub use state::{action_layer, apply_with_yield, ChoiceRecord, OverlapState};
pub use switch::{
    apply_disable_plan, plan_disable_overlapping, restore_tool, PlannedFileEdit, PlannedSwitch,
    RestoreReport, SwitchOutcome, MAX_REPAIRS,
};
