//! R4D-2E — replays the operator preflight corpus through the model and
//! compares every expected surface exhaustively.
//!
//! # How this corpus is produced
//!
//! Nothing in `expected/` is hand-written. This suite reads `actions.jsonl`,
//! drives each row through the crate's real public constructors, **produces**
//! the durable rows and the projection from the model's own answers, and then
//! compares them to the committed files. A behaviour change moves the produced
//! side and the comparison fails; a hand-edited expectation moves the committed
//! side and the comparison fails too.
//!
//! # The baseline row, and why the corpus is written as departures
//!
//! Row zero declares one sound plan, one sound mapping and one sound custody.
//! Every later row names the fields it changes and nothing else, so a refusal is
//! attributable to exactly the departure the row made. A corpus of thirty
//! independently written rows would have thirty chances to refuse for a reason
//! nobody intended, and the reader could not tell which.
//!
//! # Fail-closed reading
//!
//! An unknown action kind, an unknown change field and an unknown enum label are
//! all loud failures. The owner refuses a row it does not understand rather than
//! skipping it: a skipped row is a row that silently stops being evidence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use grapt_service::{
    construct_differential_plan, ArtifactClass, ArtifactCustody, BoundaryKind, ComparisonScope,
    DeclaredArtifact, DifferentialConstruction, DifferentialMatrix, EngineProcessCount,
    EntrypointCapability, EvidenceTier, HostBoundsSource, KillDiscipline, MappingBasis,
    MappingReceipt, MatrixEvidence, NegativeControl, Operation, OperationSubset, OperatorPlan,
    OperatorPlanParts, OperatorRefusal, ParkReason, PhaseGate, ReceiptRoot, RetainedOwnerMapping,
    RetainedSource, RunAuthorization, RunPhase, SelectorState, TierCaps, TimeoutBudget,
};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Corpus location and loud reading.
// ---------------------------------------------------------------------------

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-operator-preflight-contract")
}

fn read(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => panic!(
            "the corpus file {} could not be read ({error}); a missing expected surface is a failure, never a skip",
            path.display()
        ),
    }
}

fn parse(text: &str, what: &str) -> Value {
    match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => panic!("{what} is not valid JSON: {error}"),
    }
}

/// Lines with any trailing carriage return removed.
///
/// The checkout policy for this repository can deliver these files with CRLF
/// endings while the committed bytes are LF. Comparing raw lines would then fail
/// on the newline rather than on the content, which reads as a corpus defect and
/// is not one.
fn lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .collect()
}

fn field<'a>(row: &'a Value, key: &str, ordinal: i64) -> &'a Value {
    match row.get(key) {
        Some(value) => value,
        None => panic!("action {ordinal} has no `{key}` field"),
    }
}

fn text_of(value: &Value, what: &str) -> String {
    match value.as_str() {
        Some(text) => text.to_owned(),
        None => panic!("{what} is not a string"),
    }
}

fn integer_of(value: &Value, what: &str) -> u64 {
    match value.as_u64() {
        Some(number) => number,
        None => panic!("{what} is not a non-negative integer"),
    }
}

fn boolean_of(value: &Value, what: &str) -> bool {
    match value.as_bool() {
        Some(flag) => flag,
        None => panic!("{what} is not a boolean"),
    }
}

// ---------------------------------------------------------------------------
// Label decoding. Every unknown label is a loud failure, never a default.
// ---------------------------------------------------------------------------

fn tier_of(label: &str) -> EvidenceTier {
    match EvidenceTier::from_label(label) {
        Some(tier) => tier,
        None => panic!("`{label}` is not an evidence tier this build knows"),
    }
}

fn entrypoint_of(label: &str) -> EntrypointCapability {
    match EntrypointCapability::from_label(label) {
        Some(entrypoint) => entrypoint,
        None => panic!("`{label}` is not an entrypoint capability this build knows"),
    }
}

fn matrix_of(label: &str) -> DifferentialMatrix {
    match DifferentialMatrix::from_label(label) {
        Some(matrix) => matrix,
        None => panic!("`{label}` is not a matrix this build knows"),
    }
}

