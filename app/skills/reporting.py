"""Report generation skill."""

from datetime import datetime
from pathlib import Path

from app.config.preferences import UserPreferences
from app.skills.base import Skill, SkillStep


def _build_html(title: str, body_sections: list[str]) -> str:
    """Build a simple styled HTML report from section strings."""
    body = "\n".join(body_sections)
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
         max-width: 960px; margin: 2rem auto; padding: 0 1rem; color: #333; }}
  h1 {{ border-bottom: 2px solid #2563eb; padding-bottom: 0.5rem; }}
  h2 {{ color: #1e40af; margin-top: 2rem; }}
  table {{ border-collapse: collapse; width: 100%; margin: 1rem 0; }}
  th, td {{ border: 1px solid #d1d5db; padding: 0.5rem; text-align: left; }}
  th {{ background: #f3f4f6; }}
  tr:nth-child(even) {{ background: #f9fafb; }}
  figure {{ margin: 1rem 0; text-align: center; }}
  figcaption {{ font-style: italic; color: #6b7280; margin-top: 0.25rem; }}
  .quality-good {{ color: #059669; }} .quality-fair {{ color: #d97706; }} .quality-poor {{ color: #dc2626; }}
</style>
</head>
<body>
{body}
</body>
</html>"""


def _generate_report(**kwargs) -> dict:
    """Generate a Markdown or HTML report for a dataset."""
    dataset_name = kwargs.get("dataset_name", "report")
    title = kwargs.get("title", f"PRISM Report \u2014 {dataset_name}")
    include_plots = kwargs.get("include_plots", True)
    output_path = kwargs.get("output_path")
    report_format = kwargs.get("format")
    validation_results = kwargs.get("validation_results")

    prefs = UserPreferences.load()

    # Determine output format
    if not report_format:
        report_format = prefs.report_format  # "markdown", "html", or "pdf"

    from app.data.store import DataStore

    store = DataStore()

    # Load dataset if available
    df = None
    if dataset_name:
        try:
            df = store.load(dataset_name)
        except FileNotFoundError:
            pass

    # ---- Build Markdown ----
    lines = [f"# {title}", "", f"*Generated: {datetime.now().strftime('%Y-%m-%d %H:%M')}*", ""]

    # Also build HTML sections in parallel
    html_sections = [
        f"<h1>{title}</h1>",
        f"<p><em>Generated: {datetime.now().strftime('%Y-%m-%d %H:%M')}</em></p>",
    ]

    numeric_cols: list[str] = []

    if df is not None:
        lines.append("## Dataset Summary")
        lines.append("")
        lines.append(f"- **Name:** {dataset_name}")
        lines.append(f"- **Rows:** {len(df)}")
        lines.append(f"- **Columns:** {len(df.columns)}")
        lines.append("")

        html_sections.append(
            f"<h2>Dataset Summary</h2>"
            f"<ul><li><strong>Name:</strong> {dataset_name}</li>"
            f"<li><strong>Rows:</strong> {len(df)}</li>"
            f"<li><strong>Columns:</strong> {len(df.columns)}</li></ul>"
        )

        # Data preview table
        lines.append("## Data Preview")
        lines.append("")
        preview = df.head(10)
        cols = list(preview.columns)
        lines.append("| " + " | ".join(cols) + " |")
        lines.append("| " + " | ".join(["---"] * len(cols)) + " |")

        html_rows = ["<tr>" + "".join(f"<th>{c}</th>" for c in cols) + "</tr>"]
        for _, row in preview.iterrows():
            vals = []
            for c in cols:
                v = row[c]
                if isinstance(v, float):
                    vals.append(f"{v:.4g}")
                else:
                    vals.append(str(v))
            lines.append("| " + " | ".join(vals) + " |")
            html_rows.append("<tr>" + "".join(f"<td>{v}</td>" for v in vals) + "</tr>")
        lines.append("")
        html_sections.append(
            f"<h2>Data Preview</h2><table>{''.join(html_rows)}</table>"
        )

        # Property statistics
        numeric_cols = df.select_dtypes(include=["float64", "float32", "int64", "int32"]).columns.tolist()
        if numeric_cols:
            lines.append("## Property Statistics")
            lines.append("")
            lines.append("| Property | Mean | Std | Min | Max |")
            lines.append("| --- | --- | --- | --- | --- |")
            stat_html = ["<tr><th>Property</th><th>Mean</th><th>Std</th><th>Min</th><th>Max</th></tr>"]
            for col in numeric_cols:
                s = df[col].dropna()
                if len(s) == 0:
                    continue
                lines.append(
                    f"| {col} | {s.mean():.4g} | {s.std():.4g} | {s.min():.4g} | {s.max():.4g} |"
                )
                stat_html.append(
                    f"<tr><td>{col}</td><td>{s.mean():.4g}</td><td>{s.std():.4g}</td>"
                    f"<td>{s.min():.4g}</td><td>{s.max():.4g}</td></tr>"
                )
            lines.append("")
            html_sections.append(
                f"<h2>Property Statistics</h2><table>{''.join(stat_html)}</table>"
            )

        # Correlation section (2+ numeric columns)
        if len(numeric_cols) >= 2:
            corr = df[numeric_cols].corr()
            # Collect top pairs
            pairs = []
            for i in range(len(corr.columns)):
                for j in range(i + 1, len(corr.columns)):
                    pairs.append((corr.columns[i], corr.columns[j], corr.iloc[i, j]))
            pairs.sort(key=lambda p: abs(p[2]), reverse=True)
            top5 = pairs[:5]

            lines.append("## Correlation Matrix")
            lines.append("")
            lines.append("| Property A | Property B | Correlation |")
            lines.append("| --- | --- | --- |")
            corr_html = ["<tr><th>Property A</th><th>Property B</th><th>Correlation</th></tr>"]
            for a, b, r in top5:
                lines.append(f"| {a} | {b} | {r:.4f} |")
                corr_html.append(f"<tr><td>{a}</td><td>{b}</td><td>{r:.4f}</td></tr>")
            lines.append("")

            # Reference correlation plot if it exists
            out_dir = Path(prefs.output_dir)
            corr_plot = out_dir / f"{dataset_name}_correlation.png"
            if corr_plot.exists():
                lines.append(f"![{dataset_name}_correlation]({corr_plot})")
                lines.append(f"*Figure: Correlation heatmap for {dataset_name}*")
                lines.append("")
                html_sections.append(
                    f"<h2>Correlation Matrix</h2><table>{''.join(corr_html)}</table>"
                    f"<figure><img src=\"{corr_plot}\" alt=\"Correlation heatmap\">"
                    f"<figcaption>Correlation heatmap for {dataset_name}</figcaption></figure>"
                )
            else:
                html_sections.append(
                    f"<h2>Correlation Matrix</h2><table>{''.join(corr_html)}</table>"
                )

        # ML prediction summary
        pred_cols = [c for c in df.columns if c.startswith("predicted_")]
        if pred_cols:
            lines.append("## ML Predictions")
            lines.append("")
            pred_html = []
            for pc in pred_cols:
                prop = pc.replace("predicted_", "")
                s = df[pc].dropna()
                if len(s) > 0:
                    lines.append(f"- **{prop}**: predicted for {len(s)} materials (mean={s.mean():.4g})")
                    pred_html.append(f"<li><strong>{prop}</strong>: predicted for {len(s)} materials (mean={s.mean():.4g})</li>")
            lines.append("")
            if pred_html:
                html_sections.append(
                    f"<h2>ML Predictions</h2><ul>{''.join(pred_html)}</ul>"
                )

    # Validation summary
    if validation_results:
        n_outliers = len(validation_results.get("outliers", []))
        n_violations = len(validation_results.get("constraint_violations", []))
        completeness = validation_results.get("completeness", {})
        overall = completeness.get("overall_completeness", 0)
        total_findings = validation_results.get("total_findings", 0)

        lines.append("## Data Quality")
        lines.append("")
        lines.append(f"- **Total findings:** {total_findings}")
        lines.append(f"- **Outliers:** {n_outliers}")
        lines.append(f"- **Constraint violations:** {n_violations}")
        lines.append(f"- **Overall completeness:** {overall:.0%}")
        lines.append("")

        quality_class = "quality-good" if total_findings == 0 else ("quality-fair" if total_findings < 5 else "quality-poor")
        html_sections.append(
            f"<h2>Data Quality</h2><ul>"
            f"<li><strong>Total findings:</strong> {total_findings}</li>"
            f"<li><strong>Outliers:</strong> {n_outliers}</li>"
            f"<li><strong>Constraint violations:</strong> {n_violations}</li>"
            f"<li class=\"{quality_class}\"><strong>Overall completeness:</strong> {overall:.0%}</li></ul>"
        )

    # Plot references with captions
    figure_num = 1
    if include_plots:
        out_dir = Path(prefs.output_dir)
        if out_dir.exists():
            pngs = sorted(out_dir.glob(f"{dataset_name}*.png"))
            if pngs:
                lines.append("## Visualizations")
                lines.append("")
                viz_html = []
                for p in pngs:
                    caption = f"Figure {figure_num}: {p.stem} \u2014 auto-generated plot"
                    lines.append(f"![{p.stem}]({p})")
                    lines.append(f"*{caption}*")
                    lines.append("")
                    viz_html.append(
                        f"<figure><img src=\"{p}\" alt=\"{p.stem}\">"
                        f"<figcaption>{caption}</figcaption></figure>"
                    )
                    figure_num += 1
                html_sections.append(
                    f"<h2>Visualizations</h2>{''.join(viz_html)}"
                )

    md_content = "\n".join(lines)

    # Determine output path
    if not output_path:
        out_dir = Path(prefs.output_dir)
        out_dir.mkdir(parents=True, exist_ok=True)
        if report_format == "html":
            output_path = str(out_dir / f"{dataset_name}_report.html")
        else:
            output_path = str(out_dir / f"{dataset_name}_report.md")

    Path(output_path).parent.mkdir(parents=True, exist_ok=True)

    # Write output based on format
    if report_format == "html":
        html_content = _build_html(title, html_sections)
        Path(output_path).write_text(html_content)
        result = {"report_path": output_path, "format": "html"}
    else:
        Path(output_path).write_text(md_content)
        result = {"report_path": output_path, "format": "markdown"}

        # Optional PDF conversion
        if report_format == "pdf" or prefs.report_format == "pdf":
            try:
                import markdown
                import weasyprint

                html = markdown.markdown(md_content, extensions=["tables"])
                pdf_path = output_path.replace(".md", ".pdf")
                weasyprint.HTML(string=html).write_pdf(pdf_path)
                result["pdf_path"] = pdf_path
                result["format"] = "pdf"
            except ImportError:
                pass  # PDF deps not installed — Markdown still saved

    return result


REPORT_SKILL = Skill(
    name="generate_report",
    description=(
        "Generate a Markdown or HTML report for a dataset including summary, "
        "data preview, property statistics, correlations, ML prediction summary, "
        "validation quality, and visualization references. Optionally converts to PDF."
    ),
    steps=[
        SkillStep("load_dataset", "Load dataset from DataStore", "internal"),
        SkillStep("build_content", "Build report content (Markdown/HTML)", "internal"),
        SkillStep("correlations", "Compute correlation matrix for numeric columns", "internal"),
        SkillStep("validation_summary", "Include data quality validation summary", "internal", optional=True),
        SkillStep("write_report", "Write report file", "internal"),
        SkillStep("convert_pdf", "Convert to PDF if configured", "internal", optional=True),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": "Name of the dataset to report on",
            },
            "title": {
                "type": "string",
                "description": "Report title (auto-generated if omitted)",
            },
            "include_plots": {
                "type": "boolean",
                "description": "Include plot references (default: true)",
            },
            "output_path": {
                "type": "string",
                "description": "Output file path (default: output/<name>_report.md)",
            },
            "format": {
                "type": "string",
                "enum": ["markdown", "html", "pdf"],
                "description": "Output format (default: from preferences)",
            },
            "validation_results": {
                "type": "object",
                "description": "Validation results from validate_dataset skill (optional)",
            },
        },
    },
    func=_generate_report,
    category="reporting",
)
