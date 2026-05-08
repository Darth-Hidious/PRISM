"""Built-in skill registry: loads all PRISM skills.

NOTE: The dataset-shaped skills (VALIDATE_SKILL / REVIEW_SKILL /
VISUALIZE_SKILL) were collapsed into the unified `dataset` Tool in
Round 6. They are NOT registered here anymore — the underlying
implementations stay in their respective files and are dispatched
via app/tools/dataset.py. See that file's docstring for rationale.
"""

from app.tools.skills.base import SkillRegistry


def load_builtin_skills() -> SkillRegistry:
    """Register all built-in skills and return the registry."""
    registry = SkillRegistry()

    from app.tools.skills.acquisition import ACQUIRE_SKILL
    from app.tools.skills.prediction import PREDICT_SKILL
    from app.tools.skills.reporting import REPORT_SKILL
    from app.tools.skills.selection import SELECT_SKILL
    from app.tools.skills.discovery import DISCOVER_SKILL
    from app.tools.skills.simulation_plan import SIM_PLAN_SKILL
    from app.tools.skills.phase_analysis import PHASE_ANALYSIS_SKILL

    registry.register(ACQUIRE_SKILL)
    registry.register(PREDICT_SKILL)
    registry.register(REPORT_SKILL)
    registry.register(SELECT_SKILL)
    registry.register(DISCOVER_SKILL)
    registry.register(SIM_PLAN_SKILL)
    registry.register(PHASE_ANALYSIS_SKILL)

    return registry
