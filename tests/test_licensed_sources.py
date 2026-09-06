"""Licensed-source resolution tests. Every TDB here is synthetic metadata."""

import json
import sys
import types
from pathlib import Path

import numpy as np

from app.tools.calphad import _calphad_compute
from app.tools.evidence import EvidenceClass
from app.tools.licensed_sources import (
    LicensedSource,
    LicensedSourceResolver,
    LocalFileSourceProvider,
    PlatformSourceProvider,
    ProviderResponse,
    SourceCoverage,
    SourceOrigin,
    SourceRequest,
    SourceType,
)
from app.tools.simulation.calphad_bridge import CalphadBridge


class RecordingProvider:
    def __init__(self, *sources: LicensedSource):
        self.sources = sources
        self.requests: list[SourceRequest] = []

    def find(self, request: SourceRequest) -> ProviderResponse:
        self.requests.append(request)
        return ProviderResponse(self.sources)


def _source(
    path: Path,
    *,
    source_id: str,
    elements: list[str],
    systems: list[list[str]],
    origin: SourceOrigin,
    evidence_class: EvidenceClass = EvidenceClass.SCREENING,
) -> LicensedSource:
    return LicensedSource(
        source_id=source_id,
        source_type=SourceType.THERMODYNAMIC_DATABASE,
        name=f"Synthetic {source_id}",
        version="test-version",
        licence="Synthetic test licence",
        evidence_class=evidence_class,
        coverage=SourceCoverage.from_mapping(
            {"elements": elements, "systems": systems}
        ),
        origin=origin,
        access_kind="file",
        path=path,
    )


def _write_local_config(
    config_path: Path,
    tdb_path: Path,
    *,
    source_id: str = "synthetic-al-ni",
    elements: list[str] | None = None,
    systems: list[list[str]] | None = None,
    evidence_class: str = "screening",
) -> None:
    config_path.write_text(
        json.dumps(
            {
                "sources": [
                    {
                        "id": source_id,
                        "type": "thermodynamic_database",
                        "name": f"Synthetic {source_id}",
                        "version": "test-version",
                        "licence": "Synthetic test licence",
                        "evidence_class": evidence_class,
                        "path": str(tdb_path),
                        "coverage": {
                            "elements": elements or ["Al", "Ni"],
                            "systems": systems or [["Al", "Ni"]],
                        },
                    }
                ]
            }
        )
    )


def _numeric_leaves(value):
    if isinstance(value, dict):
        for item in value.values():
            yield from _numeric_leaves(item)
    elif isinstance(value, list):
        for item in value:
            yield from _numeric_leaves(item)
    elif isinstance(value, (int, float)) and not isinstance(value, bool):
        yield value


