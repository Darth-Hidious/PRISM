#!/usr/bin/env python3
"""Coverage manifold of the PRISM knowledge graph.

The store is treated as a fabric over (subject x physical quantity):

  * WOVEN  -- >=2 independent sources corroborate the cell,
  * THIN   -- a single source asserts it (or an assertion with no linked
              source entity at all),
  * HOLE   -- nothing was ever measured: an answer here must be predicted,
              not looked up. Holes are where research goes next.

Quantity is identified by UNIT, not predicate: in this store most assertions
share one catch-all EMMO predicate, so the predicate does not say what was
measured -- the unit does (MPa = a strength, GPa = a modulus, K = a
temperature).

Read-only: the database is opened with mode=ro and never written.

Usage:
  python3 scripts/coverage_manifold.py [--db PATH] [--out PATH] [--tenant T]
                                       [--max-rows N] [--max-cols N]
"""

from __future__ import annotations

import argparse
import difflib
import re
import sqlite3
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")  # headless: never depends on a display server

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.colors import BoundaryNorm, ListedColormap
from matplotlib.patches import Patch, Rectangle
from scipy.cluster.hierarchy import leaves_list, linkage
from scipy.spatial.distance import pdist

DEFAULT_DB = Path.home() / ".prism" / "provenance.db"
DEFAULT_OUT = Path("coverage_manifold.png")

# A cell is WOVEN from this many distinct sources upward; below it is THIN.
WOVEN_MIN_SOURCES = 2

# Source counts are displayed as discrete levels 0,1,2,3,4+ (4 aggregates the
# tail so the legend stays finite). Counts are small integers; a continuous
# ramp would imply fractional sources, which do not exist.
LEVEL_CAP = 4

# Holes are NOT the low end of the data ramp: a never-measured cell is not a
# measurement of zero. They get a dedicated neutral grey plus hatching so the
# distinction survives greyscale printing and colour-vision deficiency.
HOLE_FACE = "#d4d4d4"
HOLE_HATCH = "////"
HOLE_EDGE = "#8a8a8a"

# Subject strings likelier than this to be the same material (after
# normalisation, difflib.SequenceMatcher ratio) are flagged as "torn fabric".
# 0.78 is tuned to catch measured duplicates in this store
# ("CrCoNi medium-entropy alloy" vs "CrCoNi equiatomic medium-entropy")
# without gluing genuinely different subjects together.
TEAR_RATIO = 0.78

# Okabe-Ito qualitative palette: colour-vision-deficiency safe. Used only for
# the torn-fabric row markers; the "~Gn" text prefix carries the same
# information redundantly, so hue alone never encodes meaning.
GROUP_COLORS = ["#E69F00", "#56B4E9", "#009E73", "#CC79A7",
                "#D55E00", "#0072B2", "#F0E442", "#000000"]

CELL_QUERY = """
SELECT a.subject, a.unit AS quantity,
       COUNT(DISTINCT e.source_entity_id) AS sources
FROM prov_assertion a
LEFT JOIN prov_assertion_evidence e ON e.assertion_id = a.id
WHERE a.value IS NOT NULL AND a.unit IS NOT NULL AND TRIM(a.unit) <> ''
{tenant_clause}
GROUP BY a.subject, a.unit
"""


def fetch_cells(db_path: Path, tenant: str | None) -> list[tuple[str, str, int]]:
    """Return (subject, unit, distinct-source-count) rows, read-only."""
    # as_uri() percent-encodes spaces etc., which sqlite's URI parser honours;
    # a bare f-string path would break on paths containing '?' or spaces.
    uri = db_path.resolve().as_uri() + "?mode=ro"
    con = sqlite3.connect(uri, uri=True)
    try:
        clause, params = "", ()
        if tenant is not None:
            clause, params = "AND a.tenant = ?", (tenant,)
        rows = con.execute(CELL_QUERY.format(tenant_clause=clause), params).fetchall()
        if not rows and tenant is not None:
            known = [r[0] for r in con.execute(
                "SELECT DISTINCT tenant FROM prov_assertion ORDER BY tenant")]
            raise SystemExit(
                f"error: no assertions for tenant {tenant!r}; "
                f"tenants present: {known}")
        return [(str(s), str(u), int(n)) for s, u, n in rows]
    finally:
        con.close()


