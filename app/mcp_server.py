"""PRISM MCP Server — exposes all tools to external MCP hosts."""
import json
import inspect
from typing import Annotated, Optional

from pydantic import Field
from fastmcp import FastMCP

from app.tools.base import Tool, ToolRegistry
from app.tools.data import create_data_tools
from app.tools.system import create_system_tools
from app.tools.visualization import create_visualization_tools
from app.tools.prediction import create_prediction_tools
from app.agent.memory import SessionMemory
from app.simulation.bridge import check_pyiron_available


def _build_registry() -> ToolRegistry:
    """Build a full tool registry with all PRISM tools and skills."""
    registry = ToolRegistry()
    create_system_tools(registry)
    create_data_tools(registry)
    create_visualization_tools(registry)
    create_prediction_tools(registry)
    if check_pyiron_available():
        from app.tools.simulation import create_simulation_tools
        create_simulation_tools(registry)
    # Load built-in skills as tools
    try:
        from app.skills.registry import load_builtin_skills
        load_builtin_skills().register_all_as_tools(registry)
    except Exception:
        pass
    return registry


# Type mapping from JSON schema to Python types
_JSON_TO_PYTHON_TYPE = {
    "string": str,
    "integer": int,
    "number": float,
    "boolean": bool,
    "array": list,
    "object": dict,
}


def _make_typed_handler(tool: Tool):
    """Generate a typed handler from a Tool's JSON schema for FastMCP.

    FastMCP requires functions with explicit typed parameters (no **kwargs).
    We create a function with the correct inspect.Parameter signature
    built from the tool's input_schema. Uses Annotated[type, Field(description=...)]
    to pass parameter descriptions through to the MCP schema.
    """
    properties = tool.input_schema.get("properties", {})
    required = set(tool.input_schema.get("required", []))

    # Build inspect.Parameter list for the signature
    params = []
    annotations = {}
    for pname, pdef in properties.items():
        base_type = _JSON_TO_PYTHON_TYPE.get(pdef.get("type", "string"), str)
        desc = pdef.get("description", "")

        # Use Annotated[type, Field(...)] to pass descriptions to FastMCP
        if desc:
            ann_type = Annotated[base_type, Field(description=desc)]
        else:
            ann_type = base_type

        if pname in required:
            p = inspect.Parameter(
                pname,
                inspect.Parameter.POSITIONAL_OR_KEYWORD,
                annotation=ann_type,
            )
        else:
            p = inspect.Parameter(
                pname,
                inspect.Parameter.POSITIONAL_OR_KEYWORD,
                default=pdef.get("default", None),
                annotation=ann_type,
            )
        params.append(p)
        annotations[pname] = ann_type

    sig = inspect.Signature(params, return_annotation=str)

    # Create the wrapper function
    execute_fn = tool.execute

    def handler(*args, **kwargs):
        bound = sig.bind(*args, **kwargs)
        bound.apply_defaults()
        # Filter out None values (optional params not provided)
        filtered = {k: v for k, v in bound.arguments.items() if v is not None}
        result = execute_fn(**filtered)
        return json.dumps(result, default=str)

    # Set metadata that FastMCP reads
    handler.__name__ = tool.name
    handler.__qualname__ = tool.name
    handler.__doc__ = tool.description
    handler.__signature__ = sig
    handler.__annotations__ = annotations
    handler.__annotations__["return"] = str

    return handler


def _register_resources(mcp: FastMCP):
    """Register MCP resources for PRISM data."""

    @mcp.resource("prism://sessions")
    def list_sessions() -> str:
        """List saved PRISM sessions."""
        memory = SessionMemory()
        sessions = memory.list_sessions()
        return json.dumps(sessions, default=str)

    @mcp.resource("prism://tools")
    def list_tools_resource() -> str:
        """List all available PRISM tools."""
        registry = _build_registry()
        tools = [
            {"name": t.name, "description": t.description}
            for t in registry.list_tools()
        ]
        return json.dumps(tools)

    @mcp.resource("prism://datasets")
    def list_datasets() -> str:
        """List collected materials datasets."""
        from app.data.store import DataStore
        store = DataStore()
        return json.dumps(store.list_datasets(), default=str)

    @mcp.resource("prism://models")
    def list_trained_models() -> str:
        """List trained ML models and their metrics."""
        from app.ml.registry import ModelRegistry
        registry = ModelRegistry()
        return json.dumps(registry.list_models(), default=str)

    @mcp.resource("prism://datasets/{name}")
    def get_dataset(name: str) -> str:
        """Get a specific dataset's metadata and preview."""
        from app.data.store import DataStore
        store = DataStore()
        df = store.load(name)
        return json.dumps(
            {
                "name": name,
                "rows": len(df),
                "columns": list(df.columns),
                "preview": df.head(10).to_dict(orient="records"),
            },
            default=str,
        )

    @mcp.resource("prism://skills")
    def list_skills_resource() -> str:
        """List available PRISM skills with their steps."""
        try:
            from app.skills.registry import load_builtin_skills
            skills = load_builtin_skills()
            data = []
            for s in skills.list_skills():
                data.append({
                    "name": s.name,
                    "description": s.description,
                    "category": s.category,
                    "steps": [
                        {"name": st.name, "description": st.description, "optional": st.optional}
                        for st in s.steps
                    ],
                })
            return json.dumps(data)
        except Exception:
            return json.dumps([])

    # --- Simulation resources (only when pyiron is available) ----------------
    if check_pyiron_available():
        @mcp.resource("prism://simulations/structures")
        def list_structures() -> str:
            """List stored atomistic structures."""
            from app.simulation.bridge import get_bridge
            bridge = get_bridge()
            return json.dumps(bridge.structures.to_summary_list(), default=str)

        @mcp.resource("prism://simulations/jobs")
        def list_sim_jobs() -> str:
            """List simulation jobs."""
            from app.simulation.bridge import get_bridge
            bridge = get_bridge()
            return json.dumps(bridge.jobs.to_summary_list(), default=str)

        @mcp.resource("prism://simulations/jobs/{job_id}")
        def get_sim_job(job_id: str) -> str:
            """Get details of a specific simulation job."""
            from app.simulation.bridge import get_bridge
            bridge = get_bridge()
            job = bridge.jobs.get(job_id)
            if job is None:
                return json.dumps({"error": f"Job {job_id} not found"})
            info = {
                "job_id": job_id,
                "status": str(job.status),
                "job_name": getattr(job, "job_name", job_id),
            }
            return json.dumps(info, default=str)


def create_mcp_server(registry: Optional[ToolRegistry] = None) -> FastMCP:
    """Create a FastMCP server exposing all PRISM tools and resources."""
    mcp = FastMCP(
        name="prism",
        instructions=(
            "PRISM: Materials science research tools. Search OPTIMADE databases, "
            "query Materials Project, predict properties with ML, visualize results, "
            "export data, and run atomistic simulations via pyiron."
        ),
    )

    if registry is None:
        registry = _build_registry()

    # Register each tool from ToolRegistry as a FastMCP tool
    for tool in registry.list_tools():
        handler = _make_typed_handler(tool)
        mcp.add_tool(handler)

    # Register resources
    _register_resources(mcp)

    return mcp


def generate_claude_desktop_config() -> dict:
    """Generate claude_desktop_config.json entry for PRISM."""
    import sys

    return {
        "mcpServers": {
            "prism": {
                "command": sys.executable,
                "args": ["-m", "app.cli", "serve"],
            }
        }
    }