def test_request_outside_coverage_returns_structured_refusal_without_number(
    tmp_path, monkeypatch
):
    """Mo-Nb-Ta-W-Hf outside declared coverage is a fact, not a calculation."""
    synthetic_tdb = tmp_path / "synthetic-fragmented-hea.tdb"
    synthetic_tdb.write_text("$ synthetic test TDB; no licensed data\n")
    config = tmp_path / "licensed_sources.json"
    # Every requested element is present, but only binary systems are declared.
    # Element presence must not be mistaken for five-component validation.
    _write_local_config(
        config,
        synthetic_tdb,
        source_id="synthetic-fragmented-hea",
        elements=["Mo", "Nb", "Ta", "W", "Hf"],
        systems=[["Mo", "Nb"], ["Ta", "W"], ["Hf", "Mo"]],
    )

    resolver = LicensedSourceResolver(
        local_provider=LocalFileSourceProvider(config),
        remote_provider=RecordingProvider(),
    )
    monkeypatch.setattr("app.tools.licensed_sources._RESOLVER", resolver)
    # `calphad_compute` checks the CALPHAD engine before it resolves a source
    # (an engine-less install must be told so, not sent to buy a TDB licence).
    # This test is about the coverage refusal, so assert the engine is there
    # and keep the assertion exercising the resolver on any interpreter.
    monkeypatch.setattr(
        "app.tools.simulation.calphad_bridge.check_calphad_available",
        lambda: True,
    )
    # `_delegate` bound the same name at import; patch that binding too, or a
    # pycalphad-less interpreter answers "sidecar not provisioned" instead.
    monkeypatch.setattr("app.tools.calphad.check_calphad_available", lambda: True)

    result = _calphad_compute(
        action="equilibrium",
        components=["Mo", "Nb", "Ta", "W", "Hf"],
        conditions={"T": 1300, "P": 101325},
    )

    assert result["status"] == "refused"
    assert result["error"] == "No validated TDB covers Mo-Nb-Ta-W-Hf"
    assert result["refusal"]["code"] == "licensed_source_coverage_gap"
    assert result["refusal"]["coverage_gap"]["systems"] == ["Mo-Nb-Ta-W-Hf"]
    checked_gap = result["refusal"]["sources_checked"][0]["gap"]
    assert checked_gap["missing_elements"] == []
    assert checked_gap["uncovered_systems"] == ["Mo-Nb-Ta-W-Hf"]
    assert "install_hint" in result
    assert "result" not in result
    assert list(_numeric_leaves(result)) == []


def test_resolution_order_is_local_then_entitled_remote(tmp_path):
    local_tdb = tmp_path / "local.tdb"
    local_tdb.write_text("$ synthetic local TDB\n")
    remote_tdb = tmp_path / "remote-mounted.tdb"
    remote_tdb.write_text("$ synthetic mounted TDB\n")
    config = tmp_path / "licensed_sources.json"
    _write_local_config(config, local_tdb)

    remote = RecordingProvider(
        _source(
            remote_tdb,
            source_id="synthetic-remote-hea",
            elements=["Mo", "Nb", "Ta", "W", "Hf"],
            systems=[["Mo", "Nb", "Ta", "W", "Hf"]],
            origin=SourceOrigin.ENTITLED_REMOTE,
        )
    )
    resolver = LicensedSourceResolver(LocalFileSourceProvider(config), remote)

    local_result = resolver.resolve(
        SourceRequest.thermodynamic_database(["Al", "Ni"])
    )
    assert isinstance(local_result, LicensedSource)
    assert local_result.origin == SourceOrigin.LOCAL_FILE
    assert remote.requests == []

    remote_result = resolver.resolve(
        SourceRequest.thermodynamic_database(["Mo", "Nb", "Ta", "W", "Hf"])
    )
    assert isinstance(remote_result, LicensedSource)
    assert remote_result.origin == SourceOrigin.ENTITLED_REMOTE
    assert len(remote.requests) == 1


def test_source_type_axis_supports_measured_property_datasets(tmp_path):
    synthetic_dataset = tmp_path / "synthetic-properties.csv"
    synthetic_dataset.write_text("material,creep_rate\nsynthetic,0\n")
    config = tmp_path / "licensed_sources.json"
    config.write_text(
        json.dumps(
            {
                "sources": [
                    {
                        "id": "synthetic-property-signals",
                        "type": "property_dataset",
                        "name": "Synthetic measured properties",
                        "version": "test-version",
                        "licence": "Synthetic test licence",
                        "evidence_class": "reference_validated",
                        "path": str(synthetic_dataset),
                        "coverage": {
                            "elements": ["Ni"],
                            "property_domains": ["creep"],
                        },
                    }
                ]
            }
        )
    )
    resolver = LicensedSourceResolver(
        LocalFileSourceProvider(config), RecordingProvider()
    )
    request = SourceRequest(
        source_type=SourceType.PROPERTY_DATASET,
        elements=("Ni",),
        property_domains=("creep",),
    )

    result = resolver.resolve(request)

    assert isinstance(result, LicensedSource)
    assert result.source_type == SourceType.PROPERTY_DATASET
    assert "database" not in result.provenance()


