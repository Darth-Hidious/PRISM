"""The `dataset` tool must not advertise a knob its implementation never reads.

`_SCHEMA` carried a `kind` property for action='visualize' that
`app/tools/skills/visualization.py::_visualize_dataset` never looks at, while
`additionalProperties: False` made the two arguments it DOES read
(`chart_types`, `properties`) unreachable from the tool surface. Measured before
the fix: `kind='distribution'` and `kind='not-a-real-kind'` returned
byte-identical plot lists, comparison scatter included — the knob steered
nothing and there was no way for a caller to steer anything.
"""

import csv
import inspect

from app.tools.dataset import _SCHEMA, _dataset
from app.tools.skills.visualization import _visualize_dataset


def _write_dataset(tmp_path):
    path = tmp_path / "d.csv"
    with path.open("w", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(["formula", "a", "b"])
        for i in range(10):
            writer.writerow([f"El{i}O2", 0.1 * i, 2.0 + 0.1 * i])
    return path


def test_visualize_schema_only_advertises_knobs_the_impl_reads():
    source = inspect.getsource(_visualize_dataset)
    advertised = set(_SCHEMA["properties"])

    assert "kind" not in source, "test premise broken: the impl now reads `kind`"
    assert "kind" not in advertised, (
        "the schema advertises `kind`, which _visualize_dataset never reads — "
        "a caller steering by it silently gets every chart"
    )
    for knob in ("chart_types", "properties"):
        assert knob in source, f"test premise broken: impl no longer reads {knob}"
        assert knob in advertised, (
            f"_visualize_dataset reads {knob}, but additionalProperties=False "
            "keeps it unreachable from the tool surface"
        )


def test_chart_types_and_properties_actually_steer_the_output(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    source_csv = _write_dataset(tmp_path)
    imported = _dataset(
        action="import", file_path=str(source_csv), dataset_name="d"
    )
    assert "error" not in imported, imported

    both = _dataset(action="visualize", dataset_name="d")
    dist_only = _dataset(
        action="visualize", dataset_name="d", chart_types=["distribution"]
    )
    one_column = _dataset(action="visualize", dataset_name="d", properties=["a"])

    # 2 numeric columns -> 2 histograms + 1 pairwise scatter.
    assert len(both["plots"]) == 3, both
    # Asking for distributions only must actually drop the scatter.
    assert len(dist_only["plots"]) == 2, dist_only
    assert len(dist_only["plots"]) < len(both["plots"])
    # Restricting the columns must actually restrict them.
    assert one_column["columns_plotted"] == ["a"], one_column
    assert len(one_column["plots"]) == 1, one_column
