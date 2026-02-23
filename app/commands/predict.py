"""Predict CLI command."""
import click
from rich.console import Console
from rich.table import Table


@click.command()
@click.argument("formula")
@click.option("--property", "prop", default="band_gap", help="Property to predict")
@click.option("--algorithm", default="random_forest", help="ML algorithm")
@click.option("--all-properties", is_flag=True, help="Predict all available properties")
def predict(formula, prop, algorithm, all_properties):
    """Predict material properties from chemical formula."""
    console = Console()
    from app.ml.predictor import Predictor
    from app.ml.registry import ModelRegistry

    predictor = Predictor()
    registry = ModelRegistry()

    if all_properties:
        models = registry.list_models()
        if not models:
            console.print("[yellow]No trained models. Run 'prism model train' first.[/yellow]")
            return

        table = Table(title=f"Predictions for {formula}")
        table.add_column("Property")
        table.add_column("Algorithm")
        table.add_column("Prediction")

        for m in models:
            result = predictor.predict(formula, m["property"], m["algorithm"])
            val = f"{result['prediction']:.4f}" if "prediction" in result else result.get("error", "?")
            table.add_row(m["property"], m["algorithm"], val)

        console.print(table)
    else:
        result = predictor.predict(formula, prop, algorithm)
        if "prediction" in result:
            console.print(f"[bold]{formula}[/bold] → {prop} = [green]{result['prediction']:.4f}[/green] ({algorithm})")
        else:
            console.print(f"[red]{result.get('error', 'Unknown error')}[/red]")
