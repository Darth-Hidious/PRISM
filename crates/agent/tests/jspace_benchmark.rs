// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Weight-free contracts for the controlled cosine-vs-influence benchmark.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

#[path = "support/jspace_benchmark.rs"]
mod benchmark;

use std::collections::BTreeSet;
use std::time::Duration;

use benchmark::evaluator::{
    ArmSummary, DeterminismControls, FieldCounts, FixtureRun, ObservedCall, PairedCounts,
    PhaseTimings, TokenCounts, evaluate_benchmark, strict_win_gate,
};
use benchmark::{
    FinalPromptTokenCounter, RetrieverCase, build_exact_token_bucket, load_candidate_pool,
    load_cases, materialize_tool_set, ordered_definition_digest,
};
use prism_agent::influence::{CandidateInfluence, influence_refusal_reason};
use prism_llm::{ChatResponse, FunctionDef, ToolDefinition};
use serde_json::{Value, json};

const CANDIDATES: &str = include_str!("fixtures/jspace_candidates.json");
const CASES: &str = include_str!("fixtures/jspace_cases.json");
const TRUTH: &str = include_str!("fixtures/jspace_truth.json");

fn controls() -> DeterminismControls {
    DeterminismControls {
        gguf_sha256: "11".repeat(32),
        template_sha256: "22".repeat(32),
        candidate_definitions_sha256: "33".repeat(32),
        greedy: true,
        warm_load_count: 1,
    }
}

fn default_tokens() -> TokenCounts {
    TokenCounts {
        scorer_input: 10,
        final_prompt_prefill: 100,
        generated_decode: 5,
    }
}

fn default_timings() -> PhaseTimings {
    PhaseTimings {
        scoring: Duration::from_millis(1),
        prefill: Duration::from_millis(2),
        decode: Duration::from_millis(3),
    }
}

#[derive(Debug, Clone)]
struct GenerationFailureObservation {
    total_wall_time: Duration,
    error: String,
}

#[derive(Debug)]
struct RecordedFixtureRun {
    run: FixtureRun,
    failure: Option<GenerationFailureObservation>,
}

fn require_discriminative_influence(scores: &[CandidateInfluence]) -> anyhow::Result<()> {
    if let Some(reason) = influence_refusal_reason(scores) {
        anyhow::bail!(reason);
    }
    Ok(())
}

fn measured_success_run(
    case_id: &str,
    response: ChatResponse,
    controls: &DeterminismControls,
    scorer_input: usize,
    scoring: Duration,
    expected_prompt_tokens: usize,
) -> anyhow::Result<FixtureRun> {
    let usage = response
        .usage
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("local GGUF generation did not report exact token usage"))?;
    let metrics = response
        .generation_metrics
        .ok_or_else(|| anyhow::anyhow!("local GGUF generation did not report phase timings"))?;
    let prompt_tokens = usize::try_from(usage.prompt_tokens)
        .map_err(|_| anyhow::anyhow!("local prompt token count exceeds usize"))?;
    anyhow::ensure!(
        prompt_tokens == expected_prompt_tokens,
        "case {case_id} rendered {expected_prompt_tokens} tokens but generation reported {prompt_tokens}"
    );
    let calls = response
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|call| ObservedCall {
            name: call.function.name,
            arguments: serde_json::from_str(&call.function.arguments).ok(),
        })
        .collect();
    Ok(FixtureRun {
        case_id: case_id.to_string(),
        calls,
        controls: controls.clone(),
        tokens: TokenCounts {
            scorer_input,
            final_prompt_prefill: prompt_tokens,
            generated_decode: usize::try_from(usage.completion_tokens)
                .map_err(|_| anyhow::anyhow!("local completion token count exceeds usize"))?,
        },
        timings: PhaseTimings {
            scoring,
            prefill: Duration::from_micros(metrics.prefill_wall_time_micros),
            decode: Duration::from_micros(metrics.decode_wall_time_micros),
        },
    })
}

/// Convert a generation or response-parse error into a failed prediction so
/// one bad fixture cannot erase the paired benchmark's raw denominator.
///
/// The exact pre-rendered prompt size and scorer measurements remain known.
/// Decode tokens and the prefill/decode split are unavailable on the error
/// path, so their numeric accumulator contribution is zero and report output
/// marks those fields `NA` rather than presenting zero as a measurement.
fn record_generation_attempt(
    case_id: &str,
    response: anyhow::Result<ChatResponse>,
    controls: &DeterminismControls,
    scorer_input: usize,
    scoring: Duration,
    expected_prompt_tokens: usize,
    total_wall_time: Duration,
) -> anyhow::Result<RecordedFixtureRun> {
    match response {
        Ok(response) => Ok(RecordedFixtureRun {
            run: measured_success_run(
                case_id,
                response,
                controls,
                scorer_input,
                scoring,
                expected_prompt_tokens,
            )?,
            failure: None,
        }),
        Err(error) => Ok(RecordedFixtureRun {
            run: FixtureRun {
                case_id: case_id.to_string(),
                calls: Vec::new(),
                controls: controls.clone(),
                tokens: TokenCounts {
                    scorer_input,
                    final_prompt_prefill: expected_prompt_tokens,
                    generated_decode: 0,
                },
                timings: PhaseTimings {
                    scoring,
                    prefill: Duration::ZERO,
                    decode: Duration::ZERO,
                },
            },
            failure: Some(GenerationFailureObservation {
                total_wall_time,
                error: format!("{error:#}"),
            }),
        }),
    }
}

fn candidate_names() -> BTreeSet<String> {
    load_candidate_pool(CANDIDATES)
        .expect("candidate fixture must load")
        .manifest
        .ordered_names
        .into_iter()
        .collect()
}

fn no_call(case_id: impl Into<String>) -> FixtureRun {
    FixtureRun {
        case_id: case_id.into(),
        calls: Vec::new(),
        controls: controls(),
        tokens: default_tokens(),
        timings: default_timings(),
    }
}

