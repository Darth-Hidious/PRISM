//! A rejected agent-run INSERT must be observed and must not fail the turn.

#[path = "support/agent_run_harness.rs"]
mod agent_run_harness;
mod common;

use agent_run_harness::{TEST_EXPECTED_COST_USD, run_stub_turn};

#[test]
fn ledger_row_write_failure_does_not_fail_the_real_turn() {
    // This integration binary has one test and common's pre-main constructor
    // has already selected its isolated database path.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime");
    runtime.block_on(async {
        let path = prism_agent::hooks::provenance_db_path();
        let store = prism_provenance::ProvenanceStore::open(&path)
            .await
            .expect("initialize isolated schema");
        drop(store);

        let database = turso::Builder::new_local(path.to_str().expect("UTF-8 test path"))
            .build()
            .await
            .expect("open trigger connection");
        let connection = database.connect().expect("connect trigger database");
        connection
            .execute(
                "CREATE TABLE agent_run_write_attempts (attempts INTEGER NOT NULL)",
                (),
            )
            .await
            .expect("create attempt counter");
        connection
            .execute("INSERT INTO agent_run_write_attempts VALUES (0)", ())
            .await
            .expect("seed attempt counter");
        // RAISE(IGNORE) keeps the counter update but suppresses the row. The
        // store detects the zero-row INSERT as an error, giving this test both
        // a forced write failure and proof that write-through was attempted.
        connection
            .execute(
                r#"CREATE TRIGGER reject_agent_run_insert
                   BEFORE INSERT ON agent_runs
                   BEGIN
                       UPDATE agent_run_write_attempts SET attempts = attempts + 1;
                       SELECT RAISE(IGNORE);
                   END"#,
                (),
            )
            .await
            .expect("install rejecting trigger");

        let outcome = run_stub_turn("agent-run-ledger-write-failure")
            .await
            .expect("ledger failure must not become a turn failure");
        assert_eq!(outcome.answer, "LEDGER_TURN_DONE");
        assert!(
            (outcome.estimated_cost - TEST_EXPECTED_COST_USD).abs() < f64::EPSILON,
            "the real turn must retain accurate billing when its ledger write fails"
        );

        let mut rows = connection
            .query("SELECT attempts FROM agent_run_write_attempts", ())
            .await
            .expect("query attempt counter");
        let row = rows
            .next()
            .await
            .expect("read attempt counter")
            .expect("attempt counter row");
        assert_eq!(
            row.get_value(0)
                .expect("attempt value")
                .as_integer()
                .copied(),
            Some(1),
            "the real turn must attempt the rejected ledger write exactly once"
        );
    });
}
