"""Tests for EasternLiteratureCollector and the alloy-designation module.

Three properties this suite exists to protect:
  1. A gated or failed source is NAMED — never silently indistinguishable
     from a source that ran fine and found nothing.
  2. Cyrillic and CJK survive intact all the way into a stored record.
  3. Alloy equivalences are only asserted with a cited standard; everything
     else is an explicit refusal, never a guess.

The `network` marked tests at the bottom hit the real sources and are skipped
unless PRISM_LIVE_SOURCES=1 — a passing mock is not evidence a source exists.
"""
import os
from unittest.mock import MagicMock, patch

import pytest

from app.tools.data_collectors.alloy_designations import (
    detect_script,
    find_designations,
    infer_language,
    map_designation,
)
from app.tools.data_collectors.eastern_literature_collector import (
    GATED_SOURCES,
    EasternLiteratureCollector,
)

RU_TITLE = "Жаро- и коррозионностойкое покрытие для лопаток из сплава ВЖЛ21"
JA_TITLE = "排気バルブ用ニッケル基高耐熱合金の開発"

OAI_XML = f"""<?xml version="1.0" encoding="UTF-8"?>
<OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/">
 <ListRecords>
  <record><metadata>
   <oai_dc:dc xmlns:oai_dc="http://www.openarchives.org/OAI/2.0/oai_dc/"
              xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>{RU_TITLE}</dc:title>
    <dc:creator>Косьмин А. А.</dc:creator>
    <dc:identifier>https://cyberleninka.ru/article/n/zharo-i-korrozionnostoykoe</dc:identifier>
   </oai_dc:dc>
  </metadata></record>
  <record><metadata>
   <oai_dc:dc xmlns:oai_dc="http://www.openarchives.org/OAI/2.0/oai_dc/"
              xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Совершенно другая тема без совпадений</dc:title>
    <dc:identifier>https://cyberleninka.ru/article/n/other</dc:identifier>
   </oai_dc:dc>
  </metadata></record>
 </ListRecords>
</OAI-PMH>""".encode("utf-8")

NTRS_JSON = {
    "results": [{
        "id": 19670018966,
        "title": "The effect of small additions of refractory on an aluminum alloy",
        "abstract": "Zirconium and titanium additions for improved properties",
        "otherReportNumbers": ["NASA-TT-F-11015"],
        "disseminated": "METADATA_ONLY",
        "authorAffiliations": [{"meta": {"author": {"name": "Kirpichnikov, K. S."}}}],
    }]
}

JSTAGE_XML = f"""<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
 <entry>
  <article_title><en>Development of Heat-resistant Nickel Alloy</en><ja>{JA_TITLE}</ja></article_title>
  <article_link><en>https://example.org/en</en><ja>https://example.org/ja</ja></article_link>
  <author><ja><name>富永 克彦</name></ja><en><name>Katsuhiko TOMINAGA</name></en></author>
  <material_title><en>Honda R&amp;D Technical Review</en><ja>Honda R&amp;D Technical Review</ja></material_title>
  <pubyear>2007</pubyear>
  <doi>10.69239/hondatechnicalreview.2007_19_2_10</doi>
 </entry>
</feed>""".encode("utf-8")

ARCHIVE_JSON = {"response": {"docs": [{
    "identifier": "21427-83",
    "title": "ГОСТ 21427 83 Сталь Лист Электротех",
    "year": "1984",
    "language": "rus",
}]}}


def _resp(content=None, json_body=None):
    r = MagicMock()
    r.raise_for_status = MagicMock()
    if content is not None:
        r.content = content
    if json_body is not None:
        r.json.return_value = json_body
    return r


