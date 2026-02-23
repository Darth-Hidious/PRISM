"""Visualization helpers for ML model results."""
from typing import Dict, List

import numpy as np


def plot_parity(
    y_true: np.ndarray,
    y_pred: np.ndarray,
    property_name: str,
    output_path: str = "parity.png",
) -> Dict:
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        fig, ax = plt.subplots(figsize=(6, 6))
        ax.scatter(y_true, y_pred, alpha=0.6, s=20)
        lims = [min(y_true.min(), y_pred.min()), max(y_true.max(), y_pred.max())]
        ax.plot(lims, lims, "k--", alpha=0.5, label="Perfect")
        ax.set_xlabel(f"Actual {property_name}")
        ax.set_ylabel(f"Predicted {property_name}")
        ax.set_title(f"Parity Plot: {property_name}")
        ax.legend()
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed"}
    except Exception as e:
        return {"error": str(e)}


def plot_feature_importance(
    feature_names: List[str],
    importances: List[float],
    output_path: str = "feature_importance.png",
    top_n: int = 20,
) -> Dict:
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        pairs = sorted(zip(importances, feature_names), reverse=True)[:top_n]
        vals, names = zip(*pairs)

        fig, ax = plt.subplots(figsize=(8, max(4, len(names) * 0.3)))
        ax.barh(range(len(names)), vals, align="center")
        ax.set_yticks(range(len(names)))
        ax.set_yticklabels(names, fontsize=8)
        ax.invert_yaxis()
        ax.set_xlabel("Importance")
        ax.set_title(f"Top {len(names)} Feature Importances")
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed"}
    except Exception as e:
        return {"error": str(e)}
