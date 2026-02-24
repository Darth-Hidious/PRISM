"""Report generation skill."""

from datetime import datetime
from pathlib import Path

from app.config.preferences import UserPreferences
from app.skills.base import Skill, SkillStep


def _generate_report(**kwargs) -> dict:
    """Generate a Markdown (optionally PDF) report for a dataset."""
    dataset_name = kwargs.get("dataset_name", "report")
    title = kwargs.get("title", f"PRISM Report — {dataset_name}")
    include_plots = kwargs.get("include_plots", True)
    output_path = kwargs.get("output_path")

    prefs = UserPreferences.load()

    from app.data.store import DataStore

    store = DataStore()

    # Load dataset if available
    df = None
    if dataset_name:
        try:
            df = store.load(dataset_name)
        except FileNotFoundError:
            pass

    # Build Markdown
    lines = [f"# {title}", "", f"*Generated: {datetime.now().strftime('%Y-%m-%d %H:%M')}*", ""]

    if df is not None:
        lines.append("## Dataset Summary")
        lines.append("")
        lines.append(f"- **Name:** {dataset_name}")
        lines.append(f"- **Rows:** {len(df)}")
        lines.append(f"- **Columns:** {len(df.columns)}")
        lines.append("")

        # Data preview table
        lines.append("## Data Preview")
        lines.append("")
        preview = df.head(10)
        cols = list(preview.columns)
        lines.append("| " + " | ".join(cols) + " |")
        lines.append("| " + " | ".join(["---"] * len(cols)) + " |")
        for _, row in preview.iterrows():
            vals = []
            for c in cols:
                v = row[c]
                if isinstance(v, float):
                    vals.append(f"{v:.4g}")
                else:
                    vals.append(str(v))
            lines.append("| " + " | ".join(vals) + " |")
        lines.append("")

        # Property statistics
        numeric_cols = df.select_dtypes(include=["float64", "float32", "int64", "int32"]).columns.tolist()
        if numeric_cols:
            lines.append("## Property Statistics")
            lines.append("")
            lines.append("| Property | Mean | Std | Min | Max |")
            lines.append("| --- | --- | --- | --- | --- |")
            for col in numeric_cols:
                s = df[col].dropna()
                if len(s) == 0:
                    continue
                lines.append(
                    f"| {col} | {s.mean():.4g} | {s.std():.4g} | {s.min():.4g} | {s.max():.4g} |"
                )
            lines.append("")

        # ML prediction summary
        pred_cols = [c for c in df.columns if c.startswith("predicted_")]
        if pred_cols:
            lines.append("## ML Predictions")
            lines.append("")
            for pc in pred_cols:
                prop = pc.replace("predicted_", "")
                s = df[pc].dropna()
                if len(s) > 0:
                    lines.append(f"- **{prop}**: predicted for {len(s)} materials (mean={s.mean():.4g})")
            lines.append("")

    # Plot references
    if include_plots:
        out_dir = Path(prefs.output_dir)
        if out_dir.exists():
            pngs = sorted(out_dir.glob(f"{dataset_name}*.png"))
            if pngs:
                lines.append("## Visualizations")
                lines.append("")
                for p in pngs:
                    lines.append(f"![{p.stem}]({p})")
                    lines.append("")

    md_content = "\n".join(lines)

    # Determine output path
    if not output_path:
        out_dir = Path(prefs.output_dir)
        out_dir.mkdir(parents=True, exist_ok=True)
        output_path = str(out_dir / f"{dataset_name}_report.md")

    Path(output_path).parent.mkdir(parents=True, exist_ok=True)
    Path(output_path).write_text(md_content)

    result = {"report_path": output_path, "format": "markdown"}

    # Optional PDF conversion
    if prefs.report_format == "pdf":
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
        "Generate a Markdown report for a dataset including summary, "
        "data preview, property statistics, ML prediction summary, "
        "and visualization references. Optionally converts to PDF."
    ),
    steps=[
        SkillStep("load_dataset", "Load dataset from DataStore", "internal"),
        SkillStep("build_markdown", "Build Markdown content", "internal"),
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
        },
    },
    func=_generate_report,
    category="reporting",
)