fn one_call(case_id: impl Into<String>, name: &str, arguments: Value) -> FixtureRun {
    FixtureRun {
        case_id: case_id.into(),
        calls: vec![ObservedCall {
            name: name.to_owned(),
            arguments: Some(arguments),
        }],
        controls: controls(),
        tokens: default_tokens(),
        timings: default_timings(),
    }
}

#[test]
fn retriever_fixtures_do_not_contain_evaluator_labels() {
    let cases = load_cases(CASES).expect("retriever case fixture must load");
    let pool = load_candidate_pool(CANDIDATES).expect("candidate fixture must load");
    assert_eq!(cases.len(), 16);
    assert_eq!(pool.definitions.len(), 16);

    for case in &cases {
        for candidate_name in &pool.manifest.ordered_names {
            assert!(
                !case.user.to_lowercase().contains(candidate_name),
                "{} leaks exact candidate name {candidate_name:?} into retriever input",
                case.id
            );
        }
    }

    let mut leaked: Value = serde_json::from_str(CASES).expect("fixture JSON");
    leaked["cases"][0]["expected"] = json!({"name": "query_local"});
    let leaked_raw = serde_json::to_string(&leaked).expect("mutated fixture serializes");
    let error = load_cases(&leaked_raw).expect_err("unknown truth fields must be rejected");
    assert!(
        error
            .to_string()
            .contains("invalid J-space case fixture JSON")
    );

    // The only API which parses TRUTH is the evaluator. Supplying no calls
    // proves the labels are available for scoring without entering a case or
    // ranking type.
    let empty_runs: Vec<_> = cases.iter().map(|case| no_call(&case.id)).collect();
    let evaluated = evaluate_benchmark(TRUTH, &candidate_names(), &empty_runs, &empty_runs)
        .expect("evaluator-only truth must load");
    assert_eq!(evaluated.cosine.total, 16);
    assert_eq!(evaluated.cosine.passed, 0);
    assert_eq!(evaluated.cosine.failed, 16);
    assert_eq!(evaluated.paired.both_fail, 16);
    assert!(evaluated.cosine.fields.false_negative > 0);
    assert!(!evaluated.influence_wins);
}

#[test]
fn candidate_fixture_locks_ordered_full_definitions() {
    let pool = load_candidate_pool(CANDIDATES).expect("candidate fixture must load");
    let loaded_names: Vec<_> = pool
        .definitions
        .iter()
        .map(|definition| definition.function.name.as_str())
        .collect();
    let fixture_names: Vec<_> = pool
        .manifest
        .ordered_names
        .iter()
        .map(String::as_str)
        .collect();

    assert_eq!(loaded_names, fixture_names);
    assert_eq!(pool.manifest.selection_cardinality, 4);
    assert_eq!(pool.manifest.max_final_prompt_tokens, 8192);
    assert_eq!(
        pool.definitions_sha256,
        pool.manifest.expected_definitions_sha256
    );
    for definition in &pool.definitions {
        assert_eq!(definition.tool_type, "function");
        assert!(!definition.function.description.trim().is_empty());
        assert!(definition.function.parameters.is_object());
    }

    let mut reversed = pool.definitions.clone();
    reversed.reverse();
    assert_ne!(
        ordered_definition_digest(&reversed).expect("reversed definitions serialize"),
        pool.definitions_sha256,
        "the golden digest must cover ordering as well as full definitions"
    );
}

struct FakeFullPromptCounter;

impl FinalPromptTokenCounter for FakeFullPromptCounter {
    fn final_prompt_tokens(
        &self,
        case: &RetrieverCase,
        tools: &[ToolDefinition],
    ) -> anyhow::Result<usize> {
        let tool_tokens = tools
            .iter()
            .map(|tool| match tool.function.name.as_str() {
                "a" => 1,
                "b" => 2,
                "c" => 3,
                "d" => 4,
                "fixed" => 7,
                unknown => panic!("unexpected fake tool {unknown}"),
            })
            .sum::<usize>();
        Ok(10 + case.user.split_whitespace().count() + tool_tokens)
    }
}

fn fake_tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_owned(),
        function: FunctionDef {
            name: name.to_owned(),
            description: format!("Full deterministic definition for {name}"),
            parameters: json!({
                "type": "object",
                "properties": {"marker": {"const": name}}
            }),
        },
    }
}

