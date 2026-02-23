"""Model training pipeline."""
import numpy as np
from typing import Dict, Optional


AVAILABLE_ALGORITHMS = {
    "random_forest": "Random Forest Regressor",
    "gradient_boosting": "Gradient Boosting Regressor",
    "linear": "Linear Regression",
}

try:
    import xgboost
    AVAILABLE_ALGORITHMS["xgboost"] = "XGBoost Regressor"
except ImportError:
    pass

try:
    import lightgbm
    AVAILABLE_ALGORITHMS["lightgbm"] = "LightGBM Regressor"
except ImportError:
    pass


def _create_model(algorithm: str):
    from sklearn.ensemble import RandomForestRegressor, GradientBoostingRegressor
    from sklearn.linear_model import LinearRegression

    if algorithm == "random_forest":
        return RandomForestRegressor(n_estimators=100, random_state=42)
    elif algorithm == "gradient_boosting":
        return GradientBoostingRegressor(n_estimators=100, random_state=42)
    elif algorithm == "linear":
        return LinearRegression()
    elif algorithm == "xgboost":
        import xgboost as xgb
        return xgb.XGBRegressor(n_estimators=100, random_state=42)
    elif algorithm == "lightgbm":
        import lightgbm as lgb
        return lgb.LGBMRegressor(n_estimators=100, random_state=42, verbose=-1)
    else:
        raise ValueError(f"Unknown algorithm: {algorithm}")


def train_model(
    X: np.ndarray,
    y: np.ndarray,
    algorithm: str = "random_forest",
    property_name: str = "property",
    test_size: float = 0.2,
) -> Dict:
    from sklearn.model_selection import train_test_split
    from sklearn.metrics import mean_absolute_error, mean_squared_error, r2_score

    model = _create_model(algorithm)

    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=test_size, random_state=42,
    )

    model.fit(X_train, y_train)
    y_pred = model.predict(X_test)

    metrics = {
        "mae": float(mean_absolute_error(y_test, y_pred)),
        "rmse": float(np.sqrt(mean_squared_error(y_test, y_pred))),
        "r2": float(r2_score(y_test, y_pred)),
        "n_train": len(X_train),
        "n_test": len(X_test),
    }

    return {
        "model": model,
        "metrics": metrics,
        "algorithm": algorithm,
        "property_name": property_name,
    }
