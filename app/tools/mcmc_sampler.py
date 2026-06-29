"""MCMC alloy sampler — lightweight, torch-free alternative to GFlowNet.

Uses Metropolis-Hastings with physics-descriptor rewards (delta, VEC,
entropy, density, melting point, electronegativity spread). No neural
network, no torch — just numpy. This is the default local sampler.

The reward function is a weighted sum of desirability scores from
PhysicsSurrogate. The proposal distribution is a Gaussian random walk
in composition space (normalized to sum to 1).

This gives decent diversity for cold-start exploration without the
21K-token torch dependency. GFlowNet remains available as a premium
marketplace tool for users who want learned, diversity-optimized sampling.
"""

from __future__ import annotations

import numpy as np

from app.tools.gflownet.spaces import AlloyDesignSpace, ELEMENT_DATA
from app.tools.gflownet.surrogates.physics import PhysicsSurrogate


def mcmc_sample(
    space: AlloyDesignSpace,
    surrogate: PhysicsSurrogate,
    weights: np.ndarray,
    n_samples: int = 64,
    n_burn: int = 200,
    n_steps: int = 1000,
    step_size: float = 0.05,
    seed: int | None = None,
) -> tuple[np.ndarray, np.ndarray]:
    """Metropolis-Hastings sampling of alloy compositions.

    Args:
        space: Alloy design space (elements, n_units).
        surrogate: Physics surrogate for reward computation.
        weights: Preference weights for each objective.
        n_samples: Number of accepted samples to collect.
        n_burn: Burn-in steps before collecting.
        n_steps: Max total steps (burn + sample).
        step_size: Gaussian proposal std in fraction space.
        seed: Random seed for reproducibility.

    Returns:
        (X, R) where X is (n_samples, n_elements) fractions and
        R is (n_samples,) reward values.
    """
    rng = np.random.default_rng(seed)
    n = space.n_elements

    # Start from uniform composition
    x = np.ones(n) / n

    # Compute reward for current state
    pred = surrogate.predict(x.reshape(1, -1))
    r = _scalar_reward(pred, weights)

    samples: list[np.ndarray] = []
    rewards: list[float] = []

    total = n_burn + n_steps
    accepted = 0

    for i in range(total):
        # Gaussian proposal
        x_new = x + rng.normal(0, step_size, size=n)
        # Clip to [0, inf) and renormalize
        x_new = np.clip(x_new, 0, None)
        s = x_new.sum()
        if s == 0:
            continue
        x_new = x_new / s

        # Compute reward for proposal
        pred_new = surrogate.predict(x_new.reshape(1, -1))
        r_new = _scalar_reward(pred_new, weights)

        # Metropolis acceptance: accept if better, or with prob exp(ΔR)
        log_alpha = r_new - r
        if log_alpha >= 0 or rng.random() < np.exp(log_alpha):
            x = x_new
            r = r_new
            accepted += 1

        # Collect after burn-in
        if i >= n_burn and (i - n_burn) % max(1, (n_steps // n_samples)) == 0:
            samples.append(x.copy())
            rewards.append(r)

        if len(samples) >= n_samples:
            break

    if not samples:
        # Fallback: return the current state
        samples = [x]
        rewards = [r]

    X = np.array(samples)
    R = np.array(rewards)
    return X, R


def _scalar_reward(pred, weights: np.ndarray) -> float:
    """Scalarize multi-objective predictions into a single reward.

    Normalizes each descriptor to [0, 1] using reasonable ranges so
    no single objective dominates (e.g. tm ~ 3000K vs delta ~ 3%).
    """
    # Normalization ranges (typical for structural/refractory alloys)
    RANGES = {
        "delta": (0.0, 10.0),  # size mismatch %
        "vec": (3.0, 12.0),  # valence electron concentration
        "entropy": (0.0, 15.0),  # J/mol/K (ln(16) * R ~ 23 max)
        "density": (2.0, 20.0),  # g/cm^3
        "tm": (900.0, 3700.0),  # K
        "dchi": (0.0, 1.0),  # electronegativity spread
    }

    desirs = []
    for key in pred.mean:
        vals = pred.mean[key]
        v = float(vals[0]) if hasattr(vals, "__len__") else float(vals)
        lo, hi = RANGES.get(key, (0.0, 1.0))
        # Normalize to [0, 1]
        norm = (v - lo) / max(hi - lo, 1e-6)
        norm = np.clip(norm, 0, 1)
        desirs.append(norm)

    desirs = np.array(desirs)
    if len(desirs) != len(weights):
        if len(desirs) < len(weights):
            desirs = np.pad(desirs, (0, len(weights) - len(desirs)))
        else:
            desirs = desirs[: len(weights)]

    w = weights / weights.sum()
    return float(np.dot(desirs, w))


def mcmc_discover(
    space: AlloyDesignSpace,
    surrogate: PhysicsSurrogate,
    weights: np.ndarray,
    n_rounds: int = 5,
    batch_size: int = 16,
    n_burn: int = 100,
    n_steps: int = 500,
    seed: int | None = None,
) -> dict:
    """Multi-round MCMC discovery with adaptive step size.

    Each round: sample → evaluate → narrow step size around best.
    Returns the best compositions found and their rewards.
    """
    rng = np.random.default_rng(seed)
    all_X = []
    all_R = []
    step = 0.1

    for round_idx in range(n_rounds):
        X, R = mcmc_sample(
            space,
            surrogate,
            weights,
            n_samples=batch_size,
            n_burn=n_burn,
            n_steps=n_steps,
            step_size=step,
            seed=seed + round_idx if seed else None,
        )
        all_X.append(X)
        all_R.append(R)

        # Adapt: narrow step size around best samples
        best_idx = np.argmax(R)
        if R[best_idx] > 0:
            step *= 0.8  # narrow search

    X_all = np.vstack(all_X)
    R_all = np.concatenate(all_R)

    # Deduplicate and rank
    order = np.argsort(-R_all)
    X_sorted = X_all[order]
    R_sorted = R_all[order]

    # Remove near-duplicates (within step_size in L2)
    unique = [0]
    for i in range(1, len(X_sorted)):
        is_dup = any(np.linalg.norm(X_sorted[i] - X_sorted[j]) < 0.05 for j in unique)
        if not is_dup:
            unique.append(i)

    return {
        "X": X_sorted[unique],
        "R": R_sorted[unique],
        "n_total": len(X_all),
        "n_unique": len(unique),
        "n_rounds": n_rounds,
    }
