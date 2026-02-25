import time


def test_health_starts_closed():
    from app.search.resilience.circuit_breaker import ProviderHealth
    h = ProviderHealth(provider_id="mp")
    assert h.circuit_state == "closed"
    assert h.should_query() is True


def test_circuit_opens_after_failures():
    from app.search.resilience.circuit_breaker import ProviderHealth
    h = ProviderHealth(provider_id="aflow")
    h.record_failure()
    h.record_failure()
    assert h.should_query() is True  # 2 failures, still closed
    h.record_failure()
    assert h.circuit_state == "open"
    assert h.should_query() is False


def test_circuit_half_open_after_cooldown():
    from app.search.resilience.circuit_breaker import ProviderHealth
    h = ProviderHealth(provider_id="aflow")
    for _ in range(3):
        h.record_failure()
    assert h.should_query() is False
    # Simulate cooldown passed
    h.last_failure = time.time() - 120
    assert h.should_query(cooldown_seconds=60) is True
    assert h.circuit_state == "half_open"


def test_success_closes_circuit():
    from app.search.resilience.circuit_breaker import ProviderHealth
    h = ProviderHealth(provider_id="aflow")
    for _ in range(3):
        h.record_failure()
    h.last_failure = time.time() - 120
    h.should_query(cooldown_seconds=60)  # moves to half_open
    h.record_success(200.0)
    assert h.circuit_state == "closed"
    assert h.consecutive_failures == 0


def test_avg_latency_tracking():
    from app.search.resilience.circuit_breaker import ProviderHealth
    h = ProviderHealth(provider_id="mp")
    h.record_success(100.0)
    h.record_success(200.0)
    assert h.avg_latency_ms > 0


def test_health_manager_load_save(tmp_path):
    from app.search.resilience.circuit_breaker import HealthManager
    mgr = HealthManager(persist_path=tmp_path / "health.json")
    mgr.get("mp").record_success(100.0)
    mgr.get("aflow").record_failure()
    mgr.save()
    # Reload
    mgr2 = HealthManager(persist_path=tmp_path / "health.json")
    mgr2.load()
    assert mgr2.get("mp").success_count == 1
    assert mgr2.get("aflow").failure_count == 1
