use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use prism_llm::ToolDefinition;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateManifest {
    pub schema_version: u32,
    pub catalog: CandidateCatalog,
    pub ordered_names: Vec<String>,
    pub selection_cardinality: usize,
    pub max_final_prompt_tokens: usize,
    pub expected_definitions_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateCatalog {
    pub source: String,
    pub local_node_online: bool,
}

#[derive(Debug)]
pub struct CandidatePool {
    pub manifest: CandidateManifest,
    pub definitions: Vec<ToolDefinition>,
    pub definitions_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrieverCase {
    pub id: String,
    pub user: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseManifest {
    schema_version: u32,
    cases: Vec<RetrieverCase>,
}

pub fn load_cases(raw: &str) -> Result<Vec<RetrieverCase>> {
    let fixture: CaseManifest =
        serde_json::from_str(raw).context("invalid J-space case fixture JSON")?;
    ensure!(
        fixture.schema_version == 1,
        "unsupported J-space case fixture schema {}",
        fixture.schema_version
    );
    ensure!(!fixture.cases.is_empty(), "J-space case fixture is empty");

    let mut ids = BTreeSet::new();
    for case in &fixture.cases {
        ensure!(!case.id.trim().is_empty(), "case id must not be blank");
        ensure!(
            !case.user.trim().is_empty(),
            "case {} has a blank retriever query",
            case.id
        );
        ensure!(
            ids.insert(case.id.as_str()),
            "duplicate case id {}",
            case.id
        );
    }
    Ok(fixture.cases)
}

pub fn load_candidate_pool(raw: &str) -> Result<CandidatePool> {
    let manifest: CandidateManifest =
        serde_json::from_str(raw).context("invalid J-space candidate fixture JSON")?;
    ensure!(
        manifest.schema_version == 1,
        "unsupported J-space candidate fixture schema {}",
        manifest.schema_version
    );
    ensure!(
        manifest.catalog.source == "command_tools_filtered",
        "unsupported candidate catalog source {:?}",
        manifest.catalog.source
    );
    ensure!(
        !manifest.ordered_names.is_empty(),
        "candidate fixture has no ordered names"
    );
    ensure!(
        manifest.selection_cardinality > 0
            && manifest.selection_cardinality < manifest.ordered_names.len(),
        "selection_cardinality must be between one and candidate_count - 1"
    );

    let mut unique_names = BTreeSet::new();
    for name in &manifest.ordered_names {
        ensure!(
            unique_names.insert(name.as_str()),
            "duplicate candidate name {name}"
        );
    }

    let available =
        prism_agent::command_tools::command_tools_filtered(manifest.catalog.local_node_online);
    let mut definitions = Vec::with_capacity(manifest.ordered_names.len());
    for name in &manifest.ordered_names {
        let loaded = available
            .iter()
            .find(|tool| tool.name == *name)
            .with_context(|| {
                format!("candidate {name:?} is absent from the command-tool surface")
            })?;
        definitions.push(loaded.to_definition());
    }

    let definitions_sha256 = ordered_definition_digest(&definitions)?;
    ensure!(
        definitions_sha256.eq_ignore_ascii_case(&manifest.expected_definitions_sha256),
        "ordered full-definition digest changed: fixture={} actual={definitions_sha256}",
        manifest.expected_definitions_sha256
    );

    Ok(CandidatePool {
        manifest,
        definitions,
        definitions_sha256,
    })
}

pub fn ordered_definition_digest(definitions: &[ToolDefinition]) -> Result<String> {
    let bytes = serde_json::to_vec(definitions)
        .context("could not serialize ordered full tool definitions")?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Counts tokens in the complete rendered prompt for one candidate set.
///
/// The production implementation will use the GGUF's own chat template and
/// tokenizer. Unit tests use a deterministic fake so the equality algorithm
/// remains active without model files.
pub trait FinalPromptTokenCounter {
    fn final_prompt_tokens(&self, case: &RetrieverCase, tools: &[ToolDefinition]) -> Result<usize>;
}

#[cfg(feature = "local-inference")]
#[async_trait::async_trait]
pub trait AsyncFinalPromptTokenCounter {
    async fn final_prompt_tokens(
        &self,
        case: &RetrieverCase,
        tools: &[ToolDefinition],
    ) -> Result<usize>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTokenBucket {
    pub final_prompt_tokens: usize,
    pub candidate_count: usize,
    pub subsets: Vec<Vec<usize>>,
}

/// Find the exact-token bucket with the most distinct fixed-cardinality
/// candidate subsets. The target is chosen before either scorer runs and
/// without evaluator truth, so both arms compete inside the same feasible
/// final-prompt budget.
pub fn build_exact_token_bucket(
    case: &RetrieverCase,
    candidates: &[ToolDefinition],
    fixed_tools: &[ToolDefinition],
    selection_cardinality: usize,
    max_final_prompt_tokens: usize,
    counter: &dyn FinalPromptTokenCounter,
) -> Result<ExactTokenBucket> {
    ensure!(
        candidates.len() <= 20,
        "exact-token subset enumeration is bounded to 20 candidates"
    );
    ensure!(
        selection_cardinality > 0 && selection_cardinality < candidates.len(),
        "invalid exact-token selection cardinality {selection_cardinality} for {} candidates",
        candidates.len()
    );

    let subset_limit = 1usize
        .checked_shl(u32::try_from(candidates.len()).context("candidate count exceeds u32")?)
        .context("candidate subset count overflows usize")?;
    let mut buckets: BTreeMap<usize, Vec<Vec<usize>>> = BTreeMap::new();

    for mask in 0..subset_limit {
        if mask.count_ones() as usize != selection_cardinality {
            continue;
        }
        let subset: Vec<usize> = (0..candidates.len())
            .filter(|index| mask & (1usize << index) != 0)
            .collect();
        let tools = materialize_tool_set(candidates, fixed_tools, &subset)?;
        let tokens = counter.final_prompt_tokens(case, &tools)?;
        if tokens <= max_final_prompt_tokens {
            buckets.entry(tokens).or_default().push(subset);
        }
    }

    let (final_prompt_tokens, mut subsets) = buckets
        .into_iter()
        .filter(|(_, subsets)| subsets.len() >= 2)
        .max_by(
            |(left_tokens, left_subsets), (right_tokens, right_subsets)| {
                left_subsets
                    .len()
                    .cmp(&right_subsets.len())
                    .then_with(|| left_tokens.cmp(right_tokens))
            },
        )
        .context(
            "no exact final-token bucket contains two competing candidate subsets under the cap",
        )?;
    subsets.sort_unstable();

    Ok(ExactTokenBucket {
        final_prompt_tokens,
        candidate_count: candidates.len(),
        subsets,
    })
}

/// Async counterpart used by the ignored GGUF benchmark. Every subset is
/// rendered and tokenized as a complete production prompt; candidate token
/// estimates are never added together.
#[cfg(feature = "local-inference")]
pub async fn build_exact_token_bucket_async(
    case: &RetrieverCase,
    candidates: &[ToolDefinition],
    fixed_tools: &[ToolDefinition],
    selection_cardinality: usize,
    max_final_prompt_tokens: usize,
    counter: &dyn AsyncFinalPromptTokenCounter,
) -> Result<ExactTokenBucket> {
    ensure!(
        candidates.len() <= 20,
        "exact-token subset enumeration is bounded to 20 candidates"
    );
    ensure!(
        selection_cardinality > 0 && selection_cardinality < candidates.len(),
        "invalid exact-token selection cardinality {selection_cardinality} for {} candidates",
        candidates.len()
    );

    let subset_limit = 1usize
        .checked_shl(u32::try_from(candidates.len()).context("candidate count exceeds u32")?)
        .context("candidate subset count overflows usize")?;
    let mut buckets: BTreeMap<usize, Vec<Vec<usize>>> = BTreeMap::new();

    for mask in 0..subset_limit {
        if mask.count_ones() as usize != selection_cardinality {
            continue;
        }
        let subset: Vec<usize> = (0..candidates.len())
            .filter(|index| mask & (1usize << index) != 0)
            .collect();
        let tools = materialize_tool_set(candidates, fixed_tools, &subset)?;
        let tokens = counter.final_prompt_tokens(case, &tools).await?;
        if tokens <= max_final_prompt_tokens {
            buckets.entry(tokens).or_default().push(subset);
        }
    }

    let (final_prompt_tokens, mut subsets) = buckets
        .into_iter()
        .filter(|(_, subsets)| subsets.len() >= 2)
        .max_by(
            |(left_tokens, left_subsets), (right_tokens, right_subsets)| {
                left_subsets
                    .len()
                    .cmp(&right_subsets.len())
                    .then_with(|| left_tokens.cmp(right_tokens))
            },
        )
        .context(
            "no exact final-token bucket contains two competing candidate subsets under the cap",
        )?;
    subsets.sort_unstable();

    Ok(ExactTokenBucket {
        final_prompt_tokens,
        candidate_count: candidates.len(),
        subsets,
    })
}

impl ExactTokenBucket {
    /// Choose the feasible subset with the lexicographically best rank
    /// positions. Candidates are still returned in canonical pool order, so
    /// ranking does not introduce an insertion-order confound.
    pub fn select_for_ranking(&self, ranking: &[usize]) -> Result<Vec<usize>> {
        ensure!(
            ranking.len() == self.candidate_count,
            "ranking has {} entries for {} candidates",
            ranking.len(),
            self.candidate_count
        );
        let mut rank_positions = vec![usize::MAX; self.candidate_count];
        for (position, &candidate) in ranking.iter().enumerate() {
            ensure!(
                candidate < self.candidate_count,
                "ranking contains out-of-range candidate {candidate}"
            );
            ensure!(
                rank_positions[candidate] == usize::MAX,
                "ranking repeats candidate {candidate}"
            );
            rank_positions[candidate] = position;
        }

        let mut best: Option<(Vec<usize>, Vec<usize>)> = None;
        for subset in &self.subsets {
            let mut signature: Vec<usize> = subset
                .iter()
                .map(|candidate| rank_positions[*candidate])
                .collect();
            signature.sort_unstable();
            let replace = best.as_ref().is_none_or(|(best_signature, best_subset)| {
                signature < *best_signature
                    || (signature == *best_signature && subset < best_subset)
            });
            if replace {
                best = Some((signature, subset.clone()));
            }
        }
        best.map(|(_, subset)| subset)
            .context("exact-token bucket contains no subsets")
    }
}

pub fn materialize_tool_set(
    candidates: &[ToolDefinition],
    fixed_tools: &[ToolDefinition],
    subset: &[usize],
) -> Result<Vec<ToolDefinition>> {
    let mut previous = None;
    let mut tools = Vec::with_capacity(subset.len() + fixed_tools.len());
    tools.extend(fixed_tools.iter().cloned());
    for &index in subset {
        ensure!(
            index < candidates.len(),
            "candidate index {index} is out of range"
        );
        if let Some(previous) = previous {
            ensure!(
                index > previous,
                "selected candidate indices must be in canonical order"
            );
        }
        previous = Some(index);
        tools.push(candidates[index].clone());
    }
    Ok(tools)
}

pub mod evaluator {
    use super::*;
    use std::time::Duration;

    use serde_json::Value;

    #[derive(Debug, Clone)]
    pub struct ObservedCall {
        pub name: String,
        /// `None` records a call whose arguments could not be parsed as JSON.
        pub arguments: Option<Value>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DeterminismControls {
        pub gguf_sha256: String,
        pub template_sha256: String,
        /// Digest of the full candidate definitions in their canonical order.
        pub candidate_definitions_sha256: String,
        pub greedy: bool,
        /// Exactly one load/warm-up precedes all timed fixture runs.
        pub warm_load_count: usize,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct TokenCounts {
        pub scorer_input: usize,
        pub final_prompt_prefill: usize,
        pub generated_decode: usize,
    }

    impl TokenCounts {
        fn add(&mut self, other: Self) {
            self.scorer_input += other.scorer_input;
            self.final_prompt_prefill += other.final_prompt_prefill;
            self.generated_decode += other.generated_decode;
        }
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct PhaseTimings {
        pub scoring: Duration,
        pub prefill: Duration,
        pub decode: Duration,
    }

    impl PhaseTimings {
        fn add(&mut self, other: Self) {
            self.scoring += other.scoring;
            self.prefill += other.prefill;
            self.decode += other.decode;
        }
    }

    #[derive(Debug, Clone)]
    pub struct FixtureRun {
        pub case_id: String,
        pub calls: Vec<ObservedCall>,
        pub controls: DeterminismControls,
        pub tokens: TokenCounts,
        pub timings: PhaseTimings,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct FieldCounts {
        pub true_positive: usize,
        pub false_positive: usize,
        pub false_negative: usize,
    }

    impl FieldCounts {
        pub fn precision(self) -> f64 {
            ratio(self.true_positive, self.true_positive + self.false_positive)
        }

        pub fn recall(self) -> f64 {
            ratio(self.true_positive, self.true_positive + self.false_negative)
        }

        pub fn f1(self) -> f64 {
            ratio(
                2 * self.true_positive,
                2 * self.true_positive + self.false_positive + self.false_negative,
            )
        }

        fn add(&mut self, other: Self) {
            self.true_positive += other.true_positive;
            self.false_positive += other.false_positive;
            self.false_negative += other.false_negative;
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ArmSummary {
        pub total: usize,
        pub passed: usize,
        pub failed: usize,
        pub fields: FieldCounts,
        pub tokens: TokenCounts,
        pub timings: PhaseTimings,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct PairedCounts {
        pub both_pass: usize,
        pub influence_only: usize,
        pub cosine_only: usize,
        pub both_fail: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CaseEvaluation {
        pub case_id: String,
        pub cosine_pass: bool,
        pub influence_pass: bool,
        pub cosine_fields: FieldCounts,
        pub influence_fields: FieldCounts,
        pub cosine_tokens: TokenCounts,
        pub influence_tokens: TokenCounts,
        pub cosine_timings: PhaseTimings,
        pub influence_timings: PhaseTimings,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BenchmarkEvaluation {
        pub cosine: ArmSummary,
        pub influence: ArmSummary,
        pub paired: PairedCounts,
        pub cases: Vec<CaseEvaluation>,
        pub controls_verified: bool,
        pub influence_wins: bool,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TruthManifest {
        schema_version: u32,
        cases: Vec<TruthCase>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TruthCase {
        id: String,
        expected: ExpectedCall,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ExpectedCall {
        name: String,
        arguments: Value,
    }

    struct ScoredRun {
        exact_pass: bool,
        fields: FieldCounts,
    }

    /// Load evaluator-only truth and compare already-completed arm runs.
    /// Retriever cases and ranking APIs have no truth-bearing type in their
    /// signatures; this is the sole truth loader in the scaffold.
    pub fn evaluate_benchmark(
        truth_raw: &str,
        candidate_names: &BTreeSet<String>,
        cosine_runs: &[FixtureRun],
        influence_runs: &[FixtureRun],
    ) -> Result<BenchmarkEvaluation> {
        let truth: TruthManifest =
            serde_json::from_str(truth_raw).context("invalid evaluator truth JSON")?;
        ensure!(
            truth.schema_version == 1,
            "unsupported evaluator truth schema {}",
            truth.schema_version
        );
        ensure!(!truth.cases.is_empty(), "evaluator truth is empty");

        let cosine = runs_by_id(cosine_runs, "cosine")?;
        let influence = runs_by_id(influence_runs, "influence")?;
        ensure!(
            cosine.len() == truth.cases.len(),
            "cosine produced {} runs for {} truth cases",
            cosine.len(),
            truth.cases.len()
        );
        ensure!(
            influence.len() == truth.cases.len(),
            "influence produced {} runs for {} truth cases",
            influence.len(),
            truth.cases.len()
        );
        validate_determinism_controls(cosine_runs, influence_runs)?;

        let mut truth_ids = BTreeSet::new();
        let mut cosine_summary = ArmSummary {
            total: truth.cases.len(),
            passed: 0,
            failed: 0,
            fields: FieldCounts::default(),
            tokens: TokenCounts::default(),
            timings: PhaseTimings::default(),
        };
        let mut influence_summary = cosine_summary.clone();
        let mut paired = PairedCounts::default();
        let mut cases = Vec::with_capacity(truth.cases.len());

        for case in truth.cases {
            ensure!(
                truth_ids.insert(case.id.clone()),
                "duplicate truth id {}",
                case.id
            );
            ensure!(
                candidate_names.contains(&case.expected.name),
                "truth for {} names non-candidate tool {}",
                case.id,
                case.expected.name
            );
            ensure!(
                case.expected.arguments.is_object(),
                "truth arguments for {} must be a JSON object",
                case.id
            );
            let cosine_run = cosine
                .get(case.id.as_str())
                .with_context(|| format!("cosine is missing case {}", case.id))?;
            let influence_run = influence
                .get(case.id.as_str())
                .with_context(|| format!("influence is missing case {}", case.id))?;
            ensure!(
                cosine_run.tokens.final_prompt_prefill == influence_run.tokens.final_prompt_prefill,
                "case {} has unequal final rendered prompt tokens: cosine={} influence={}",
                case.id,
                cosine_run.tokens.final_prompt_prefill,
                influence_run.tokens.final_prompt_prefill
            );
            let cosine_score = score_run(cosine_run, &case.expected);
            let influence_score = score_run(influence_run, &case.expected);

            record_summary(&mut cosine_summary, &cosine_score, cosine_run);
            record_summary(&mut influence_summary, &influence_score, influence_run);
            match (cosine_score.exact_pass, influence_score.exact_pass) {
                (true, true) => paired.both_pass += 1,
                (false, true) => paired.influence_only += 1,
                (true, false) => paired.cosine_only += 1,
                (false, false) => paired.both_fail += 1,
            }
            cases.push(CaseEvaluation {
                case_id: case.id,
                cosine_pass: cosine_score.exact_pass,
                influence_pass: influence_score.exact_pass,
                cosine_fields: cosine_score.fields,
                influence_fields: influence_score.fields,
                cosine_tokens: cosine_run.tokens,
                influence_tokens: influence_run.tokens,
                cosine_timings: cosine_run.timings,
                influence_timings: influence_run.timings,
            });
        }

        ensure!(
            cosine.keys().all(|id| truth_ids.contains(*id)),
            "cosine contains a case absent from evaluator truth"
        );
        ensure!(
            influence.keys().all(|id| truth_ids.contains(*id)),
            "influence contains a case absent from evaluator truth"
        );

        let controls_verified = true;
        let influence_wins = strict_win_gate(
            &cosine_summary,
            &influence_summary,
            paired,
            controls_verified,
        );
        Ok(BenchmarkEvaluation {
            cosine: cosine_summary,
            influence: influence_summary,
            paired,
            cases,
            controls_verified,
            influence_wins,
        })
    }

    pub fn strict_win_gate(
        cosine: &ArmSummary,
        influence: &ArmSummary,
        paired: PairedCounts,
        controls_verified: bool,
    ) -> bool {
        controls_verified
            && influence.passed > cosine.passed
            && paired.influence_only > paired.cosine_only
            && f1_no_worse(influence.fields, cosine.fields)
    }

    fn validate_determinism_controls(
        cosine_runs: &[FixtureRun],
        influence_runs: &[FixtureRun],
    ) -> Result<()> {
        let reference = cosine_runs
            .first()
            .context("cosine has no run controls")?
            .controls
            .clone();
        for (label, digest) in [
            ("GGUF", reference.gguf_sha256.as_str()),
            ("template", reference.template_sha256.as_str()),
            (
                "candidate definitions",
                reference.candidate_definitions_sha256.as_str(),
            ),
        ] {
            ensure!(
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{label} digest must be a 64-character SHA-256 hex string"
            );
        }
        ensure!(reference.greedy, "benchmark sampling must be greedy");
        ensure!(
            reference.warm_load_count == 1,
            "benchmark requires exactly one load/warm-up before timed runs"
        );

        for (arm, runs) in [("cosine", cosine_runs), ("influence", influence_runs)] {
            for run in runs {
                ensure!(
                    run.controls == reference,
                    "{arm} case {} changed model, template, candidate order, sampling, or warm-up controls",
                    run.case_id
                );
                ensure!(
                    run.tokens.final_prompt_prefill > 0,
                    "{arm} case {} did not record final rendered prompt tokens",
                    run.case_id
                );
            }
        }
        Ok(())
    }

    fn runs_by_id<'a>(
        runs: &'a [FixtureRun],
        arm: &str,
    ) -> Result<BTreeMap<&'a str, &'a FixtureRun>> {
        let mut by_id = BTreeMap::new();
        for run in runs {
            if by_id.insert(run.case_id.as_str(), run).is_some() {
                bail!("{arm} repeats case {}", run.case_id);
            }
        }
        Ok(by_id)
    }

    fn score_run(run: &FixtureRun, expected: &ExpectedCall) -> ScoredRun {
        let exact_pass = run.calls.len() == 1
            && run.calls[0].name == expected.name
            && run.calls[0].arguments.as_ref() == Some(&expected.arguments);
        let expected_atoms = call_atoms(&expected.name, Some(&expected.arguments), 0);
        let mut predicted_atoms = BTreeSet::new();
        for (ordinal, call) in run.calls.iter().enumerate() {
            predicted_atoms.extend(call_atoms(&call.name, call.arguments.as_ref(), ordinal));
        }
        let fields = FieldCounts {
            true_positive: predicted_atoms.intersection(&expected_atoms).count(),
            false_positive: predicted_atoms.difference(&expected_atoms).count(),
            false_negative: expected_atoms.difference(&predicted_atoms).count(),
        };
        ScoredRun { exact_pass, fields }
    }

    fn call_atoms(name: &str, arguments: Option<&Value>, ordinal: usize) -> BTreeSet<String> {
        let encoded_name = serde_json::to_string(name).expect("a string always serializes");
        let mut atoms = BTreeSet::new();
        atoms.insert(format!("call[{ordinal}].name={encoded_name}"));
        if let Some(arguments) = arguments {
            flatten_value(
                &format!("call[{ordinal}:{encoded_name}].arguments"),
                arguments,
                &mut atoms,
            );
        }
        atoms
    }

    fn flatten_value(prefix: &str, value: &Value, atoms: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) if object.is_empty() => {
                atoms.insert(format!("{prefix}={{}}"));
            }
            Value::Object(object) => {
                for (key, child) in object {
                    let encoded_key =
                        serde_json::to_string(key).expect("an object key always serializes");
                    flatten_value(&format!("{prefix}[{encoded_key}]"), child, atoms);
                }
            }
            Value::Array(array) if array.is_empty() => {
                atoms.insert(format!("{prefix}=[]"));
            }
            Value::Array(array) => {
                for (index, child) in array.iter().enumerate() {
                    flatten_value(&format!("{prefix}[{index}]"), child, atoms);
                }
            }
            scalar => {
                atoms.insert(format!("{prefix}={scalar}"));
            }
        }
    }

    fn record_summary(summary: &mut ArmSummary, score: &ScoredRun, run: &FixtureRun) {
        if score.exact_pass {
            summary.passed += 1;
        } else {
            summary.failed += 1;
        }
        summary.fields.add(score.fields);
        summary.tokens.add(run.tokens);
        summary.timings.add(run.timings);
    }

    fn ratio(numerator: usize, denominator: usize) -> f64 {
        if denominator == 0 {
            0.0
        } else {
            numerator as f64 / denominator as f64
        }
    }

    fn f1_no_worse(left: FieldCounts, right: FieldCounts) -> bool {
        let left_numerator = 2_u128 * left.true_positive as u128;
        let left_denominator =
            left_numerator + left.false_positive as u128 + left.false_negative as u128;
        let right_numerator = 2_u128 * right.true_positive as u128;
        let right_denominator =
            right_numerator + right.false_positive as u128 + right.false_negative as u128;
        match (left_denominator, right_denominator) {
            (0, 0) => true,
            (0, _) => false,
            (_, 0) => true,
            _ => left_numerator * right_denominator >= right_numerator * left_denominator,
        }
    }
}