def build_matrix(cells: list[tuple[str, str, int]]
                 ) -> tuple[list[str], list[str], np.ndarray]:
    """Full (subject x unit) matrix; NaN marks a hole (absent != zero)."""
    subjects = sorted({c[0] for c in cells})
    units = sorted({c[1] for c in cells})
    si = {s: i for i, s in enumerate(subjects)}
    ui = {u: j for j, u in enumerate(units)}
    mat = np.full((len(subjects), len(units)), np.nan)
    for s, u, n in cells:
        mat[si[s], ui[u]] = n
    return subjects, units, mat


def select_window(mat: np.ndarray, subjects: list[str], units: list[str],
                  max_rows: int, max_cols: int
                  ) -> tuple[np.ndarray, list[str], list[str], int, int]:
    """Cap rows/cols by coverage, deterministically.

    Columns are chosen by global coverage first, then rows by coverage WITHIN
    those columns, then columns that became empty are pruned. This guarantees
    no all-hole row/column appears merely because its measurements fall
    outside the window -- that would fabricate holes.
    Ties break on name so identical data always renders identically.
    """
    measured = ~np.isnan(mat)

    col_order = sorted(range(len(units)),
                       key=lambda j: (-int(measured[:, j].sum()), units[j]))
    keep_c = sorted(col_order[:max_cols])

    row_cov = measured[:, keep_c].sum(axis=1)
    row_order = sorted((i for i in range(len(subjects)) if row_cov[i] > 0),
                       key=lambda i: (-int(row_cov[i]), subjects[i]))
    keep_r = sorted(row_order[:max_rows])

    # Prune columns emptied by the row cap (possible when a well-covered
    # column's subjects were all dropped); an all-hole column here would lie.
    sub = mat[np.ix_(keep_r, keep_c)]
    nonempty = ~np.all(np.isnan(sub), axis=0)
    keep_c = [c for c, ok in zip(keep_c, nonempty) if ok]
    sub = mat[np.ix_(keep_r, keep_c)]

    return (sub, [subjects[i] for i in keep_r], [units[j] for j in keep_c],
            len(subjects) - len(keep_r), len(units) - len(keep_c))


def seriate(mat: np.ndarray, row_names: list[str], col_names: list[str]
            ) -> tuple[np.ndarray, list[str], list[str]]:
    """Order rows/cols so structure is visible, deterministically.

    Hierarchical clustering (average linkage, Jaccard on the binary presence
    matrix) groups subjects measured for similar quantity sets, making woven
    blocks and hole fields contiguous. Input rows are pre-sorted by
    (coverage desc, name) so scipy sees a canonical order and ties resolve
    the same way every run. Fewer than 3 rows/cols: clustering is
    meaningless, fall back to that same coverage sort.
    """
    def coverage_order(names: list[str], present: np.ndarray) -> list[int]:
        return sorted(range(len(names)),
                      key=lambda k: (-int(present[k].sum()), names[k]))

    present = ~np.isnan(mat)

    r_pre = coverage_order(row_names, present)
    if len(r_pre) >= 3:
        p = present[r_pre].astype(float)
        r_ord = [r_pre[k] for k in leaves_list(linkage(pdist(p, "jaccard"),
                                                       method="average"))]
    else:
        r_ord = r_pre

    c_pre = coverage_order(col_names, present.T)
    if len(c_pre) >= 3:
        p = present.T[c_pre].astype(float)
        c_ord = [c_pre[k] for k in leaves_list(linkage(pdist(p, "jaccard"),
                                                       method="average"))]
    else:
        c_ord = c_pre

    return (mat[np.ix_(r_ord, c_ord)],
            [row_names[i] for i in r_ord], [col_names[j] for j in c_ord])


def normalise_subject(s: str) -> str:
    """Canonical comparison form: case, punctuation, parentheticals and the
    generic token 'alloy(s)' carry no material identity, so drop them.
    'IN718 (Inconel 718)' and 'IN718' collapse to the same string;
    'Ti-6Al-4V' and 'Ti6Al4V alloy' likewise."""
    s = re.sub(r"\([^)]*\)", " ", s.lower())        # parenthetical suffixes
    tokens = re.sub(r"[^a-z0-9]+", " ", s).split()   # punctuation/whitespace
    return "".join(t for t in tokens if t not in {"alloy", "alloys"})