#[test]
fn subset_selection_competes_only_inside_one_exact_final_token_bucket() {
    let case = RetrieverCase {
        id: "neutral_case".to_owned(),
        user: "one two".to_owned(),
    };
    let candidates = [
        fake_tool("a"),
        fake_tool("b"),
        fake_tool("c"),
        fake_tool("d"),
    ];
    let fixed = [fake_tool("fixed")];
    let bucket =
        build_exact_token_bucket(&case, &candidates, &fixed, 2, 24, &FakeFullPromptCounter)
            .expect("one equal-token bucket exists");

    assert_eq!(bucket.final_prompt_tokens, 24);
    assert_eq!(bucket.subsets, vec![vec![0, 3], vec![1, 2]]);

    let first = bucket
        .select_for_ranking(&[3, 0, 1, 2])
        .expect("complete ranking");
    let second = bucket
        .select_for_ranking(&[1, 2, 0, 3])
        .expect("complete ranking");
    assert_eq!(first, vec![0, 3]);
    assert_eq!(second, vec![1, 2]);

    for subset in [&first, &second] {
        let tools = materialize_tool_set(&candidates, &fixed, subset)
            .expect("canonical subset materializes");
        assert_eq!(tools.first().expect("fixed tool").function.name, "fixed");
        assert_eq!(
            tools[1..]
                .iter()
                .map(|tool| tool.function.name.as_str())
                .collect::<Vec<_>>(),
            subset
                .iter()
                .map(|index| candidates[*index].function.name.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            FakeFullPromptCounter
                .final_prompt_tokens(&case, &tools)
                .expect("fake prompt renders"),
            bucket.final_prompt_tokens
        );
    }

    assert!(bucket.select_for_ranking(&[0, 0, 2, 3]).is_err());
    assert!(materialize_tool_set(&candidates, &fixed, &[3, 0]).is_err());
}

#[test]
fn benchmark_refuses_the_same_no_signal_and_top_tie_vectors_as_production() {
    let influences = |values: &[f64]| {
        values
            .iter()
            .enumerate()
            .map(|(index, score)| CandidateInfluence {
                name: format!("candidate_{index}"),
                score_per_added_token: *score,
            })
            .collect::<Vec<_>>()
    };

    let no_signal = require_discriminative_influence(&influences(&[0.0, 0.0, 0.0]))
        .expect_err("a canonical-order non-result must not enter ranking");
    assert_eq!(no_signal.to_string(), "influence_no_signal");

    let top_tied = require_discriminative_influence(&influences(&[0.8, 0.8, 0.2]))
        .expect_err("a tied top score must not enter ranking");
    assert_eq!(top_tied.to_string(), "influence_top_tied");

    require_discriminative_influence(&influences(&[0.8, 0.7, 0.2]))
        .expect("one unique top score is discriminative");
}

#[test]
fn generation_or_parse_error_is_recorded_as_a_failed_fixture() {
    let recorded = record_generation_attempt(
        "generation_error",
        Err(anyhow::anyhow!("synthetic local response parse failure")),
        &controls(),
        37,
        Duration::from_millis(5),
        100,
        Duration::from_millis(11),
    )
    .expect("a response error must become an observed failed prediction");

    assert!(recorded.run.calls.is_empty());
    assert_eq!(recorded.run.tokens.scorer_input, 37);
    assert_eq!(recorded.run.tokens.final_prompt_prefill, 100);
    assert_eq!(recorded.run.tokens.generated_decode, 0);
    assert_eq!(recorded.run.timings.scoring, Duration::from_millis(5));
    assert_eq!(recorded.run.timings.prefill, Duration::ZERO);
    assert_eq!(recorded.run.timings.decode, Duration::ZERO);
    let failure = recorded
        .failure
        .as_ref()
        .expect("unavailable generation phases must remain explicit");
    assert_eq!(failure.total_wall_time, Duration::from_millis(11));
    assert!(
        failure
            .error
            .contains("synthetic local response parse failure")
    );

    let truth = json!({
        "schema_version": 1,
        "cases": [{
            "id": "generation_error",
            "expected": {"name": "only_tool", "arguments": {}}
        }]
    });
    let cosine = one_call("generation_error", "only_tool", json!({}));
    let result = evaluate_benchmark(
        &serde_json::to_string(&truth).expect("truth serializes"),
        &BTreeSet::from(["only_tool".to_string()]),
        &[cosine],
        &[recorded.run],
    )
    .expect("one failed generation must not erase the paired denominator");
    assert_eq!((result.influence.passed, result.influence.failed), (0, 1));
    assert_eq!(result.paired.cosine_only, 1);
    assert!(!result.influence_wins);
}

#[test]
fn evaluator_reports_raw_exact_and_flattened_field_metrics() {
    let truth = json!({
        "schema_version": 1,
        "cases": [
            {
                "id": "metric_1",
                "expected": {
                    "name": "deploy_health",
                    "arguments": {
                        "deployment_id": "dep-correct",
                        "options": {"checks": ["http", "tcp"]}
                    }
                }
            },
            {
                "id": "metric_2",
                "expected": {"name": "workflow_list", "arguments": {}}
            }
        ]
    });
    let candidates = BTreeSet::from(["deploy_health".to_owned(), "workflow_list".to_owned()]);
    let mut cosine = vec![
        one_call(
            "metric_1",
            "deploy_health",
            json!({
                "deployment_id": "dep-wrong",
                "options": {"checks": ["http", "tcp"]},
                "extra": true
            }),
        ),
        no_call("metric_2"),
    ];
    let mut influence = vec![
        one_call(
            "metric_1",
            "deploy_health",
            json!({
                "deployment_id": "dep-correct",
                "options": {"checks": ["http", "tcp"]}
            }),
        ),
        one_call("metric_2", "workflow_list", json!({})),
    ];
    cosine[0].tokens = TokenCounts {
        scorer_input: 11,
        final_prompt_prefill: 101,
        generated_decode: 3,
    };
    cosine[1].tokens = TokenCounts {
        scorer_input: 13,
        final_prompt_prefill: 99,
        generated_decode: 5,
    };
    influence[0].tokens = TokenCounts {
        scorer_input: 17,
        final_prompt_prefill: 101,
        generated_decode: 7,
    };
    influence[1].tokens = TokenCounts {
        scorer_input: 19,
        final_prompt_prefill: 99,
        generated_decode: 11,
    };
    cosine[0].timings = PhaseTimings {
        scoring: Duration::from_millis(2),
        prefill: Duration::from_millis(3),
        decode: Duration::from_millis(5),
    };
    cosine[1].timings = PhaseTimings {
        scoring: Duration::from_millis(7),
        prefill: Duration::from_millis(11),
        decode: Duration::from_millis(13),
    };
    influence[0].timings = PhaseTimings {
        scoring: Duration::from_millis(17),
        prefill: Duration::from_millis(3),
        decode: Duration::from_millis(19),
    };
    influence[1].timings = PhaseTimings {
        scoring: Duration::from_millis(23),
        prefill: Duration::from_millis(11),
        decode: Duration::from_millis(29),
    };

    let result = evaluate_benchmark(
        &serde_json::to_string(&truth).expect("truth serializes"),
        &candidates,
        &cosine,
        &influence,
    )
    .expect("runs evaluate");

    assert_eq!((result.cosine.passed, result.cosine.failed), (0, 2));
    assert_eq!((result.influence.passed, result.influence.failed), (2, 0));
    assert_eq!(
        result.cosine.fields,
        FieldCounts {
            true_positive: 3,
            false_positive: 2,
            false_negative: 3,
        }
    );
    assert_eq!(
        result.influence.fields,
        FieldCounts {
            true_positive: 6,
            false_positive: 0,
            false_negative: 0,
        }
    );
    assert!((result.cosine.fields.precision() - 0.6).abs() < f64::EPSILON);
    assert!((result.cosine.fields.recall() - 0.5).abs() < f64::EPSILON);
    assert!((result.cosine.fields.f1() - (6.0 / 11.0)).abs() < f64::EPSILON);
    assert_eq!(
        result.cosine.tokens,
        TokenCounts {
            scorer_input: 24,
            final_prompt_prefill: 200,
            generated_decode: 8,
        }
    );
    assert_eq!(
        result.influence.tokens,
        TokenCounts {
            scorer_input: 36,
            final_prompt_prefill: 200,
            generated_decode: 18,
        }
    );
    assert_eq!(result.cosine.timings.scoring, Duration::from_millis(9));
    assert_eq!(result.cosine.timings.prefill, Duration::from_millis(14));
    assert_eq!(result.cosine.timings.decode, Duration::from_millis(18));
    assert_eq!(result.influence.timings.scoring, Duration::from_millis(40));
    assert_eq!(result.influence.timings.prefill, Duration::from_millis(14));
    assert_eq!(result.influence.timings.decode, Duration::from_millis(48));
    assert_eq!(result.cases[0].cosine_tokens.final_prompt_prefill, 101);
    assert_eq!(result.cases[0].influence_tokens.final_prompt_prefill, 101);
    assert_eq!(
        result.cases[0].cosine_timings.prefill,
        Duration::from_millis(3)
    );
    assert_eq!(
        result.cases[0].influence_timings.prefill,
        Duration::from_millis(3)
    );
    assert!(result.controls_verified);
    assert_eq!(result.paired.influence_only, 2);
    assert!(result.influence_wins);
}

#[test]
fn paired_categories_and_strict_win_gate_require_a_real_paired_gain() {
    let truth = json!({
        "schema_version": 1,
        "cases": (1..=5).map(|number| json!({
            "id": format!("pair_{number}"),
            "expected": {"name": "only_tool", "arguments": {"value": number}}
        })).collect::<Vec<_>>()
    });
    let candidates = BTreeSet::from(["only_tool".to_owned()]);
    let cosine = vec![
        one_call("pair_1", "only_tool", json!({"value": 1})),
        no_call("pair_2"),
        one_call("pair_3", "only_tool", json!({"value": 3})),
        no_call("pair_4"),
        no_call("pair_5"),
    ];
    let influence = vec![
        one_call("pair_1", "only_tool", json!({"value": 1})),
        one_call("pair_2", "only_tool", json!({"value": 2})),
        no_call("pair_3"),
        one_call("pair_4", "only_tool", json!({"value": 4})),
        no_call("pair_5"),
    ];

    let result = evaluate_benchmark(
        &serde_json::to_string(&truth).expect("truth serializes"),
        &candidates,
        &cosine,
        &influence,
    )
    .expect("paired runs evaluate");
    assert_eq!(
        result.paired,
        PairedCounts {
            both_pass: 1,
            influence_only: 2,
            cosine_only: 1,
            both_fail: 1,
        }
    );
    assert_eq!((result.cosine.passed, result.influence.passed), (2, 3));
    assert!(result.influence_wins);

    let pristine_cosine = ArmSummary {
        total: 5,
        passed: 2,
        failed: 3,
        fields: FieldCounts {
            true_positive: 100,
            false_positive: 0,
            false_negative: 0,
        },
        tokens: TokenCounts::default(),
        timings: PhaseTimings::default(),
    };
    let degraded_influence = ArmSummary {
        total: 5,
        passed: 3,
        failed: 2,
        fields: FieldCounts {
            true_positive: 1,
            false_positive: 50,
            false_negative: 50,
        },
        tokens: TokenCounts::default(),
        timings: PhaseTimings::default(),
    };
    assert!(!strict_win_gate(
        &pristine_cosine,
        &degraded_influence,
        result.paired,
        true,
    ));

    let mut no_raw_gain = result.influence.clone();
    no_raw_gain.passed = result.cosine.passed;
    no_raw_gain.failed = result.cosine.failed;
    assert!(!strict_win_gate(
        &result.cosine,
        &no_raw_gain,
        result.paired,
        true,
    ));

    let paired_tie = PairedCounts {
        influence_only: 1,
        cosine_only: 1,
        ..result.paired
    };
    assert!(!strict_win_gate(
        &result.cosine,
        &result.influence,
        paired_tie,
        true,
    ));
    assert!(!strict_win_gate(
        &result.cosine,
        &result.influence,
        result.paired,
        false,
    ));

    let truth_raw = serde_json::to_string(&truth).expect("truth serializes");
    let mut unequal_budget = influence.clone();
    unequal_budget[0].tokens.final_prompt_prefill += 1;
    let error = evaluate_benchmark(&truth_raw, &candidates, &cosine, &unequal_budget)
        .expect_err("unequal final-token budgets must be rejected");
    assert!(
        error
            .to_string()
            .contains("unequal final rendered prompt tokens")
    );

    let mut changed_controls = influence;
    changed_controls[0].controls.greedy = false;
    let error = evaluate_benchmark(&truth_raw, &candidates, &cosine, &changed_controls)
        .expect_err("changed controls must be rejected");
    assert!(
        error
            .to_string()
            .contains("changed model, template, candidate order")
    );
}

#[cfg(all(
    feature = "local-inference",
    not(all(target_os = "macos", target_arch = "x86_64"))
))]
mod live {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, ensure};
    use prism_agent::capability::CapabilityIndex;
    use prism_agent::influence::{CandidateInfluence, InfluenceIndex};
    use prism_embed::{NativeModelStatus, NativeOnnx, native_model_status};
    use prism_llm::{
        BUNDLED_GEMMA, ChatMessage, LOCAL_GGUF_URL, LlmClient, LlmConfig,
        LocalModelIdentityOutcome, LocalPromptInfluenceOutcome, ToolDefinition,
    };

    use super::benchmark::evaluator::{
        BenchmarkEvaluation, DeterminismControls, evaluate_benchmark,
    };
    use super::benchmark::{
        AsyncFinalPromptTokenCounter, CandidatePool, RetrieverCase, build_exact_token_bucket_async,
        load_candidate_pool, load_cases, materialize_tool_set,
    };
    use super::{
        CANDIDATES, CASES, GenerationFailureObservation, TRUTH, record_generation_attempt,
        require_discriminative_influence,
    };

    const BENCHMARK_SYSTEM: &str = "Choose exactly one supplied function that best fulfills the user request. Call that function with JSON arguments. Do not answer in prose.";

    struct LivePromptCounter<'a> {
        client: &'a LlmClient,
    }

    #[async_trait::async_trait]
    impl AsyncFinalPromptTokenCounter for LivePromptCounter<'_> {
        async fn final_prompt_tokens(
            &self,
            case: &RetrieverCase,
            tools: &[ToolDefinition],
        ) -> Result<usize> {
            let rendered = self
                .client
                .render_local_prompt(&benchmark_messages(&case.user), tools)
                .await?;
            usize::try_from(rendered.token_count).context("rendered token count exceeds usize")
        }
    }

    /// Exact BERT/WordPiece token counter for the ASCII benchmark queries.
    /// Candidate documents are embedded once during untimed index setup; only
    /// one query is tokenized during each timed cosine lookup.
    struct BgeQueryTokenCounter {
        vocab: BTreeSet<String>,
        max_input_chars_per_word: usize,
        single_special_tokens: usize,
    }

    impl BgeQueryTokenCounter {
        fn from_installed_snapshot() -> Result<Self> {
            let snapshot_dir = match native_model_status() {
                NativeModelStatus::Ready { snapshot_dir, .. } => snapshot_dir,
                NativeModelStatus::Unavailable(reason) => return Err(reason.into()),
            };
            let tokenizer_path = snapshot_dir.join("tokenizer.json");
            let tokenizer: serde_json::Value = serde_json::from_slice(
                &std::fs::read(&tokenizer_path)
                    .with_context(|| format!("cannot read {}", tokenizer_path.display()))?,
            )
            .with_context(|| format!("invalid {}", tokenizer_path.display()))?;
            ensure!(
                tokenizer["model"]["type"].as_str() == Some("WordPiece"),
                "installed BGE tokenizer is not WordPiece"
            );
            ensure!(
                tokenizer["pre_tokenizer"]["type"].as_str() == Some("BertPreTokenizer"),
                "installed BGE tokenizer is not BertPreTokenizer"
            );
            ensure!(
                tokenizer["normalizer"]["type"].as_str() == Some("BertNormalizer")
                    && tokenizer["normalizer"]["lowercase"].as_bool() == Some(true),
                "installed BGE tokenizer does not use the expected lowercase BertNormalizer"
            );
            let vocab = tokenizer["model"]["vocab"]
                .as_object()
                .context("BGE tokenizer has no WordPiece vocabulary")?
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>();
            ensure!(vocab.contains("[UNK]"), "BGE vocabulary has no [UNK]");
            let max_input_chars_per_word = tokenizer["model"]["max_input_chars_per_word"]
                .as_u64()
                .context("BGE tokenizer has no max_input_chars_per_word")?
                .try_into()
                .context("BGE max input chars exceeds usize")?;
            let single_special_tokens = tokenizer["post_processor"]["single"]
                .as_array()
                .context("BGE tokenizer has no single-sequence post processor")?
                .iter()
                .filter(|entry| entry.get("SpecialToken").is_some())
                .count();
            ensure!(
                single_special_tokens == 2,
                "BGE single-sequence template must add exactly [CLS] and [SEP]"
            );
            Ok(Self {
                vocab,
                max_input_chars_per_word,
                single_special_tokens,
            })
        }

        fn count(&self, text: &str) -> Result<usize> {
            ensure!(
                text.is_ascii(),
                "the exact test-only BGE counter accepts ASCII benchmark queries only"
            );
            let mut basic_tokens = Vec::new();
            let mut current = String::new();
            for byte in text.bytes() {
                if byte.is_ascii_whitespace() || byte.is_ascii_control() {
                    if !current.is_empty() {
                        basic_tokens.push(std::mem::take(&mut current));
                    }
                } else if byte.is_ascii_punctuation() {
                    if !current.is_empty() {
                        basic_tokens.push(std::mem::take(&mut current));
                    }
                    basic_tokens.push(char::from(byte).to_string());
                } else {
                    current.push(char::from(byte).to_ascii_lowercase());
                }
            }
            if !current.is_empty() {
                basic_tokens.push(current);
            }

            let wordpieces = basic_tokens
                .iter()
                .map(|token| self.wordpiece_count(token))
                .sum::<usize>();
            Ok(self.single_special_tokens + wordpieces)
        }

        fn wordpiece_count(&self, token: &str) -> usize {
            if token.len() > self.max_input_chars_per_word {
                return 1;
            }
            let mut start = 0;
            let mut pieces = 0;
            while start < token.len() {
                let found = (start + 1..=token.len()).rev().find(|end| {
                    if start == 0 {
                        self.vocab.contains(&token[start..*end])
                    } else {
                        self.vocab.contains(&format!("##{}", &token[start..*end]))
                    }
                });
                let Some(end) = found else {
                    // WordPiece replaces the entire word, not just the
                    // unmatched suffix, with one [UNK].
                    return 1;
                };
                pieces += 1;
                start = end;
            }
            pieces
        }
    }

    #[derive(Debug)]
    struct LiveSelection {
        case_id: String,
        final_prompt_tokens: usize,
        cosine_candidates: Vec<String>,
        influence_candidates: Vec<String>,
        cosine_generation_failure: Option<GenerationFailureObservation>,
        influence_generation_failure: Option<GenerationFailureObservation>,
    }

    fn benchmark_messages(user: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(BENCHMARK_SYSTEM.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(user.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ]
    }

    fn fixed_production_tools() -> Vec<ToolDefinition> {
        prism_agent::meta_tools::definitions()
            .iter()
            .map(prism_agent::tool_catalog::LoadedTool::to_definition)
            .collect()
    }

    fn ranking_indices(ranking: &[String], pool: &CandidatePool) -> Result<Vec<usize>> {
        let ordinal_by_name = pool
            .definitions
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.function.name.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        ensure!(
            ranking.len() == ordinal_by_name.len(),
            "ranking has {} entries for {} candidates",
            ranking.len(),
            ordinal_by_name.len()
        );
        ranking
            .iter()
            .map(|name| {
                ordinal_by_name
                    .get(name.as_str())
                    .copied()
                    .with_context(|| format!("ranking names unknown candidate {name:?}"))
            })
            .collect()
    }

    fn subset_names(subset: &[usize], pool: &CandidatePool) -> Vec<String> {
        subset
            .iter()
            .map(|index| pool.definitions[*index].function.name.clone())
            .collect()
    }

    fn print_arm(
        label: &str,
        summary: &super::benchmark::evaluator::ArmSummary,
        generation_failures: usize,
    ) {
        println!(
            "{label:<9} pass={}/{} fail={} tp={} fp={} fn={} precision={:.4} recall={:.4} f1={:.4} scorer_tokens={} final_prompt_tokens={} observed_decode_tokens={} scoring_ms={:.3} observed_prefill_ms={:.3} observed_decode_ms={:.3} generation_failures={} phase_totals_complete={}",
            summary.passed,
            summary.total,
            summary.failed,
            summary.fields.true_positive,
            summary.fields.false_positive,
            summary.fields.false_negative,
            summary.fields.precision(),
            summary.fields.recall(),
            summary.fields.f1(),
            summary.tokens.scorer_input,
            summary.tokens.final_prompt_prefill,
            summary.tokens.generated_decode,
            summary.timings.scoring.as_secs_f64() * 1_000.0,
            summary.timings.prefill.as_secs_f64() * 1_000.0,
            summary.timings.decode.as_secs_f64() * 1_000.0,
            generation_failures,
            generation_failures == 0,
        );
    }

    fn print_report(
        evaluation: &BenchmarkEvaluation,
        selections: &[LiveSelection],
        controls: &DeterminismControls,
        cosine_index_setup: Duration,
    ) {
        let cosine_generation_failures = selections
            .iter()
            .filter(|selection| selection.cosine_generation_failure.is_some())
            .count();
        let influence_generation_failures = selections
            .iter()
            .filter(|selection| selection.influence_generation_failure.is_some())
            .count();
        println!(
            "JSPACE_PROTOCOL gguf_sha256={} template_sha256={} candidate_definitions_sha256={} greedy={} warm_load_count={} cosine_index_setup_ms={:.3} setup_excluded_from_timed_scoring=true",
            controls.gguf_sha256,
            controls.template_sha256,
            controls.candidate_definitions_sha256,
            controls.greedy,
            controls.warm_load_count,
            cosine_index_setup.as_secs_f64() * 1_000.0,
        );
        println!("JSPACE_SUMMARY arm raw_accuracy fields tokens wall_time");
        print_arm("cosine", &evaluation.cosine, cosine_generation_failures);
        print_arm(
            "influence",
            &evaluation.influence,
            influence_generation_failures,
        );
        println!(
            "JSPACE_PAIRED both_pass={} influence_only={} cosine_only={} both_fail={}",
            evaluation.paired.both_pass,
            evaluation.paired.influence_only,
            evaluation.paired.cosine_only,
            evaluation.paired.both_fail,
        );
        println!(
            "JSPACE_CASES case final_tokens outcome cosine_candidates influence_candidates cosine_scorer_tokens influence_scorer_tokens cosine_prefill_tokens influence_prefill_tokens cosine_decode_tokens_or_NA influence_decode_tokens_or_NA cosine_score_ms influence_score_ms cosine_prefill_ms_or_NA influence_prefill_ms_or_NA cosine_decode_ms_or_NA influence_decode_ms_or_NA"
        );
        for (case, selection) in evaluation.cases.iter().zip(selections) {
            assert_eq!(case.case_id, selection.case_id);
            let outcome = match (case.cosine_pass, case.influence_pass) {
                (true, true) => "both_pass",
                (false, true) => "influence_only",
                (true, false) => "cosine_only",
                (false, false) => "both_fail",
            };
            let cosine_decode_tokens = selection.cosine_generation_failure.as_ref().map_or_else(
                || case.cosine_tokens.generated_decode.to_string(),
                |_| "NA".into(),
            );
            let influence_decode_tokens =
                selection.influence_generation_failure.as_ref().map_or_else(
                    || case.influence_tokens.generated_decode.to_string(),
                    |_| "NA".into(),
                );
            let cosine_prefill_ms = selection.cosine_generation_failure.as_ref().map_or_else(
                || format!("{:.3}", case.cosine_timings.prefill.as_secs_f64() * 1_000.0),
                |_| "NA".into(),
            );
            let influence_prefill_ms = selection.influence_generation_failure.as_ref().map_or_else(
                || {
                    format!(
                        "{:.3}",
                        case.influence_timings.prefill.as_secs_f64() * 1_000.0
                    )
                },
                |_| "NA".into(),
            );
            let cosine_decode_ms = selection.cosine_generation_failure.as_ref().map_or_else(
                || format!("{:.3}", case.cosine_timings.decode.as_secs_f64() * 1_000.0),
                |_| "NA".into(),
            );
            let influence_decode_ms = selection.influence_generation_failure.as_ref().map_or_else(
                || {
                    format!(
                        "{:.3}",
                        case.influence_timings.decode.as_secs_f64() * 1_000.0
                    )
                },
                |_| "NA".into(),
            );
            println!(
                "{} {} {} {:?} {:?} {} {} {} {} {} {} {:.3} {:.3} {} {} {} {}",
                case.case_id,
                selection.final_prompt_tokens,
                outcome,
                selection.cosine_candidates,
                selection.influence_candidates,
                case.cosine_tokens.scorer_input,
                case.influence_tokens.scorer_input,
                case.cosine_tokens.final_prompt_prefill,
                case.influence_tokens.final_prompt_prefill,
                cosine_decode_tokens,
                influence_decode_tokens,
                case.cosine_timings.scoring.as_secs_f64() * 1_000.0,
                case.influence_timings.scoring.as_secs_f64() * 1_000.0,
                cosine_prefill_ms,
                influence_prefill_ms,
                cosine_decode_ms,
                influence_decode_ms,
            );
            for (arm, failure) in [
                ("cosine", selection.cosine_generation_failure.as_ref()),
                ("influence", selection.influence_generation_failure.as_ref()),
            ] {
                if let Some(failure) = failure {
                    println!(
                        "JSPACE_GENERATION_FAILURE case={} arm={} total_wall_ms={:.3} error={}",
                        case.case_id,
                        arm,
                        failure.total_wall_time.as_secs_f64() * 1_000.0,
                        serde_json::to_string(&failure.error)
                            .expect("generation failure text is serializable"),
                    );
                }
            }
        }
        println!(
            "JSPACE_DECISION controls_verified={} strict_win={} shipping_status={}",
            evaluation.controls_verified,
            evaluation.influence_wins,
            if evaluation.influence_wins {
                "win"
            } else {
                "experimental_off_by_default"
            }
        );
    }

    /// Real, paired local benchmark. It never downloads either model: both
    /// the GGUF and pinned BGE snapshot must already be installed explicitly.
    /// Truth is first referenced after both retrieval/generation arms finish.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires PRISM_TEST_GGUF plus the explicitly installed pinned BGE snapshot"]
    async fn paired_cosine_vs_influence_local_gguf_benchmark() -> Result<()> {
        let gguf = std::env::var("PRISM_TEST_GGUF").context("set PRISM_TEST_GGUF")?;
        ensure!(
            std::path::Path::new(&gguf).is_file(),
            "PRISM_TEST_GGUF is not a file"
        );
        let pool = load_candidate_pool(CANDIDATES)?;
        let cases = load_cases(CASES)?;
        let fixed_tools = fixed_production_tools();
        ensure!(fixed_tools.len() == 7, "production fixed-tool set changed");

        let client = LlmClient::new(LlmConfig {
            base_url: LOCAL_GGUF_URL.to_string(),
            model: gguf,
            max_output_tokens: Some(128),
            ..LlmConfig::default()
        });
        let model_identity = match client.local_model_identity().await? {
            LocalModelIdentityOutcome::Verified { identity } => identity,
            LocalModelIdentityOutcome::Unavailable { code, detail } => {
                anyhow::bail!("local model identity unavailable ({code:?}): {detail}")
            }
        };
        ensure!(
            model_identity.sha256 == BUNDLED_GEMMA.sha256
                && model_identity.size_bytes == BUNDLED_GEMMA.size_bytes,
            "PRISM_TEST_GGUF does not match the pinned Gemma artifact (sha256={}, bytes={})",
            model_identity.sha256,
            model_identity.size_bytes
        );
        let model_sha256 = model_identity.sha256;
        let warm_messages = benchmark_messages("Reply with exactly the word READY.");
        let warm_rendered = client.render_local_prompt(&warm_messages, &[]).await?;
        let warm_response = client.chat_with_tools(&warm_messages, &[]).await?;
        ensure!(
            warm_response
                .usage
                .as_ref()
                .is_some_and(|usage| usage.completion_tokens > 0),
            "GGUF warm-up produced no decode tokens"
        );
        ensure!(
            warm_response.generation_metrics.is_some(),
            "GGUF warm-up did not expose phase timings"
        );

        let controls = DeterminismControls {
            gguf_sha256: model_sha256.clone(),
            template_sha256: warm_rendered.template_sha256.clone(),
            candidate_definitions_sha256: pool.definitions_sha256.clone(),
            greedy: true,
            warm_load_count: 1,
        };

        let embedder = tokio::task::spawn_blocking(NativeOnnx::new)
            .await
            .context("native BGE initialization task panicked")??;
        let bge_tokens = BgeQueryTokenCounter::from_installed_snapshot()?;
        let mut cosine_index = CapabilityIndex::from_entries(pool.definitions.iter().map(|tool| {
            (
                tool.function.name.clone(),
                format!("{}: {}", tool.function.name, tool.function.description),
            )
        }));
        let cosine_index_started = Instant::now();
        cosine_index.embed_all(&embedder).await?;
        let cosine_index_setup = cosine_index_started.elapsed();
        ensure!(
            cosine_index.embedded_count() == pool.definitions.len(),
            "cosine index did not embed the complete candidate pool"
        );

        let influence_index = InfluenceIndex::new(
            pool.definitions.clone(),
            &model_sha256,
            &controls.template_sha256,
        );
        let prompt_counter = LivePromptCounter { client: &client };
        let mut cosine_runs = Vec::with_capacity(cases.len());
        let mut influence_runs = Vec::with_capacity(cases.len());
        let mut selections = Vec::with_capacity(cases.len());

        for (case_index, case) in cases.iter().enumerate() {
            println!(
                "JSPACE_PROGRESS case={} ordinal={}/{} stage=exact_token_bucket",
                case.id,
                case_index + 1,
                cases.len()
            );
            let messages = benchmark_messages(&case.user);
            let bucket = build_exact_token_bucket_async(
                case,
                &pool.definitions,
                &fixed_tools,
                pool.manifest.selection_cardinality,
                pool.manifest.max_final_prompt_tokens,
                &prompt_counter,
            )
            .await
            .with_context(|| format!("no fair final-token bucket for {}", case.id))?;

            println!(
                "JSPACE_PROGRESS case={} ordinal={}/{} stage=scoring",
                case.id,
                case_index + 1,
                cases.len()
            );
            let cosine_started = Instant::now();
            let cosine_names = cosine_index
                .retrieve(&case.user, pool.definitions.len(), &embedder)
                .await;
            let cosine_scoring = cosine_started.elapsed();
            let cosine_scorer_tokens = bge_tokens.count(&case.user)?;
            let cosine_ranking = ranking_indices(&cosine_names, &pool)?;

            let influence_started = Instant::now();
            let influence_outcome = client
                .score_local_tool_influence(&messages, &fixed_tools, &pool.definitions)
                .await?;
            let influence_scoring = influence_started.elapsed();
            let influence_report = match influence_outcome {
                LocalPromptInfluenceOutcome::Scored { report } => report,
                LocalPromptInfluenceOutcome::Unavailable { code, detail } => {
                    anyhow::bail!("local influence unavailable ({code:?}): {detail}")
                }
            };
            ensure!(
                influence_report.model_sha256 == controls.gguf_sha256
                    && influence_report.model_size_bytes == BUNDLED_GEMMA.size_bytes,
                "{} changed the loaded GGUF identity",
                case.id
            );
            ensure!(
                influence_report.template_sha256 == controls.template_sha256,
                "{} changed the GGUF template identity",
                case.id
            );
            ensure!(
                influence_report.candidates.len() == pool.definitions.len(),
                "{} returned an incomplete influence score vector",
                case.id
            );
            for score in &influence_report.candidates {
                ensure!(
                    pool.definitions
                        .get(score.candidate_index)
                        .is_some_and(|tool| tool.function.name == score.tool_name),
                    "{} returned a misaligned influence candidate",
                    case.id
                );
            }
            let influence_scores = influence_report
                .candidates
                .iter()
                .filter_map(|score| {
                    score
                        .normalized_js_divergence_per_added_prompt_token
                        .filter(|value| value.is_finite())
                        .map(|score_per_added_token| CandidateInfluence {
                            name: score.tool_name.clone(),
                            score_per_added_token,
                        })
                })
                .collect::<Vec<_>>();
            require_discriminative_influence(&influence_scores)
                .with_context(|| format!("{} refused influence ranking", case.id))?;
            let influence_names = influence_index.rank(&influence_scores);
            let influence_ranking = ranking_indices(&influence_names, &pool)?;
            let influence_scorer_tokens = influence_report
                .candidates
                .iter()
                .fold(influence_report.baseline_prompt_tokens, |total, score| {
                    total.saturating_add(score.candidate_prompt_tokens)
                });
            let influence_scorer_tokens = usize::try_from(influence_scorer_tokens)
                .context("influence scorer token count exceeds usize")?;

            let cosine_subset = bucket.select_for_ranking(&cosine_ranking)?;
            let influence_subset = bucket.select_for_ranking(&influence_ranking)?;
            let cosine_tools =
                materialize_tool_set(&pool.definitions, &fixed_tools, &cosine_subset)?;
            let influence_tools =
                materialize_tool_set(&pool.definitions, &fixed_tools, &influence_subset)?;
            let cosine_rendered = client.render_local_prompt(&messages, &cosine_tools).await?;
            let influence_rendered = client
                .render_local_prompt(&messages, &influence_tools)
                .await?;
            ensure!(
                cosine_rendered.template_sha256 == controls.template_sha256
                    && influence_rendered.template_sha256 == controls.template_sha256,
                "{} rendered with a different template",
                case.id
            );
            ensure!(
                usize::try_from(cosine_rendered.token_count)? == bucket.final_prompt_tokens
                    && cosine_rendered.token_count == influence_rendered.token_count,
                "{} did not preserve the exact common final-token bucket",
                case.id
            );

            println!(
                "JSPACE_PROGRESS case={} ordinal={}/{} stage=generation",
                case.id,
                case_index + 1,
                cases.len()
            );
            // Alternate arm order to avoid giving either method every first
            // generation while retaining fresh KV contexts for every call.
            let (cosine_attempt, influence_attempt) = if case_index % 2 == 0 {
                let cosine_started = Instant::now();
                let cosine = client.chat_with_tools(&messages, &cosine_tools).await;
                let cosine = (cosine, cosine_started.elapsed());
                let influence_started = Instant::now();
                let influence = client.chat_with_tools(&messages, &influence_tools).await;
                let influence = (influence, influence_started.elapsed());
                (cosine, influence)
            } else {
                let influence_started = Instant::now();
                let influence = client.chat_with_tools(&messages, &influence_tools).await;
                let influence = (influence, influence_started.elapsed());
                let cosine_started = Instant::now();
                let cosine = client.chat_with_tools(&messages, &cosine_tools).await;
                let cosine = (cosine, cosine_started.elapsed());
                (cosine, influence)
            };
            let cosine_recorded = record_generation_attempt(
                &case.id,
                cosine_attempt.0,
                &controls,
                cosine_scorer_tokens,
                cosine_scoring,
                bucket.final_prompt_tokens,
                cosine_attempt.1,
            )?;
            let influence_recorded = record_generation_attempt(
                &case.id,
                influence_attempt.0,
                &controls,
                influence_scorer_tokens,
                influence_scoring,
                bucket.final_prompt_tokens,
                influence_attempt.1,
            )?;
            cosine_runs.push(cosine_recorded.run);
            influence_runs.push(influence_recorded.run);
            selections.push(LiveSelection {
                case_id: case.id.clone(),
                final_prompt_tokens: bucket.final_prompt_tokens,
                cosine_candidates: subset_names(&cosine_subset, &pool),
                influence_candidates: subset_names(&influence_subset, &pool),
                cosine_generation_failure: cosine_recorded.failure,
                influence_generation_failure: influence_recorded.failure,
            });
            println!(
                "JSPACE_PROGRESS case={} ordinal={}/{} stage=complete",
                case.id,
                case_index + 1,
                cases.len()
            );
        }

        // Evaluator-only truth enters only after both retrievers and both
        // generation arms have completed for every fixture.
        let candidate_names = pool
            .manifest
            .ordered_names
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let evaluation =
            evaluate_benchmark(TRUTH, &candidate_names, &cosine_runs, &influence_runs)?;
        ensure!(
            evaluation.controls_verified,
            "benchmark controls were not verified"
        );
        print_report(&evaluation, &selections, &controls, cosine_index_setup);
        Ok(())
    }
}