def test_platform_request_cannot_assert_remote_entitlement(tmp_path):
    mounted_tdb = tmp_path / "platform-mounted.tdb"
    mounted_tdb.write_text("$ synthetic platform-mounted TDB\n")

    class FakePlatformClient:
        def __init__(self):
            self.body = None

        def post(self, path, *, json, timeout):
            self.body = json
            return {
                "sources": [
                    {
                        "id": "denied",
                        "type": "thermodynamic_database",
                        "name": "Denied source",
                        "version": "test-version",
                        "licence": "Synthetic test licence",
                        "evidence_class": "screening",
                        "entitled": False,
                        "coverage": {
                            "elements": ["Al", "Ni"],
                            "systems": [["Al", "Ni"]],
                        },
                        "access": {"kind": "file", "path": str(mounted_tdb)},
                    },
                    {
                        "id": "server-entitled",
                        "type": "thermodynamic_database",
                        "name": "Server-entitled source",
                        "version": "test-version",
                        "licence": "Synthetic test licence",
                        "evidence_class": "screening",
                        "entitled": True,
                        "coverage": {
                            "elements": ["Al", "Ni"],
                            "systems": [["Al", "Ni"]],
                        },
                        "access": {"kind": "file", "path": str(mounted_tdb)},
                    },
                ]
            }

    client = FakePlatformClient()
    response = PlatformSourceProvider(client).find(
        SourceRequest.thermodynamic_database(["Al", "Ni"])
    )

    assert "entitled" not in client.body
    assert "project_id" not in client.body
    assert [source.source_id for source in response.sources] == ["server-entitled"]
    assert any("did not confirm entitlement" in error for error in response.errors)


def test_overdeclared_elements_are_excluded_not_trusted(tmp_path):
    """A declaration is held to the file it names, not taken at its word."""
    open_tdb = tmp_path / "open-ni-co-cr.tdb"
    open_tdb.write_text(
        "$ synthetic test TDB; no licensed data\n"
        "ELEMENT /- ELECTRON_GAS 0.0000E+00 0.0000E+00 0.0000E+00 !\n"
        "ELEMENT VA VACUUM 0.0000E+00 0.0000E+00 0.0000E+00 !\n"
        "ELEMENT NI FCC_A1 5.8690E+01 4.7870E+03 2.9796E+01 !\n"
        "ELEMENT CO HCP_A3 5.8933E+01 4.7656E+03 3.0040E+01 !\n"
        "ELEMENT CR BCC_A2 5.1996E+01 4.0500E+03 2.3560E+01 !\n"
    )
    config = tmp_path / "licensed_sources.json"
    _write_local_config(
        config,
        open_tdb,
        source_id="claims-to-be-tchea",
        elements=["Ni", "Co", "Cr", "Ta", "Re"],
        systems=[["Ni", "Co", "Cr", "Ta", "Re"]],
        evidence_class="reference_validated",
    )

    response = LocalFileSourceProvider(config).find(
        SourceRequest.thermodynamic_database(["Ni", "Ta"])
    )

    assert response.sources == ()
    assert len(response.errors) == 1
    assert "Ta" in response.errors[0]
    assert "Re" in response.errors[0]


def test_unverifiable_stub_is_capped_at_screening(tmp_path):
    stub_tdb = tmp_path / "stub.tdb"
    stub_tdb.write_text("$ synthetic\n")
    config = tmp_path / "licensed_sources.json"
    _write_local_config(config, stub_tdb, evidence_class="reference_validated")

    response = LocalFileSourceProvider(config).find(
        SourceRequest.thermodynamic_database(["Al", "Ni"])
    )

    assert response.errors == ()
    (source,) = response.sources
    assert source.evidence_class == EvidenceClass.SCREENING
    assert source.verification == "unverifiable"
    assert source.provenance()["verification"] == "unverifiable"