class TestCollectorContract:
    def test_name_and_params(self):
        c = EasternLiteratureCollector()
        assert c.name == "eastern_literature"
        assert set(c.supported_params()) == {"query", "max_results", "sources"}

    def test_empty_query_returns_empty(self):
        c = EasternLiteratureCollector()
        assert c.collect(query="") == []
        assert c.collect_with_status(query="") == {"results": [], "source_status": {}}

    def test_gated_source_names_the_blocker(self):
        """A licensed source must report WHY it is unavailable. An empty list
        here would read as 'China has published nothing on this'."""
        c = EasternLiteratureCollector()
        out = c.collect_with_status(query="superalloy", sources=["cnki", "elibrary"])
        assert out["results"] == []
        assert out["source_status"]["cnki"].startswith("blocked:")
        assert "Disallow" in out["source_status"]["cnki"]
        assert out["source_status"]["elibrary"].startswith("blocked:")
        assert "subscription" in out["source_status"]["elibrary"]

    def test_every_gated_source_explains_itself(self):
        for name, reason in GATED_SOURCES.items():
            assert "Not collected" in reason, name

    def test_unknown_source_is_an_error_not_silence(self):
        c = EasternLiteratureCollector()
        out = c.collect_with_status(query="x", sources=["nonexistent"])
        assert out["source_status"]["nonexistent"].startswith("error:")

    def test_no_source_is_crowded_out_of_the_budget(self):
        """A status line saying a source returned results, next to zero of its
        records in `results`, would be a lie. Round-robin prevents it."""
        c = EasternLiteratureCollector()
        a = [{"source": "a", "n": i} for i in range(10)]
        b = [{"source": "b", "n": i} for i in range(10)]
        merged = c._interleave([a, b], limit=4)
        assert [r["source"] for r in merged] == ["a", "b", "a", "b"]

    def test_interleave_handles_ragged_and_empty_inputs(self):
        c = EasternLiteratureCollector()
        assert c._interleave([], 5) == []
        assert c._interleave([[], []], 5) == []
        merged = c._interleave([[{"n": 1}], [{"n": 2}, {"n": 3}]], 10)
        assert [r["n"] for r in merged] == [1, 2, 3]

    def test_interleave_respects_a_zero_limit(self):
        c = EasternLiteratureCollector()
        assert c._interleave([[{"n": 0}]], 0) == []
        assert c._interleave([[{"n": 0}]], -1) == []

    def test_one_broken_backend_does_not_destroy_the_others(self):
        """A backend raising must degrade to a named error, not discard the
        sources that already succeeded and erase source_status with it."""
        c = EasternLiteratureCollector()
        good = [{"source": "jstage", "title": JA_TITLE}]
        with patch.object(EasternLiteratureCollector, "_search_cyberleninka",
                          side_effect=RuntimeError("backend exploded")), \
             patch.object(EasternLiteratureCollector, "_search_jstage",
                          return_value=(good, None)):
            out = c.collect_with_status(
                query="сплав", sources=["cyberleninka", "jstage"])
        assert out["results"] == good, "healthy source's results were discarded"
        assert out["source_status"]["cyberleninka"].startswith("error:")
        assert "backend exploded" in out["source_status"]["cyberleninka"]
        assert out["source_status"]["jstage"].startswith("ok")

    def test_status_reports_what_the_budget_actually_kept(self):
        """'ok (5 results)' next to 2 kept records would overstate what the
        caller received."""
        c = EasternLiteratureCollector()
        many = [{"source": "cyberleninka", "n": i} for i in range(5)]
        few = [{"source": "jstage", "n": i} for i in range(1)]
        with patch.object(EasternLiteratureCollector, "_search_cyberleninka",
                          return_value=(many, "ok (5 results)")), \
             patch.object(EasternLiteratureCollector, "_search_jstage",
                          return_value=(few, None)):
            out = c.collect_with_status(
                query="сплав", max_results=3, sources=["cyberleninka", "jstage"])
        assert len(out["results"]) == 3
        assert "kept after max_results trim" in out["source_status"]["cyberleninka"]
        # jstage was not trimmed, so it gets no misleading suffix.
        assert "trim" not in out["source_status"]["jstage"]