fn boundary_of(label: &str) -> BoundaryKind {
    match BoundaryKind::from_label(label) {
        Some(boundary) => boundary,
        None => panic!("`{label}` is not a boundary kind this build knows"),
    }
}

fn comparison_of(label: &str) -> ComparisonScope {
    match ComparisonScope::from_label(label) {
        Some(scope) => scope,
        None => panic!("`{label}` is not a comparison scope this build knows"),
    }
}

fn park_of(label: &str) -> ParkReason {
    match ParkReason::from_label(label) {
        Some(reason) => reason,
        None => panic!("`{label}` is not a park reason this build knows"),
    }
}

fn source_of(label: &str) -> RetainedSource {
    match RetainedSource::from_label(label) {
        Some(source) => source,
        None => panic!("`{label}` is not a retained source this build knows"),
    }
}

fn basis_of(label: &str) -> MappingBasis {
    match MappingBasis::from_label(label) {
        Some(basis) => basis,
        None => panic!("`{label}` is not a mapping basis this build knows"),
    }
}

fn class_of(label: &str) -> ArtifactClass {
    match ArtifactClass::from_label(label) {
        Some(class) => class,
        None => panic!("`{label}` is not an artifact class this build knows"),
    }
}

fn control_of(label: &str) -> NegativeControl {
    match NegativeControl::from_label(label) {
        Some(control) => control,
        None => panic!("`{label}` is not a negative control this build knows"),
    }
}

fn phase_of(label: &str) -> RunPhase {
    match RunPhase::from_label(label) {
        Some(phase) => phase,
        None => panic!("`{label}` is not a run phase this build knows"),
    }
}

// ---------------------------------------------------------------------------
// The three declarable shapes, as mutable specifications.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PlanSpec {
    tier: String,
    authorization: String,
    entrypoint: String,
    matrix: String,
    boundary: String,
    comparison: String,
    engines: u64,
    kill_pid: u64,
    kill_image: Option<String>,
    whole_run_ms: u64,
    per_matrix_ms: u64,
    mapping_present: bool,
    controls: Vec<String>,
}

#[derive(Debug, Clone)]
struct MappingSpec {
    receipt_id: String,
    slot_source: String,
    instance_source: String,
    environment_source: String,
    carries_id: bool,
    pinned: bool,
    basis: String,
}

#[derive(Debug, Clone)]
struct ArtifactSpec {
    name: String,
    class: String,
    bytes: u64,
}

#[derive(Debug, Clone)]
struct CustodySpec {
    repository_root: String,
    receipt_root: String,
    artifacts: Vec<ArtifactSpec>,
}

fn plan_spec_from(value: &Value) -> PlanSpec {
    PlanSpec {
        tier: text_of(field(value, "tier", 0), "baseline plan tier"),
        authorization: text_of(field(value, "authorization", 0), "baseline authorization"),
        entrypoint: text_of(field(value, "entrypoint", 0), "baseline entrypoint"),
        matrix: text_of(field(value, "matrix", 0), "baseline matrix"),
        boundary: text_of(field(value, "boundary", 0), "baseline boundary"),
        comparison: text_of(field(value, "comparison", 0), "baseline comparison"),
        engines: integer_of(field(value, "engines", 0), "baseline engines"),
        kill_pid: integer_of(field(value, "kill_pid", 0), "baseline kill pid"),
        kill_image: None,
        whole_run_ms: integer_of(field(value, "whole_run_ms", 0), "baseline whole run"),
        per_matrix_ms: integer_of(field(value, "per_matrix_ms", 0), "baseline per matrix"),
        mapping_present: boolean_of(field(value, "mapping_present", 0), "baseline mapping flag"),
        controls: string_list(field(value, "controls", 0), "baseline controls"),
    }
}

