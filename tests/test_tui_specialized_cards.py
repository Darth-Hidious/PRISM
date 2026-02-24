"""Tests for specialized card widgets."""


def test_metrics_card():
    from app.tui.widgets.cards import MetricsCard
    card = MetricsCard(
        tool_name="train_model",
        elapsed_ms=4100,
        property_name="formation_energy",
        algorithm="random_forest",
        metrics={"mae": 0.0423, "rmse": 0.0612, "r2": 0.934, "n_train": 120, "n_test": 30},
        plot_path="parity.png",
    )
    assert card.property_name == "formation_energy"
    assert card.metrics["r2"] == 0.934


def test_calphad_card():
    from app.tui.widgets.cards import CalphadCard
    card = CalphadCard(
        tool_name="calculate_equilibrium",
        elapsed_ms=8200,
        system="W-Rh",
        conditions="T=1500K, P=101325Pa",
        phases={"BCC_A2": 0.62, "HCP_A3": 0.38},
        gibbs_energy=-45231.4,
    )
    assert "BCC_A2" in card.phases
    assert card.gibbs_energy == -45231.4


def test_validation_card():
    from app.tui.widgets.cards import ValidationCard
    card = ValidationCard(
        tool_name="validate_dataset",
        elapsed_ms=1200,
        quality_score=0.87,
        findings={
            "critical": [{"msg": "band_gap = -0.5 violates >= 0"}],
            "warning": [{"msg": "outlier z=3.4"}],
            "info": [{"msg": "density 45% completeness"}],
        },
    )
    assert card.quality_score == 0.87
    assert len(card.findings["critical"]) == 1


def test_results_table_card():
    from app.tui.widgets.cards import ResultsTableCard
    rows = [{"formula": f"W{i}Rh", "provider": "MP"} for i in range(20)]
    card = ResultsTableCard(
        tool_name="search_optimade",
        elapsed_ms=17000,
        rows=rows,
        total_count=49,
    )
    assert card.total_count == 49
    assert len(card.preview_rows) == 3  # default preview


def test_plot_card():
    from app.tui.widgets.cards import PlotCard
    card = PlotCard(
        tool_name="plot_materials_comparison",
        elapsed_ms=2300,
        description="Scatter: band_gap vs formation_energy",
        file_path="prism_scatter.png",
    )
    assert card.file_path == "prism_scatter.png"