def detect_tears(row_names: list[str]) -> dict[str, int]:
    """Union-find grouping of subject strings that likely name ONE material.

    Returns {subject: group_id} for subjects in groups of >=2. This is a
    limitation DISCLOSURE, not entity resolution: rows are never merged,
    because merging belongs to the ingestion plane, not a plotting script.
    """
    norms = {name: normalise_subject(name) for name in row_names}
    parent = {name: name for name in row_names}

    def find(x: str) -> str:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    names = sorted(row_names)  # deterministic pair order
    for i, a in enumerate(names):
        for b in names[i + 1:]:
            na, nb = norms[a], norms[b]
            if not na or not nb:
                continue
            # Material identity leads the subject string ("CrCoNi ...",
            # "IN718 ..."), so require a shared prefix: it rejects pairs like
            # "methane stream in <same rig>" vs "oxygen stream in <same rig>"
            # whose long common suffix would otherwise beat the ratio test.
            # A false "same material" mark misleads; a miss merely under-flags.
            if na[:4] != nb[:4]:
                continue
            if na == nb or difflib.SequenceMatcher(None, na, nb).ratio() >= TEAR_RATIO:
                parent[find(a)] = find(b)

    roots: dict[str, list[str]] = {}
    for name in row_names:
        roots.setdefault(find(name), []).append(name)
    groups: dict[str, int] = {}
    gid = 0
    for members in roots.values():
        if len(members) >= 2:
            gid += 1
            for m in members:
                groups[m] = gid
    return groups


def normalise_unit(u: str) -> str:
    """Conservative unit canonicalisation, for disclosure only.

    Strips spacing/dot/paren/caret noise but KEEPS '/' and '-' so that
    'm/s' never collides with 'ms' (milliseconds). Catches real variants
    like 'J/(kg K)' vs 'J/kgK' while refusing risky merges; some variants
    ('W m^-1 K^-1' vs 'W/(m K)') are deliberately missed rather than
    guessed at.
    """
    u = u.lower().replace("−", "-").replace("·", "")  # unicode minus, middot
    # Pure glyph folds, zero merge risk: micro sign vs Greek mu vs 'u',
    # and the degree sign vs 'deg' ('°C' vs 'degC' both occur in this store).
    u = u.replace("µ", "u").replace("μ", "u").replace("°", "deg")
    return re.sub(r"[\s.*^()]+", "", u)


def summarise(mat: np.ndarray) -> dict[str, int | float]:
    total = mat.size
    holes = int(np.isnan(mat).sum())
    measured = mat[~np.isnan(mat)]
    thin = int((measured < WOVEN_MIN_SOURCES).sum())
    woven = int((measured >= WOVEN_MIN_SOURCES).sum())
    return {"total": total, "holes": holes, "thin": thin, "woven": woven,
            "hole_frac": holes / total if total else 0.0}