def test_matching_declaration_is_marked_elements_match(tmp_path):
    al_ni_tdb = tmp_path / "al-ni.tdb"
    al_ni_tdb.write_text(
        "ELEMENT AL FCC_A1 2.6982E+01 4.5773E+03 2.8322E+01 !\n"
        "ELEMENT NI FCC_A1 5.8690E+01 4.7870E+03 2.9796E+01 !\n"
    )
    config = tmp_path / "licensed_sources.json"
    _write_local_config(config, al_ni_tdb, evidence_class="reference_validated")

    response = LocalFileSourceProvider(config).find(
        SourceRequest.thermodynamic_database(["Al", "Ni"])
    )

    (source,) = response.sources
    assert source.verification == "elements_match"
    assert source.evidence_class == EvidenceClass.REFERENCE_VALIDATED


def test_platform_path_gets_the_same_suffix_check_as_local():
    class FakePlatformClient:
        def post(self, path, *, json, timeout):
            return {
                "sources": [
                    {
                        "id": "server-entitled",
                        "type": "thermodynamic_database",
                        "name": "Server-entitled source",
                        "version": "test-version",
                        "licence": "Synthetic test licence",
                        "evidence_class": "screening",
                        "entitled": True,
                        "coverage": {
                            "elements": ["Al", "Ni"],
                            "systems": [["Al", "Ni"]],
                        },
                        "access": {"kind": "file", "path": "/etc/passwd"},
                    }
                ]
            }

    response = PlatformSourceProvider(FakePlatformClient()).find(
        SourceRequest.thermodynamic_database(["Al", "Ni"])
    )

    assert response.sources == ()
    assert any("/etc/passwd" in error for error in response.errors)


def test_licensed_source_provenance_and_evidence_flow_without_promotion(tmp_path):
    synthetic_tdb = tmp_path / "synthetic.tdb"
    synthetic_tdb.write_text("$ synthetic test TDB; no licensed data\n")
    source = _source(
        synthetic_tdb,
        source_id="synthetic-research-source",
        elements=["Al", "Ni"],
        systems=[["Al", "Ni"]],
        origin=SourceOrigin.LOCAL_FILE,
        evidence_class=EvidenceClass.RESEARCH,
    )

    calculation = types.SimpleNamespace()
    calculation.GM = types.SimpleNamespace(
        values=types.SimpleNamespace(squeeze=lambda: np.array([-4.0, -3.0]))
    )
    fake_pycalphad = types.ModuleType("pycalphad")
    fake_pycalphad.__version__ = "test-version"
    fake_pycalphad.Database = lambda _: object()
    fake_pycalphad.calculate = lambda *args, **kwargs: calculation
    fake_pycalphad.variables = types.SimpleNamespace()

    previous = sys.modules.get("pycalphad")
    sys.modules["pycalphad"] = fake_pycalphad
    try:
        result = CalphadBridge(base_dir=tmp_path / "unused").calculate_gibbs_energy(
            database_name=source.source_id,
            components=["AL", "NI"],
            phases=["FCC_A1"],
            temperature=1000,
            database_path=synthetic_tdb,
            licensed_source=source,
        )
    finally:
        if previous is None:
            sys.modules.pop("pycalphad", None)
        else:
            sys.modules["pycalphad"] = previous

    assert result["gibbs_energies"] == [-4.0, -3.0]
    assert result["evidence_class"] == "research"
    assert result["evidence_color"] == "orange"
    provenance = result["provenance"]
    assert provenance["licensed_source"] == source.provenance()
    database_ref = provenance["wasDerivedFrom"][0]
    assert database_ref["licensed_source"]["source_id"] == source.source_id
    assert database_ref["licensed_source"]["version"] == "test-version"
    assert database_ref["licensed_source"]["licence"] == "Synthetic test licence"
    assert len(database_ref["sha256"]) == 64
