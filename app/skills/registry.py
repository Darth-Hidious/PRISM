"""Built-in skill registry: loads all PRISM skills."""

from app.skills.base import SkillRegistry


def load_builtin_skills() -> SkillRegistry:
    """Register all built-in skills and return the registry."""
    registry = SkillRegistry()

    from app.skills.acquisition import ACQUIRE_SKILL
    from app.skills.prediction import PREDICT_SKILL
    from app.skills.visualization import VISUALIZE_SKILL
    from app.skills.reporting import REPORT_SKILL
    from app.skills.selection import SELECT_SKILL
    from app.skills.discovery import DISCOVER_SKILL
    from app.skills.simulation_plan import SIM_PLAN_SKILL
    from app.skills.phase_analysis import PHASE_ANALYSIS_SKILL
    from app.skills.validation import VALIDATE_SKILL
    from app.skills.review import REVIEW_SKILL

    registry.register(ACQUIRE_SKILL)
    registry.register(PREDICT_SKILL)
    registry.register(VISUALIZE_SKILL)
    registry.register(REPORT_SKILL)
    registry.register(SELECT_SKILL)
    registry.register(DISCOVER_SKILL)
    registry.register(SIM_PLAN_SKILL)
    registry.register(PHASE_ANALYSIS_SKILL)
    registry.register(VALIDATE_SKILL)
    registry.register(REVIEW_SKILL)

    return registry