fn mapping_spec_from(value: &Value) -> MappingSpec {
    MappingSpec {
        receipt_id: text_of(field(value, "receipt_id", 0), "baseline receipt id"),
        slot_source: text_of(field(value, "slot_source", 0), "baseline slot source"),
        instance_source: text_of(
            field(value, "instance_source", 0),
            "baseline instance source",
        ),
        environment_source: text_of(
            field(value, "environment_source", 0),
            "baseline environment source",
        ),
        carries_id: boolean_of(field(value, "carries_id", 0), "baseline carries id"),
        pinned: boolean_of(field(value, "pinned", 0), "baseline pinned"),
        basis: text_of(field(value, "basis", 0), "baseline basis"),
    }
}

fn custody_spec_from(value: &Value) -> CustodySpec {
    CustodySpec {
        repository_root: text_of(
            field(value, "repository_root", 0),
            "baseline repository root",
        ),
        receipt_root: text_of(field(value, "receipt_root", 0), "baseline receipt root"),
        artifacts: artifact_list(field(value, "artifacts", 0)),
    }
}

fn artifact_list(value: &Value) -> Vec<ArtifactSpec> {
    let Some(items) = value.as_array() else {
        panic!("an artifact list is not an array");
    };
    items
        .iter()
        .map(|item| ArtifactSpec {
            name: text_of(field(item, "name", 0), "an artifact name"),
            class: text_of(field(item, "class", 0), "an artifact class"),
            bytes: integer_of(field(item, "bytes", 0), "an artifact byte budget"),
        })
        .collect()
}

fn string_list(value: &Value, what: &str) -> Vec<String> {
    let Some(items) = value.as_array() else {
        panic!("{what} is not an array");
    };
    items.iter().map(|item| text_of(item, what)).collect()
}

/// Apply a row's declared departures, refusing any field the owner cannot apply.
///
/// The catch-all is a panic rather than a skip. A change field nobody applies is
/// a row that silently tests the baseline while claiming to test a departure —
/// the exact shape of a witness that passes for the wrong reason.
fn apply_plan_change(spec: &mut PlanSpec, change: &Map<String, Value>, ordinal: i64) {
    for (key, value) in change {
        match key.as_str() {
            "tier" => spec.tier = text_of(value, "tier"),
            "authorization" => spec.authorization = text_of(value, "authorization"),
            "entrypoint" => spec.entrypoint = text_of(value, "entrypoint"),
            "matrix" => spec.matrix = text_of(value, "matrix"),
            "boundary" => spec.boundary = text_of(value, "boundary"),
            "comparison" => spec.comparison = text_of(value, "comparison"),
            "engines" => spec.engines = integer_of(value, "engines"),
            "kill_pid" => spec.kill_pid = integer_of(value, "kill_pid"),
            "kill_image" => spec.kill_image = Some(text_of(value, "kill_image")),
            "whole_run_ms" => spec.whole_run_ms = integer_of(value, "whole_run_ms"),
            "per_matrix_ms" => spec.per_matrix_ms = integer_of(value, "per_matrix_ms"),
            "mapping_present" => spec.mapping_present = boolean_of(value, "mapping_present"),
            "controls" => spec.controls = string_list(value, "controls"),
            other => panic!(
                "action {ordinal} changes `{other}`, which the plan owner cannot apply; an unapplied change tests the baseline while claiming to test a departure"
            ),
        }
    }
}

fn apply_mapping_change(spec: &mut MappingSpec, change: &Map<String, Value>, ordinal: i64) {
    for (key, value) in change {
        match key.as_str() {
            "receipt_id" => spec.receipt_id = text_of(value, "receipt_id"),
            "slot_source" => spec.slot_source = text_of(value, "slot_source"),
            "instance_source" => spec.instance_source = text_of(value, "instance_source"),
            "environment_source" => spec.environment_source = text_of(value, "environment_source"),
            "carries_id" => spec.carries_id = boolean_of(value, "carries_id"),
            "pinned" => spec.pinned = boolean_of(value, "pinned"),
            "basis" => spec.basis = text_of(value, "basis"),
            other => {
                panic!("action {ordinal} changes `{other}`, which the mapping owner cannot apply")
            }
        }
    }
}