class TestCyberLeninka:
    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_parses_cyrillic_and_filters(self, mock_requests):
        mock_requests.get.return_value = _resp(content=OAI_XML)
        c = EasternLiteratureCollector()
        hits, err = c._search_cyberleninka("сплав", max_results=5)
        assert err.startswith("ok (")
        assert hits, "matching Cyrillic title should be returned"
        rec = hits[0]
        assert rec["title"] == RU_TITLE  # byte-for-byte, no transliteration
        assert rec["source"] == "cyberleninka"
        assert rec["source_language"] == "ru"
        assert rec["language_basis"] == "script"
        assert rec["authors"] == ["Косьмин А. А."]

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_detects_refused_designation(self, mock_requests):
        mock_requests.get.return_value = _resp(content=OAI_XML)
        c = EasternLiteratureCollector()
        hits, _ = c._search_cyberleninka("сплав", max_results=5)
        vzhl = [d for d in hits[0]["designations"] if d["designation"] == "ВЖЛ21"]
        assert vzhl and vzhl[0]["status"] == "refused"
        assert "western" not in vzhl[0]

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_reachable_but_empty_is_not_a_failure(self, mock_requests):
        """The key honesty property: 0 results with the scan budget stated,
        NOT an error and NOT an unqualified 'nothing exists'."""
        mock_requests.get.return_value = _resp(content=OAI_XML)
        c = EasternLiteratureCollector()
        hits, err = c._search_cyberleninka("zzzznomatch", max_results=5)
        assert hits == []
        assert err.startswith("ok (0 results")
        assert "scanned" in err
        assert "empty is not absence" in err

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_network_failure_is_reported_as_error(self, mock_requests):
        mock_requests.get.side_effect = Exception("boom")
        c = EasternLiteratureCollector()
        hits, err = c._search_cyberleninka("сплав", max_results=5)
        assert hits == []
        assert err.startswith("error:") and "boom" in err

    def test_query_with_no_usable_token_is_an_error(self):
        c = EasternLiteratureCollector()
        hits, err = c._search_cyberleninka("a b", max_results=5)
        assert hits == [] and err.startswith("error:")

    def test_stopwords_do_not_become_match_everything_tokens(self):
        """'для' is 3 chars and appears in most Russian technical titles —
        OR-matching on it made the filter return unrelated papers while the
        status line still implied relevance."""
        from app.tools.data_collectors.eastern_literature_collector import (
            _query_tokens,
        )
        assert _query_tokens("покрытия для лопаток") == ["покрыт", "лопато"]
        # A query of nothing but stopwords is an error, not a full-corpus dump.
        c = EasternLiteratureCollector()
        hits, err = c._search_cyberleninka("для при как", max_results=5)
        assert hits == [] and err.startswith("error:")

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_identifierless_record_gets_a_stable_unique_id(self, mock_requests):
        """normalize_records() dedups on source_id, so a blank one would make
        two real records collapse into one silently."""
        xml = OAI_XML.replace(
            b"<dc:identifier>https://cyberleninka.ru/article/n/zharo-i-korrozionnostoykoe</dc:identifier>",
            b"")
        mock_requests.get.return_value = _resp(content=xml)
        c = EasternLiteratureCollector()
        hits, _ = c._search_cyberleninka("сплав", max_results=5)
        assert hits[0]["source_id"].startswith("cyberleninka:journal_")
        # Stable across processes — hash() would be salted per run.
        again, _ = c._search_cyberleninka("сплав", max_results=5)
        assert again[0]["source_id"] == hits[0]["source_id"]


class TestNTRSTranslations:
    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_parses_and_flags_translation_provenance(self, mock_requests):
        mock_requests.get.return_value = _resp(json_body=NTRS_JSON)
        c = EasternLiteratureCollector()
        hits, err = c._search_ntrs_translations("alloy", max_results=5)
        assert err is None and len(hits) == 1
        rec = hits[0]
        assert rec["source"] == "ntrs_translations"
        assert rec["translated_series"] == "NASA Technical Translation"
        assert rec["report_numbers"] == ["NASA-TT-F-11015"]
        # METADATA_ONLY must not masquerade as a retrievable document.
        assert rec["full_text_available"] is False
        assert rec["source_language"] == "en"
        assert rec["language_basis"] == "declared"

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_requests_only_technical_translations(self, mock_requests):
        mock_requests.get.return_value = _resp(json_body={"results": []})
        EasternLiteratureCollector()._search_ntrs_translations("alloy", 5)
        params = mock_requests.get.call_args.kwargs["params"]
        assert params["stiType"] == "TECHNICAL_TRANSLATION"

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_null_results_envelope_does_not_explode(self, mock_requests):
        mock_requests.get.return_value = _resp(json_body={"results": None})
        hits, err = EasternLiteratureCollector()._search_ntrs_translations("x", 5)
        assert hits == [] and err is None

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_original_language_is_not_invented(self, mock_requests):
        """The TT series is predominantly Russian but also carries Japanese
        and French originals, and NTRS does not publish which. Claiming 'ru'
        per record would be a fabricated provenance."""
        mock_requests.get.return_value = _resp(json_body=NTRS_JSON)
        hits, _ = EasternLiteratureCollector()._search_ntrs_translations("x", 5)
        assert hits[0]["original_language"] is None


