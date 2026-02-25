"""Tests for AgentConfig and AgentRegistry."""


def test_agent_config_defaults():
    from app.agent.agent_registry import AgentConfig

    config = AgentConfig(id="test", name="Test Agent")
    assert config.id == "test"
    assert config.runtime == "local"
    assert config.tools is None
    assert config.max_iterations == 20
    assert config.enabled is True


def test_agent_registry_register_and_get():
    from app.agent.agent_registry import AgentConfig, AgentRegistry

    reg = AgentRegistry()
    config = AgentConfig(
        id="phase",
        name="Phase Agent",
        system_prompt="You are a phase specialist.",
        tools=["phase_diagram", "equilibrium"],
    )
    reg.register(config)
    assert reg.get("phase") is config
    assert reg.get("nonexistent") is None


def test_agent_registry_get_all():
    from app.agent.agent_registry import AgentConfig, AgentRegistry

    reg = AgentRegistry()
    reg.register(AgentConfig(id="a", name="A"))
    reg.register(AgentConfig(id="b", name="B"))
    assert len(reg.get_all()) == 2


def test_agent_config_remote_runtime():
    from app.agent.agent_registry import AgentConfig

    config = AgentConfig(
        id="hpc_sim",
        name="HPC Simulator",
        runtime="remote",
        remote_endpoint="https://hpc.marc27.io/agents/sim",
    )
    assert config.runtime == "remote"
    assert config.remote_endpoint == "https://hpc.marc27.io/agents/sim"
