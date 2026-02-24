"""Registry for ML algorithms, allowing plugin-based extension."""
from typing import Callable, Dict, List


class AlgorithmRegistry:
    """Registry of ML algorithm factories."""

    def __init__(self):
        self._algorithms: Dict[str, dict] = {}

    def register(self, name: str, description: str, factory: Callable) -> None:
        self._algorithms[name] = {
            "name": name,
            "description": description,
            "factory": factory,
        }

    def get(self, name: str):
        """Return a new model instance for the given algorithm."""
        if name not in self._algorithms:
            raise ValueError(
                f"Unknown algorithm: {name}. Available: {list(self._algorithms.keys())}"
            )
        return self._algorithms[name]["factory"]()

    def list_algorithms(self) -> List[dict]:
        return [
            {"name": v["name"], "description": v["description"]}
            for v in self._algorithms.values()
        ]

    def has(self, name: str) -> bool:
        return name in self._algorithms


def get_default_registry() -> AlgorithmRegistry:
    """Pre-loaded with the built-in algorithms."""
    reg = AlgorithmRegistry()

    reg.register(
        "random_forest",
        "Random Forest Regressor",
        lambda: __import__(
            "sklearn.ensemble", fromlist=["RandomForestRegressor"]
        ).RandomForestRegressor(n_estimators=100, random_state=42),
    )
    reg.register(
        "gradient_boosting",
        "Gradient Boosting Regressor",
        lambda: __import__(
            "sklearn.ensemble", fromlist=["GradientBoostingRegressor"]
        ).GradientBoostingRegressor(n_estimators=100, random_state=42),
    )
    reg.register(
        "linear",
        "Linear Regression",
        lambda: __import__(
            "sklearn.linear_model", fromlist=["LinearRegression"]
        ).LinearRegression(),
    )

    try:
        import xgboost  # noqa: F401

        reg.register(
            "xgboost",
            "XGBoost Regressor",
            lambda: __import__("xgboost").XGBRegressor(
                n_estimators=100, random_state=42
            ),
        )
    except ImportError:
        pass

    try:
        import lightgbm  # noqa: F401

        reg.register(
            "lightgbm",
            "LightGBM Regressor",
            lambda: __import__("lightgbm").LGBMRegressor(
                n_estimators=100, random_state=42, verbose=-1
            ),
        )
    except ImportError:
        pass

    return reg