class TestJStage:
    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_keeps_japanese_original_as_title(self, mock_requests):
        mock_requests.get.return_value = _resp(content=JSTAGE_XML)
        c = EasternLiteratureCollector()
        hits, err = c._search_jstage("耐熱合金", max_results=5)
        assert err is None and len(hits) == 1
        rec = hits[0]
        assert rec["title"] == JA_TITLE  # original, not the English rendering
        assert rec["title_en"] == "Development of Heat-resistant Nickel Alloy"
        assert rec["source_language"] == "ja"
        assert rec["authors"] == ["富永 克彦"]
        assert rec["source_id"].startswith("10.69239/")

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_rejected_query_is_an_error_not_a_phantom_record(self, mock_requests):
        """J-STAGE answers a query it cannot index (Cyrillic) with ERR_001 and
        one empty <entry>. Parsing that entry turned a refusal into a titleless
        record — an error wearing a result's clothes."""
        err_xml = ("""<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
 <result><status>ERR_001</status><message>ERR_001</message></result>
 <entry><title/><link/><id/><updated/></entry>
</feed>""").encode("utf-8")
        mock_requests.get.return_value = _resp(content=err_xml)
        hits, err = EasternLiteratureCollector()._search_jstage("сплав", 5)
        assert hits == []
        assert err.startswith("error:") and "ERR_001" in err

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_warning_status_still_yields_results(self, mock_requests):
        """WARN_* accompanies valid results and must not be treated as failure."""
        warn_xml = JSTAGE_XML.replace(
            b"<feed xmlns=\"http://www.w3.org/2005/Atom\">",
            b"<feed xmlns=\"http://www.w3.org/2005/Atom\">"
            b"<result><status>WARN_002</status></result>", 1)
        mock_requests.get.return_value = _resp(content=warn_xml)
        hits, err = EasternLiteratureCollector()._search_jstage("x", 5)
        assert err is None and len(hits) == 1

    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_mixed_language_coauthors_all_survive(self, mock_requests):
        """Picking one language for the whole entry dropped a foreign
        co-author who only has an <en> name."""
        xml = JSTAGE_XML.replace(
            b"</author>",
            b"</author><author><en><name>Foreign Coauthor</name></en></author>", 1)
        mock_requests.get.return_value = _resp(content=xml)
        hits, _ = EasternLiteratureCollector()._search_jstage("x", 5)
        assert hits[0]["authors"] == ["富永 克彦", "Foreign Coauthor"]


class TestInternetArchive:
    @patch("app.tools.data_collectors.eastern_literature_collector.requests")
    def test_scan_record_uses_declared_language(self, mock_requests):
        mock_requests.get.return_value = _resp(json_body=ARCHIVE_JSON)
        c = EasternLiteratureCollector()
        hits, err = c._search_archive("ГОСТ сталь", max_results=5)
        assert err is None and len(hits) == 1
        rec = hits[0]
        assert rec["type"] == "scan"
        assert rec["title"].startswith("ГОСТ")
        assert rec["source_language"] == "ru"  # 'rus' normalised
        assert rec["language_basis"] == "declared"


class TestLanguageProvenance:
    def test_scripts(self):
        assert detect_script("Жаропрочный") == "cyrillic"
        assert detect_script(JA_TITLE) == "japanese"
        assert detect_script("高温合金") == "han"
        assert detect_script("superalloy") == "latin"
        assert detect_script("") == "unknown"

    def test_latin_is_never_guessed_as_english(self):
        """The failure this whole module exists to prevent."""
        assert infer_language("Superalloy study") == (None, "undetermined")

    def test_declared_beats_detection(self):
        assert infer_language("Жаропрочный", declared="ru-RU") == ("ru", "declared")

    def test_declared_codes_are_normalised_to_one_register(self):
        """'rus' (Internet Archive) and 'ru' (OAI dc:language) must land on the
        same value or nothing downstream can join on the field."""
        assert infer_language("x", declared="rus")[0] == "ru"
        assert infer_language("x", declared="jpn")[0] == "ja"
        assert infer_language("x", declared="zho")[0] == "zh"
        # Unrecognised codes pass through rather than being invented away.
        assert infer_language("x", declared="xyz")[0] == "xyz"

    def test_script_inference(self):
        assert infer_language("Жаропрочный сплав") == ("ru", "script")
        assert infer_language(JA_TITLE) == ("ja", "script")
        assert infer_language("高温合金的研究") == ("zh", "script")