fn apply_custody_change(spec: &mut CustodySpec, change: &Map<String, Value>, ordinal: i64) {
    for (key, value) in change {
        match key.as_str() {
            "repository_root" => spec.repository_root = text_of(value, "repository_root"),
            "receipt_root" => spec.receipt_root = text_of(value, "receipt_root"),
            "artifacts" => spec.artifacts = artifact_list(value),
            other => {
                panic!("action {ordinal} changes `{other}`, which the custody owner cannot apply")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Driving the model.
// ---------------------------------------------------------------------------

fn build_mapping(spec: &MappingSpec) -> Result<RetainedOwnerMapping, OperatorRefusal> {
    RetainedOwnerMapping::declare(MappingReceipt::record(
        spec.receipt_id.clone(),
        source_of(&spec.slot_source),
        source_of(&spec.instance_source),
        source_of(&spec.environment_source),
        spec.carries_id,
        spec.pinned,
        basis_of(&spec.basis),
    ))
}

fn build_custody(spec: &CustodySpec) -> Result<ArtifactCustody, OperatorRefusal> {
    let root = ReceiptRoot::declare(spec.repository_root.clone(), spec.receipt_root.clone())?;
    let mut artifacts = Vec::with_capacity(spec.artifacts.len());
    for artifact in &spec.artifacts {
        artifacts.push(DeclaredArtifact::declare(
            artifact.name.clone(),
            class_of(&artifact.class),
            artifact.bytes,
        )?);
    }
    ArtifactCustody::declare(root, artifacts)
}

/// Declare a plan from its spec, returning the FIRST refusal any component makes.
///
/// The order is the order an operator would actually build them in: custody,
/// then the run's own budgets, then the engine and kill discipline, then the
/// mapping, and only then the plan that relates them. A row that changes a
/// component field is therefore refused by that component, and a row that
/// changes a relation is refused by the plan.
fn build_plan(
    plan: &PlanSpec,
    mapping: &MappingSpec,
    custody: &CustodySpec,
) -> Result<OperatorPlan, OperatorRefusal> {
    let tier = tier_of(&plan.tier);
    let caps = TierCaps::for_tier(tier).unwrap_or(TierCaps::ATTENDED);

    let custody = build_custody(custody)?;
    let timeout = TimeoutBudget::declare(plan.whole_run_ms, plan.per_matrix_ms, caps)?;
    let engines = EngineProcessCount::declare(clamp_u32(plan.engines), caps)?;
    let kill = match &plan.kill_image {
        Some(image) => KillDiscipline::ByImageName(image.clone()).admit()?,
        None => KillDiscipline::PidScoped(clamp_u32(plan.kill_pid)).admit()?,
    };
    let mapping = if plan.mapping_present {
        Some(build_mapping(mapping)?)
    } else {
        None
    };

    OperatorPlan::declare(OperatorPlanParts {
        tier,
        authorization: RunAuthorization::Parked(park_of(&plan.authorization)),
        entrypoint: entrypoint_of(&plan.entrypoint),
        matrix: matrix_of(&plan.matrix),
        boundary: boundary_of(&plan.boundary),
        custody,
        timeout,
        engines,
        kill,
        mapping,
        comparison: comparison_of(&plan.comparison),
        controls: plan
            .controls
            .iter()
            .map(|label| control_of(label))
            .collect(),
    })
}

/// A `u64` from the corpus narrowed to the `u32` the model takes.
///
/// Saturating rather than wrapping: a corpus row carrying an absurd count must
/// stay absurd and be refused by the model, not wrap into a legal one.
fn clamp_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Two admitted engine receipts, built entirely from literals.
fn engine_pair() -> grapt_service::AdmittedDifferential {
    use grapt_service::{EngineSelection, ProbeOutcome, SelectionReceipt};
    use grapt_types::authority::{ClockAuthority, Timestamp};
    use grapt_types::service::{
        Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration,
        ServiceInstanceId, ServiceSlotId,
    };

    #[derive(Debug)]
    struct FixedClock(i64);
    impl ClockAuthority for FixedClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(self.0)
        }
    }

    fn owner(slot: &str, instance: &str, environment: &str, pid: u32, url: &str) -> OwnerReceipt {
        match (
            ServiceSlotId::new(slot),
            ServiceInstanceId::new(instance),
            EnvironmentReceipt::new(environment),
            ProcessId::new(pid),
            Endpoint::new(url),
        ) {
            (Some(slot), Some(instance), Some(environment), Some(pid), Some(endpoint)) => {
                OwnerReceipt::new(
                    slot,
                    instance,
                    environment,
                    ServiceGeneration::FIRST,
                    pid,
                    endpoint,
                )
            }
            _ => panic!("the literal owner coordinates in this corpus owner must all be non-empty"),
        }
    }

    let clock = FixedClock(1_760_000_000_000);
    let left = match SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(owner(
            "slot-rust",
            "instance-rust",
            "env-rust",
            4321,
            "http://127.0.0.1:4321",
        )),
        &clock,
    ) {
        Ok(receipt) => receipt,
        Err(refusal) => panic!("the rust-side corpus receipt must admit, got {refusal}"),
    };
    let right = match SelectionReceipt::admit(
        EngineSelection::Retained,
        ProbeOutcome::Attributed(owner(
            "slot-retained",
            "instance-retained",
            "env-retained",
            8765,
            "http://127.0.0.1:8765",
        )),
        &clock,
    ) {
        Ok(receipt) => receipt,
        Err(refusal) => panic!("the retained-side corpus receipt must admit, got {refusal}"),
    };
    match grapt_service::AdmittedDifferential::admit(left, right) {
        Ok(pair) => pair,
        Err(refusal) => panic!("two distinct engines must admit as a pair, got {refusal}"),
    }
}

// ---------------------------------------------------------------------------
// The replay.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Replayed {
    durable: Vec<Value>,
    refusals: Vec<Value>,
    admitted_by_kind: BTreeMap<String, usize>,
    actions_run: usize,
}