def render(mat: np.ndarray, row_names: list[str], col_names: list[str],
           tears: dict[str, int], stats_disp: dict, stats_full: dict,
           full_shape: tuple[int, int], omitted_rows: int, omitted_cols: int,
           db_path: Path, tenant: str | None, out_path: Path) -> None:
    n_rows, n_cols = mat.shape

    # Discrete levels 0..LEVEL_CAP: BoundaryNorm + ListedColormap so each
    # integer source count gets exactly one colour band -- a continuous ramp
    # would imply "1.7 sources". Viridis samples: perceptually uniform and
    # colour-vision-deficiency safe.
    n_levels = LEVEL_CAP + 1
    cmap = ListedColormap(plt.get_cmap("viridis")(np.linspace(0.04, 0.96, n_levels)))
    cmap.set_bad(HOLE_FACE)  # holes: neutral grey, OUTSIDE the ramp
    norm = BoundaryNorm(np.arange(n_levels + 1) - 0.5, n_levels)

    display = np.ma.masked_invalid(np.minimum(mat, LEVEL_CAP))

    fig_w = max(7.5, 0.42 * n_cols + 5.5)
    fig_h = max(4.5, 0.26 * n_rows + 3.0)
    fig, ax = plt.subplots(figsize=(fig_w, fig_h))

    # interpolation="nearest": cells are categorical facts, not samples of a
    # continuous field -- any smoothing would invent data between cells.
    ax.imshow(display, cmap=cmap, norm=norm, interpolation="nearest",
              aspect="auto")

    # Hatch each hole so absence is encoded redundantly (texture + colour):
    # legible in greyscale and to colour-blind readers.
    for i in range(n_rows):
        for j in range(n_cols):
            if np.isnan(mat[i, j]):
                ax.add_patch(Rectangle((j - 0.5, i - 0.5), 1, 1, fill=False,
                                       hatch=HOLE_HATCH, edgecolor=HOLE_EDGE,
                                       linewidth=0))

    ax.set_xticks(np.arange(n_cols))
    ax.set_xticklabels(col_names, rotation=45, ha="right", fontsize=7)
    ax.set_yticks(np.arange(n_rows))
    labels = []
    for name in row_names:
        gid = tears.get(name)
        short = name if len(name) <= 46 else name[:45] + "…"
        labels.append(f"~G{gid} {short}" if gid else short)
    ax.set_yticklabels(labels, fontsize=7)
    for tick, name in zip(ax.get_yticklabels(), row_names):
        gid = tears.get(name)
        if gid:
            tick.set_color(GROUP_COLORS[(gid - 1) % len(GROUP_COLORS)])
            tick.set_fontweight("bold")

    # White cell borders: keeps adjacent same-level cells countable.
    ax.set_xticks(np.arange(-0.5, n_cols), minor=True)
    ax.set_yticks(np.arange(-0.5, n_rows), minor=True)
    ax.grid(which="minor", color="white", linewidth=0.6)
    ax.tick_params(which="minor", length=0)
    ax.tick_params(which="major", length=0)
    for spine in ax.spines.values():
        spine.set_visible(False)

    level_labels = ["asserted, 0 linked sources (thin)", "1 source (thin)"] + [
        f"{k} sources (woven)" for k in range(2, LEVEL_CAP)
    ] + [f"≥{LEVEL_CAP} sources (woven)"]
    handles = [Patch(facecolor=HOLE_FACE, edgecolor=HOLE_EDGE, hatch=HOLE_HATCH,
                     label="never measured (HOLE) — absent, not zero")]
    handles += [Patch(facecolor=cmap(k), label=level_labels[k])
                for k in range(n_levels)]
    ax.legend(handles=handles, loc="upper left", bbox_to_anchor=(1.02, 1.0),
              fontsize=8, frameon=False, title="cell status", title_fontsize=9)

    tenant_txt = f", tenant={tenant}" if tenant else ""
    omit_txt = ""
    if omitted_rows or omitted_cols:
        omit_txt = (f" — showing top {n_rows}×{n_cols} by coverage; "
                    f"{omitted_rows} subjects and {omitted_cols} quantities omitted")
    ax.set_title(
        f"PRISM knowledge-graph coverage manifold — subject × quantity (by unit)\n"
        f"{db_path.name}{tenant_txt}{omit_txt}",
        fontsize=11, pad=12)

    d, f = stats_disp, stats_full
    lines = [
        f"displayed {n_rows}×{n_cols}: {d['holes']} holes "
        f"({d['hole_frac']:.1%}), {d['thin']} thin, {d['woven']} woven"
        f"  |  full grid {full_shape[0]}×{full_shape[1]}: {f['holes']} holes "
        f"({f['hole_frac']:.1%}), {f['thin']} thin, {f['woven']} woven"
    ]
    if tears:
        n_groups = len(set(tears.values()))
        lines.append(
            f"TORN FABRIC: rows sharing a ~Gn tag ({len(tears)} rows, "
            f"{n_groups} groups) are subject strings that likely denote ONE "
            f"material (normalised similarity ≥ {TEAR_RATIO}); their holes are "
            "OVERSTATED until entity resolution merges them. Detection only — "
            "rows are not merged.")
    unit_norms: dict[str, list[str]] = {}
    for u in col_names:
        unit_norms.setdefault(normalise_unit(u), []).append(u)
    variant_cols = sum(len(v) for v in unit_norms.values() if len(v) >= 2)
    if variant_cols:
        lines.append(
            f"Quantities are keyed by raw unit string; {variant_cols} displayed "
            "columns are spelling variants of a shared unit (e.g. 'J/(kg K)' vs "
            "'J/kgK'), which fragments columns and also overstates holes.")

    for k, line in enumerate(lines):
        fig.text(0.01, -0.012 - 0.026 * k, line, fontsize=8,
                 ha="left", va="top")

    # bbox_inches="tight" so the outside legend and the caption lines are
    # included instead of clipped.
    fig.savefig(out_path, dpi=180, bbox_inches="tight")
    plt.close(fig)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Render the PRISM knowledge-graph coverage manifold "
                    "(holes / thin / woven cells over subject x quantity).")
    ap.add_argument("--db", type=Path, default=DEFAULT_DB,
                    help=f"provenance database (default: {DEFAULT_DB})")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT,
                    help=f"output PNG (default: {DEFAULT_OUT})")
    ap.add_argument("--tenant", default=None, help="filter to one tenant")
    ap.add_argument("--max-rows", type=int, default=40,
                    help="max subjects shown, ranked by coverage (default 40)")
    ap.add_argument("--max-cols", type=int, default=24,
                    help="max quantities shown, ranked by coverage (default 24)")
    args = ap.parse_args(argv)

    if args.max_rows < 1 or args.max_cols < 1:
        print("error: --max-rows and --max-cols must be >= 1", file=sys.stderr)
        return 2

    db_path = args.db.expanduser()
    if not db_path.is_file():
        print(f"error: database not found: {db_path}", file=sys.stderr)
        return 2

    try:
        cells = fetch_cells(db_path, args.tenant)
    except SystemExit as exc:  # tenant-not-found message from fetch_cells
        print(exc, file=sys.stderr)
        return 1
    except sqlite3.Error as exc:
        print(f"error: cannot read {db_path} as a PRISM provenance store "
              f"({exc}). Expected tables: prov_assertion, "
              "prov_assertion_evidence.", file=sys.stderr)
        return 2

    if not cells:
        print(f"error: {db_path} holds no assertions with a numeric value and "
              "a unit; nothing to plot (an empty plot would misrepresent the "
              "store).", file=sys.stderr)
        return 1

    subjects, units, full = build_matrix(cells)
    stats_full = summarise(full)

    window, row_names, col_names, om_r, om_c = select_window(
        full, subjects, units, args.max_rows, args.max_cols)
    window, row_names, col_names = seriate(window, row_names, col_names)
    stats_disp = summarise(window)
    tears = detect_tears(row_names)

    try:
        render(window, row_names, col_names, tears, stats_disp, stats_full,
               (len(subjects), len(units)), om_r, om_c, db_path, args.tenant,
               args.out)
    except OSError as exc:
        print(f"error: could not write {args.out}: {exc}", file=sys.stderr)
        return 2

    f, d = stats_full, stats_disp
    print(f"coverage manifold: {db_path}"
          + (f" (tenant={args.tenant})" if args.tenant else ""))
    print(f"  full grid   {len(subjects)} subjects x {len(units)} quantities "
          f"= {f['total']} cells: {f['holes']} holes ({f['hole_frac']:.1%}), "
          f"{f['thin']} thin, {f['woven']} woven")
    print(f"  displayed   {window.shape[0]} x {window.shape[1]} "
          f"= {d['total']} cells: {d['holes']} holes ({d['hole_frac']:.1%}), "
          f"{d['thin']} thin, {d['woven']} woven"
          + (f"  [{om_r} subjects, {om_c} quantities omitted "
             "(lowest coverage)]" if om_r or om_c else ""))
    if tears:
        by_gid: dict[int, list[str]] = {}
        for name, gid in tears.items():
            by_gid.setdefault(gid, []).append(name)
        print(f"  torn fabric {len(by_gid)} likely-same-material groups among "
              "displayed rows (holes overstated until entity resolution):")
        for gid in sorted(by_gid):
            print(f"    ~G{gid}: " + " | ".join(sorted(by_gid[gid])))
    print(f"  wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