class TestDesignations:
    def test_mapped_carries_both_standards(self):
        r = map_designation("ВТ6")
        assert r["status"] == "mapped"
        assert r["western"] == "Ti-6Al-4V"
        assert r["equivalence_type"] == "nominal_composition"
        assert any("GOST 19807" in s for s in r["standards"])
        assert any("ASTM B348" in s for s in r["standards"])

    def test_chinese_grade_mapped(self):
        r = map_designation("GH4169")
        assert r["status"] == "mapped" and r["western"] == "Alloy 718"
        assert any("GB/T 14992" in s for s in r["standards"])

    def test_homoglyph_latin_typing_finds_cyrillic_grade(self):
        """'BT6' typed on a Latin keyboard is Cyrillic 'ВТ6'."""
        assert map_designation("BT6")["western"] == "Ti-6Al-4V"
        # ...without breaking genuinely-Latin grades containing H.
        assert map_designation("GH4169")["status"] == "mapped"

    @pytest.mark.parametrize("grade", ["ЖС6У", "ЖС32", "ВЖЛ21", "ХН77ТЮР",
                                       "12Х18Н10Т", "GH3536"])
    def test_refusals_have_reasons_and_no_equivalent(self, grade):
        r = map_designation(grade)
        assert r["status"] == "refused"
        assert r["reason"].endswith("Refused.")
        assert "western" not in r

    def test_carbon_class_discriminated(self):
        """08Х18Н10Т maps to 321; 12Х18Н10Т is refused. Getting this wrong is
        exactly the kind of fabricated equivalence that must not ship."""
        assert map_designation("08Х18Н10Т")["western"] == "AISI 321"
        assert map_designation("12Х18Н10Т")["status"] == "refused"

    def test_unknown_grade_is_not_upgraded_to_a_guess(self):
        r = map_designation("АД35")
        assert r["status"] == "unknown"
        assert "western" not in r

    def test_find_designations_in_running_text(self):
        found = find_designations("Покрытие для сплава ВЖЛ21 и ВТ6")
        statuses = {d["designation"]: d["status"] for d in found}
        assert statuses["ВЖЛ21"] == "refused"
        assert statuses["ВТ6"] == "mapped"

    def test_no_false_positives_on_plain_prose(self):
        assert find_designations("Исследование структуры и свойств покрытий") == []
        assert find_designations("ГОСТ 5632-2014 Нержавеющие стали") == []

    def test_extracts_gost_steel_grades_from_running_text(self):
        """Regression: the alternating letter/digit GOST form (08 Х 18 Н 10 Т)
        was unreachable via find_designations(), so the carbon-class refusal
        never fired on real text even though map_designation() knew it."""
        found = find_designations("испытания стали 08Х18Н10Т и 12Х18Н10Т на коррозию")
        statuses = {d["designation"]: d["status"] for d in found}
        assert statuses["08Х18Н10Т"] == "mapped"
        assert statuses["12Х18Н10Т"] == "refused"

    def test_extracts_hyphenated_grade(self):
        found = find_designations("сплав ВТ-6 применяется в авиации")
        assert [d["designation"] for d in found] == ["ВТ-6"]
        assert found[0]["status"] == "mapped" and found[0]["western"] == "Ti-6Al-4V"

    def test_extracts_chinese_grade_inside_unspaced_cjk(self):
        """Regression: Han counts as \\w, so \\b never fired inside Chinese
        text and GH-grades were only found in Latin-spaced sentences."""
        assert find_designations("镍基合金GH4169的组织")[0]["designation"] == "GH4169"
        assert find_designations("GH4169合金的性能研究")[0]["status"] == "mapped"

    def test_chemical_formulas_are_not_reported_as_grades(self):
        """СО2 is structurally identical to ВТ6; denylisted rather than
        emitted as noise on every OCR'd handbook page."""
        assert find_designations("выбросы СО2 в атмосферу") == []