impl Replayed {
    fn admit(&mut self, kind: &str, row: Value) {
        *self.admitted_by_kind.entry(kind.to_owned()).or_insert(0) += 1;
        self.durable.push(row);
    }

    fn refuse(&mut self, ordinal: i64, refusal: &OperatorRefusal) {
        self.refusals.push(json!({
            "ordinal": ordinal,
            "label": refusal.label(),
            "message": refusal.to_string(),
        }));
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn replay() -> Replayed {
    let actions_text = read(&corpus_dir().join("actions.jsonl"));
    let rows: Vec<Value> = lines(&actions_text)
        .into_iter()
        .enumerate()
        .map(|(index, line)| parse(line, &format!("actions.jsonl line {}", index + 1)))
        .collect();

    let Some(baseline) = rows.first() else {
        panic!("actions.jsonl is empty; a corpus with no rows asserts nothing");
    };
    if text_of(field(baseline, "action", 0), "the first action kind") != "baseline" {
        panic!("the first action row must be the baseline every later row departs from");
    }
    let base_plan = plan_spec_from(field(baseline, "plan", 0));
    let base_mapping = mapping_spec_from(field(baseline, "mapping", 0));
    let base_custody = custody_spec_from(field(baseline, "custody", 0));

    let mut out = Replayed::default();

    for row in rows.iter().skip(1) {
        let ordinal = match field(row, "ordinal", -1).as_i64() {
            Some(ordinal) => ordinal,
            None => panic!("an action row has a non-integer ordinal"),
        };
        out.actions_run += 1;
        let kind = text_of(field(row, "action", ordinal), "an action kind");

        match kind.as_str() {
            "plan" => {
                let mut plan = base_plan.clone();
                let mapping = base_mapping.clone();
                let custody = base_custody.clone();
                if let Some(change) = row.get("change").and_then(Value::as_object) {
                    apply_plan_change(&mut plan, change, ordinal);
                }
                match build_plan(&plan, &mapping, &custody) {
                    Ok(admitted) => {
                        let receipts: Vec<Value> = admitted
                            .component_receipts()
                            .iter()
                            .map(|receipt| {
                                json!({
                                    "component": receipt.component(),
                                    "declared": receipt.declared(),
                                })
                            })
                            .collect();
                        out.admit(
                            "plan",
                            json!({
                                "ordinal": ordinal,
                                "kind": "plan",
                                "tier": admitted.tier().label(),
                                "entrypoint": admitted.entrypoint().label(),
                                "matrix": admitted.matrix().label(),
                                "boundary": admitted.boundary().label(),
                                "comparison": admitted.comparison().label(),
                                "component_receipts": receipts,
                            }),
                        );
                    }
                    Err(refusal) => out.refuse(ordinal, &refusal),
                }
            }
            "mapping" => {
                let mut mapping = base_mapping.clone();
                if let Some(change) = row.get("change").and_then(Value::as_object) {
                    apply_mapping_change(&mut mapping, change, ordinal);
                }
                match build_mapping(&mapping) {
                    Ok(admitted) => out.admit(
                        "mapping",
                        json!({
                            "ordinal": ordinal,
                            "kind": "mapping",
                            "receipt_id": admitted.receipt().receipt_id(),
                            "slot_source": admitted.receipt().slot_source().label(),
                            "instance_source": admitted.receipt().instance_source().label(),
                            "environment_source": admitted.receipt().environment_source().label(),
                            "basis": admitted.receipt().basis().label(),
                        }),
                    ),
                    Err(refusal) => out.refuse(ordinal, &refusal),
                }
            }
            "custody" => {
                let mut custody = base_custody.clone();
                if let Some(change) = row.get("change").and_then(Value::as_object) {
                    apply_custody_change(&mut custody, change, ordinal);
                }
                match build_custody(&custody) {
                    Ok(admitted) => out.admit(
                        "custody",
                        json!({
                            "ordinal": ordinal,
                            "kind": "custody",
                            "receipt_root": admitted.root().receipt_root(),
                            "artifact_count": admitted.artifacts().len(),
                            "declared_bytes": admitted.declared_bytes(),
                        }),
                    ),
                    Err(refusal) => out.refuse(ordinal, &refusal),
                }
            }
            "gate" => {
                let phases = string_list(field(row, "phases", ordinal), "a phase list");
                let mut gate = PhaseGate::open(RunAuthorization::Parked(park_of(
                    &base_plan.authorization,
                )));
                let mut refused = false;
                for label in &phases {
                    if let Err(refusal) = gate.enter(phase_of(label)) {
                        out.refuse(ordinal, &refusal);
                        refused = true;
                        break;
                    }
                }
                if !refused {
                    match gate.conclude() {
                        Ok(conclusion) => out.admit(
                            "gate",
                            json!({
                                "ordinal": ordinal,
                                "kind": "gate",
                                "conclusion": conclusion.label(),
                                "entered": gate
                                    .entered()
                                    .iter()
                                    .map(|phase| phase.label())
                                    .collect::<Vec<_>>(),
                            }),
                        ),
                        Err(refusal) => out.refuse(ordinal, &refusal),
                    }
                }
            }
            "construct" => {
                let bounds_label = text_of(field(row, "bounds", ordinal), "a bounds source");
                let bounds = match bounds_label.as_str() {
                    "no-host-has-run" => HostBoundsSource::NoHostHasRun,
                    other => panic!(
                        "action {ordinal} names the bounds source `{other}`; the only source this corpus can express before an entrypoint lands is `no-host-has-run`, because a host-reported recording cannot be fabricated here"
                    ),
                };
                let plan = match build_plan(&base_plan, &base_mapping, &base_custody) {
                    Ok(plan) => plan,
                    Err(refusal) => {
                        panic!("the baseline plan must admit before a construction is attempted, got {refusal}")
                    }
                };
                let subset = match OperationSubset::declare(
                    DifferentialMatrix::Restart,
                    vec![Operation::HealthGet],
                ) {
                    Ok(subset) => subset,
                    Err(refusal) => panic!("the landed restart subset must declare, got {refusal}"),
                };
                match construct_differential_plan(
                    &plan,
                    DifferentialConstruction {
                        bounds,
                        subset,
                        engines: engine_pair(),
                        evidence: MatrixEvidence::None,
                        terminal_selector: SelectorState::Retained,
                    },
                ) {
                    Ok(_) => panic!(
                        "action {ordinal} constructed a differential plan before any host reported its bounds; that is either a fabricated bound or a hole in the landed contract"
                    ),
                    Err(refusal) => out.refuse(ordinal, &refusal),
                }
            }
            other => panic!(
                "action {ordinal} declares the kind `{other}`, which this owner does not know; an unknown kind is refused rather than skipped"
            ),
        }
    }

    out
}

fn projection(replayed: &Replayed) -> Value {
    let mut vocabulary: Vec<String> = replayed
        .refusals
        .iter()
        .filter_map(|row| row.get("label").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    vocabulary.sort();
    vocabulary.dedup();

    json!({
        "actions_run": replayed.actions_run,
        "admitted": replayed.durable.len(),
        "refused": replayed.refusals.len(),
        "admitted_by_kind": replayed.admitted_by_kind,
        "refusals": replayed.refusals,
        "refusal_vocabulary": vocabulary,
        "own_label_count": OperatorRefusal::OWN_LABEL_COUNT,
    })
}

fn canonical(value: &Value) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(text) => text,
        Err(error) => panic!("cannot render a produced surface: {error}"),
    }
}

// ---------------------------------------------------------------------------
// The comparisons.
// ---------------------------------------------------------------------------

#[test]
fn the_corpus_replays_through_the_model_and_matches_every_expected_surface() {
    let produced = replay();

    let expected_durable_text = read(&corpus_dir().join("expected/durable.jsonl"));
    let expected_durable = lines(&expected_durable_text);
    let produced_durable: Vec<String> = produced
        .durable
        .iter()
        .map(|row| match serde_json::to_string(row) {
            Ok(text) => text,
            Err(error) => panic!("cannot render a durable row: {error}"),
        })
        .collect();

    assert_eq!(
        produced_durable.len(),
        expected_durable.len(),
        "the model admitted a different number of durable rows than the corpus records, so one of the two has moved"
    );
    for (index, (produced_row, expected_row)) in produced_durable
        .iter()
        .zip(expected_durable.iter())
        .enumerate()
    {
        assert_eq!(
            produced_row, expected_row,
            "durable row {index} produced by the model differs from the committed row at the same position"
        );
    }

    let expected_state = parse(
        &read(&corpus_dir().join("expected/state.json")),
        "expected/state.json",
    );
    assert_eq!(
        canonical(&projection(&produced)),
        canonical(&expected_state),
        "the projection the model produced differs from the committed one"
    );
}

#[test]
fn the_recorded_vocabulary_is_a_real_subset_of_the_one_this_lane_can_produce() {
    let produced = replay();
    let recorded: Vec<&str> = produced
        .refusals
        .iter()
        .filter_map(|row| row.get("label").and_then(Value::as_str))
        .collect();

    assert!(
        !recorded.is_empty(),
        "a corpus that recorded no refusal at all would not have tested its instrument"
    );
    for label in &recorded {
        let known = OperatorRefusal::OWN_LABELS.contains(label)
            || grapt_service::DifferentialRefusal::ALL_LABELS.contains(label);
        assert!(
            known,
            "the corpus recorded `{label}`, which is in neither this lane's vocabulary nor a landed one -- a word no law produces"
        );
    }
}

#[test]
fn the_waivers_file_is_an_explicit_and_empty_assertion() {
    let waivers = parse(
        &read(&corpus_dir().join("expected/waivers.json")),
        "expected/waivers.json",
    );
    let Some(entries) = waivers.get("waivers").and_then(Value::as_array) else {
        panic!(
            "expected/waivers.json carries no `waivers` array; an absent array is not an empty one"
        );
    };
    assert!(
        entries.is_empty(),
        "this corpus compares the model against itself with no normalisation, so there is nothing a waiver could name"
    );
}
