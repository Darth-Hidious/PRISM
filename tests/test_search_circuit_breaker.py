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


def _raising_provider(pid: str):
    """A provider whose search raises, the way the offline socket guard does."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class P(Provider):
        id = pid
        name = pid
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):
            raise ConnectionError("offline mode: external DNS/network access blocked")

    return P()


def _engine_with(health):
    from app.tools.search_engine.cache.engine import SearchCache
    from app.tools.search_engine.engine import SearchEngine
    from app.tools.search_engine.providers.registry import ProviderRegistry

    reg = ProviderRegistry()
    reg.register(_raising_provider("p"))
    return SearchEngine(
        registry=reg, cache=SearchCache(disk_dir=None), health_manager=health
    )


def test_hard_offline_never_penalises_provider_health(monkeypatch):
    """A refusal by the offline policy says nothing about the provider.

    It used to say plenty. The engine's failure branch called
    `record_failure()` for ANY exception, and the offline socket guard
    (`app/tools/_offline.py`) surfaces as one. `record_failure` opens the
    circuit at `consecutive_failures >= 2`, so TWO searches under
    PRISM_OFFLINE=1 opened the circuit for every provider — measured on the
    real health file: 50 open circuits before, 53 after, with
    `matcloud.mc3d-pbesol-v1` at exactly 2.

    That state persists to `~/.prism/cache/provider_health.json`, so coming
    back online meant a 300s cooldown per provider caused by a policy decision
    rather than any provider fault — hard offline silently degrading the
    product's later ONLINE behaviour.

    Drives the real `SearchEngine.search()` fan-out, not a re-implementation of
    its rule: an earlier version of this test asserted on the helper and a
    local HealthManager, and a mutation that disabled the engine's actual gate
    passed it.
    """
    import asyncio

    from app.tools.search_engine.query import MaterialSearchQuery
    from app.tools.search_engine.resilience.circuit_breaker import HealthManager

    # DISTINCT queries per call: an identical repeat is served from the
    # engine's cache and never reaches the provider, so two identical searches
    # produce only ONE recorded failure — which silently weakened the
    # policy-off half of this test until it was caught.
    queries = [
        MaterialSearchQuery(elements=["Fe"], limit=5),
        MaterialSearchQuery(elements=["Cu"], limit=5),
    ]

    # Policy ON: two searches, and the breaker must be untouched.
    monkeypatch.setenv("PRISM_OFFLINE", "1")
    health = HealthManager(persist_path=None)
    engine = _engine_with(health)
    for query in queries:
        result = asyncio.run(engine.search(query))
    assert health.get("p").consecutive_failures == 0
    assert health.get("p").circuit_state == "closed"
    assert result.query_log[0].status == "offline_blocked"

    # Policy OFF: the SAME failure must still open the circuit, or this would
    # have disabled the breaker outright rather than scoping it.
    monkeypatch.setenv("PRISM_OFFLINE", "0")
    health = HealthManager(persist_path=None)
    engine = _engine_with(health)
    for query in queries:
        result = asyncio.run(engine.search(query))
    assert health.get("p").consecutive_failures >= 2
    assert health.get("p").circuit_state == "open"
    assert result.query_log[0].status != "offline_blocked"


def test_an_offline_refusal_hands_back_the_half_open_probe_slot(monkeypatch):
    """Skipping the breaker must not also skip RELEASING the probe claim.

    `half_open_probe_claimed` means "a probe is in flight", and it was cleared
    only by `record_success`/`record_failure`. So the offline gate — added to
    stop policy refusals poisoning provider health — also stopped the release,
    and stranded the provider: once a cooldown-eligible probe landed while
    offline, `should_query()` returned False for the rest of the PROCESS, even
    back online, even for a provider that would now succeed, surfaced only as
    "No providers available for this query".

    That was worse than the bug the gate was added to fix, and it is the
    default path on a machine whose circuits are already open — which the real
    health file was: 50 of 53.
    """
    import asyncio
    import time

    from app.tools.search_engine.query import MaterialSearchQuery
    from app.tools.search_engine.resilience.circuit_breaker import HealthManager

    health = HealthManager(persist_path=None)
    engine = _engine_with(health)

    # Circuit open, cooldown elapsed: the next query takes the half-open probe.
    h = health.get("p")
    h.circuit_state = "open"
    h.consecutive_failures = 2
    h.last_failure = time.time() - 400  # older than the 300s cooldown

    monkeypatch.setenv("PRISM_OFFLINE", "1")
    asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"], limit=5)))

    h = health.get("p")
    assert h.half_open_probe_claimed is False, (
        "the probe slot was never handed back — this provider is now skipped "
        "for the life of the process, online or not"
    )
    # No probe ran, so nothing is known: the breaker must not have moved either.
    assert h.consecutive_failures == 2
    assert h.should_query() is True


def test_provider_failure_warning_names_the_cause_not_just_the_exception_type():
    """A warning of "failed: RuntimeError" is not actionable.

    Measured live during the T5 baseline run: every federated failure surfaced
    as `Provider 'mp_native' failed: RuntimeError` — no message, no cause. The
    agent could not tell a missing API key from a rate limit from a malformed
    filter, so it could not choose a different route and burned a whole turn
    re-querying. The sanitized one-liner was ALREADY being computed for the
    query log (`ProviderQueryLog.error_message`); it just never reached the
    caller.

    With the cause attached, the same run reported the real reasons — five
    providers returning `400 Bad Request` on an unsupported OPTIMADE field, and
    "MP platform proxy: cannot serve elements-only queries via the
    platform-proxy path" — which is a diagnosis rather than a shrug.
    """
    import asyncio

    from app.tools.search_engine.resilience.circuit_breaker import HealthManager
    from app.tools.search_engine.query import MaterialSearchQuery

    engine = _engine_with(HealthManager(persist_path=None))
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"], limit=5)))

    warning = next((w for w in result.warnings if "Provider 'p' failed" in w), None)
    assert warning is not None, f"expected a provider-failure warning: {result.warnings}"
    assert "ConnectionError" in warning, "the exception type is still useful"
    assert "offline mode" in warning, (
        "the CAUSE must ride the warning — without it the agent only learns that "
        f"something threw: {warning!r}"
    )