class TestStorageRoundTrip:
    def test_cyrillic_and_cjk_survive_into_a_stored_record(self, tmp_path):
        """Round-trip through the real normalize -> parquet -> load path. If
        this mangles, every downstream EMMO fact from these sources is wrong."""
        from app.tools.data_collectors.normalizer import normalize_records
        from app.tools.data_collectors.store import DataStore

        records = [
            {"source_id": "ru1", "title": RU_TITLE, "authors": ["Каблов Е. Н."],
             "source_language": "ru", "language_basis": "script",
             "designations": find_designations(RU_TITLE)},
            {"source_id": "ja1", "title": JA_TITLE, "authors": ["富永 克彦"],
             "source_language": "ja", "language_basis": "script",
             "designations": []},
        ]
        store = DataStore(str(tmp_path))
        store.save(normalize_records(records), "eastern")
        back = store.load("eastern")

        assert back.iloc[0]["title"] == RU_TITLE
        assert back.iloc[1]["title"] == JA_TITLE
        assert back.iloc[0]["authors"][0] == "Каблов Е. Н."
        assert back.iloc[1]["authors"][0] == "富永 克彦"
        assert back.iloc[0]["source_language"] == "ru"
        # Nested refusal reasons must survive too — they are the guard against
        # a downstream consumer inventing an equivalence.
        assert back.iloc[0]["designations"][0]["status"] == "refused"


class TestRegistryWiring:
    def test_registered_in_default_registry(self):
        from app.tools.data_collectors.base_collector import (
            get_default_collector_registry,
        )
        reg = get_default_collector_registry()
        assert reg.get("eastern_literature").name == "eastern_literature"


class TestPriorArtWiring:
    @patch("app.tools.data_collectors.eastern_literature_collector"
           ".EasternLiteratureCollector.collect_with_status")
    def test_prior_art_search_eastern_source(self, mock_collect):
        from app.tools.search import _prior_art_search
        mock_collect.return_value = {
            "results": [{"source": "cyberleninka", "title": RU_TITLE,
                         "abstract": ""}],
            "source_status": {"cnki": "blocked: licence required"},
        }
        out = _prior_art_search(query="сплав", source="eastern")
        assert out["counts"]["eastern"] == 1
        assert out["eastern"][0]["title"] == RU_TITLE
        assert out["eastern_source_status"]["cnki"].startswith("blocked:")

    def test_schema_advertises_eastern(self):
        from app.tools.base import ToolRegistry
        from app.tools.search import create_search_tools
        reg = ToolRegistry()
        create_search_tools(reg)
        schema = reg.get("prior_art_search").input_schema
        assert "eastern" in schema["properties"]["source"]["enum"]
        assert "eastern_sources" in schema["properties"]


# --------------------------------------------------------------------------
# Real-network probes. A green mock proves the parser, not the source.
# --------------------------------------------------------------------------

live = pytest.mark.skipif(
    os.getenv("PRISM_LIVE_SOURCES") != "1",
    reason="set PRISM_LIVE_SOURCES=1 to hit real sources",
)


@pytest.mark.network
@live
class TestRealSources:
    def test_cyberleninka_live(self):
        hits, err = EasternLiteratureCollector()._search_cyberleninka(
            "сплав", max_results=3)
        assert err.startswith("ok ("), err
        assert hits, "CyberLeninka OAI returned nothing for 'сплав'"
        assert any(ord(ch) > 0x400 for ch in hits[0]["title"])

    def test_ntrs_translations_live(self):
        hits, err = EasternLiteratureCollector()._search_ntrs_translations(
            "alloy", max_results=3)
        assert err is None, err
        assert hits, "NTRS returned no Technical Translations for 'alloy'"

    def test_jstage_live(self):
        hits, err = EasternLiteratureCollector()._search_jstage(
            "耐熱合金", max_results=3)
        assert err is None, err
        assert hits, "J-STAGE returned nothing for '耐熱合金'"

    def test_internet_archive_live(self):
        hits, err = EasternLiteratureCollector()._search_archive(
            "ГОСТ сталь", max_results=3)
        assert err is None, err
        assert hits, "Internet Archive returned nothing for 'ГОСТ сталь'"
