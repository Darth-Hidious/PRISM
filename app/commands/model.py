"""Model management CLI commands."""
import click
from rich.console import Console
from rich.table import Table


@click.group()
def model():
    """Train, evaluate, and manage ML models."""
    pass


@model.command()
@click.option("--property", "prop", default=None, help="Specific property to train")
@click.option("--algorithm", default="random_forest", help="Algorithm to use")
@click.option("--dataset", default=None, help="Dataset name to train on")
def train(prop, algorithm, dataset):
    """Train ML models on collected data."""
    console = Console()
    from app.data.store import DataStore
    from app.ml.features import composition_features
    from app.ml.trainer import train_model, AVAILABLE_ALGORITHMS
    from app.ml.registry import ModelRegistry
    import numpy as np

    store = DataStore()
    datasets = store.list_datasets()
    if not datasets:
        console.print("[yellow]No datasets found. Run 'prism data collect' first.[/yellow]")
        return

    ds_name = dataset or datasets[0]["name"]
    console.print(f"[bold]Training on dataset:[/bold] {ds_name}")

    df = store.load(ds_name)

    # Generate features for each row
    feature_rows = []
    for _, row in df.iterrows():
        formula = row.get("formula", "")
        if formula:
            feats = composition_features(formula)
            if feats:
                feature_rows.append(feats)

    if not feature_rows:
        console.print("[red]No valid formulas for featurization.[/red]")
        return

    # Build feature matrix
    all_keys = sorted(set(k for f in feature_rows for k in f.keys()))
    X = np.array([[f.get(k, 0.0) for k in all_keys] for f in feature_rows])

    target_col = prop or "band_gap"
    if target_col in df.columns:
        y = df[target_col].values[:len(X)]
    else:
        console.print(f"[yellow]Property '{target_col}' not in dataset. Using random target for demo.[/yellow]")
        y = np.random.rand(len(X))

    with console.status(f"[bold green]Training {algorithm}..."):
        result = train_model(X, y, algorithm=algorithm, property_name=target_col)

    registry = ModelRegistry()
    registry.save_model(result["model"], target_col, algorithm, result["metrics"])

    metrics = result["metrics"]
    console.print(f"[green]Model trained and saved![/green]")
    console.print(f"  MAE:  {metrics['mae']:.4f}")
    console.print(f"  RMSE: {metrics['rmse']:.4f}")
    console.print(f"  R2:   {metrics['r2']:.4f}")


@model.command()
def status():
    """List available trained models and their metrics."""
    console = Console()
    from app.ml.registry import ModelRegistry

    registry = ModelRegistry()
    models = registry.list_models()

    if not models:
        console.print("[dim]No trained models. Run 'prism model train' first.[/dim]")
        return

    table = Table(title="Trained Models")
    table.add_column("Property")
    table.add_column("Algorithm")
    table.add_column("MAE")
    table.add_column("R2")
    table.add_column("Saved At")

    for m in models:
        metrics = m.get("metrics", {})
        table.add_row(
            m.get("property", "?"),
            m.get("algorithm", "?"),
            f"{metrics.get('mae', '?'):.4f}" if isinstance(metrics.get('mae'), (int, float)) else "?",
            f"{metrics.get('r2', '?'):.4f}" if isinstance(metrics.get('r2'), (int, float)) else "?",
            m.get("saved_at", "?")[:19],
        )

    console.print(table)
