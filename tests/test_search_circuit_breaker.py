import time


def test_health_starts_closed():
    from app.tools.search_engine.resilience.circuit_breaker import ProviderHealth

    h = ProviderHealth(provider_id="mp")
    assert h.circuit_state == "closed"
    assert h.should_query() is True


def test_circuit_opens_after_failures():
    from app.tools.search_engine.resilience.circuit_breaker import ProviderHealth

    h = ProviderHealth(provider_id="aflow")
    h.record_failure()
    assert h.should_query() is True  # 1 failure, still closed
    h.record_failure()
    assert h.circuit_state == "open"  # opens after 2 consecutive failures
    assert h.should_query() is False


def test_circuit_half_open_after_cooldown():
    from app.tools.search_engine.resilience.circuit_breaker import ProviderHealth

    h = ProviderHealth(provider_id="aflow")
    for _ in range(2):
        h.record_failure()
    assert h.should_query() is False
    # Simulate cooldown passed
    h.last_failure = time.time() - 120
    assert h.should_query(cooldown_seconds=60) is True
    assert h.circuit_state == "half_open"


def test_success_closes_circuit():
    from app.tools.search_engine.resilience.circuit_breaker import ProviderHealth

    h = ProviderHealth(provider_id="aflow")
    for _ in range(2):
        h.record_failure()
    h.last_failure = time.time() - 120
    h.should_query(cooldown_seconds=60)  # moves to half_open
    h.record_success(200.0)
    assert h.circuit_state == "closed"
    assert h.consecutive_failures == 0


def test_avg_latency_tracking():
    from app.tools.search_engine.resilience.circuit_breaker import ProviderHealth

    h = ProviderHealth(provider_id="mp")
    h.record_success(100.0)
    h.record_success(200.0)
    assert h.avg_latency_ms > 0


def test_health_manager_load_save(tmp_path):
    from app.tools.search_engine.resilience.circuit_breaker import HealthManager

    mgr = HealthManager(persist_path=tmp_path / "health.json")
    mgr.get("mp").record_success(100.0)
    mgr.get("aflow").record_failure()
    mgr.save()
    # Reload
    mgr2 = HealthManager(persist_path=tmp_path / "health.json")
    mgr2.load()
    assert mgr2.get("mp").success_count == 1
    assert mgr2.get("aflow").failure_count == 1


def test_half_open_allows_only_one_concurrent_probe():
    """S6: when a circuit transitions open→half_open, concurrent callers must
    NOT all probe at once (thundering herd). Only the first claims the probe;
    the rest skip until the probe's result clears the claim."""
    from app.tools.search_engine.resilience.circuit_breaker import ProviderHealth

    h = ProviderHealth(provider_id="suspect")
    for _ in range(2):
        h.record_failure()
    assert h.circuit_state == "open"
    # Simulate cooldown passed — first should_query flips to half_open + claims
    h.last_failure = time.time() - 120
    first = h.should_query(cooldown_seconds=60)
    assert first is True
    assert h.circuit_state == "half_open"
    assert h.half_open_probe_claimed is True
    # Concurrent callers (same window) must skip — only ONE probe allowed
    second = h.should_query(cooldown_seconds=60)
    third = h.should_query(cooldown_seconds=60)
    assert second is False, "concurrent half-open caller must skip (probe already in flight)"
    assert third is False
    # A success clears the claim and closes the circuit
    h.record_success(150.0)
    assert h.circuit_state == "closed"
    assert h.half_open_probe_claimed is False
    assert h.should_query() is True


def test_half_open_claim_cleared_on_failure():
    """A failed probe re-opens the circuit AND clears the claim so the next
    cooldown window can probe again."""
    from app.tools.search_engine.resilience.circuit_breaker import ProviderHealth

    h = ProviderHealth(provider_id="dead")
    for _ in range(2):
        h.record_failure()
    h.last_failure = time.time() - 120
    assert h.should_query(cooldown_seconds=60) is True  # claims the probe
    assert h.half_open_probe_claimed is True
    h.record_failure()  # probe failed
    assert h.circuit_state == "open"
    assert h.half_open_probe_claimed is False


def test_half_open_claim_not_persisted():
    """The in-flight claim is per-process — it must NOT survive save/load
    (a fresh process has no in-flight probe, so it should re-probe)."""
    from app.tools.search_engine.resilience.circuit_breaker import HealthManager

    mgr = HealthManager(persist_path=None)
    h = mgr.get("x")
    h.half_open_probe_claimed = True
    # to_dict must not include the claim
    assert "half_open_probe_claimed" not in h.to_dict()

