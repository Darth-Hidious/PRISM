"""Spark data processing tools for large-scale ETL on materials data."""
import json
import os
from app.tools.base import Tool, ToolRegistry


def _check_spark_available() -> bool:
    """Check if PySpark is importable."""
    try:
        import pyspark  # noqa: F401
        return True
    except ImportError:
        return False


def _spark_master_url() -> str:
    """Resolve Spark master URL from env or default."""
    return os.getenv("SPARK_MASTER_URL", "spark://localhost:7077")


def _spark_submit_job(**kwargs) -> dict:
    """Submit a PySpark job for large-scale data processing."""
    if not _check_spark_available():
        return {"error": "PySpark not installed. Run: pip install pyspark"}

    from pyspark.sql import SparkSession

    job_type = kwargs.get("job_type", "transform")
    input_path = kwargs.get("input_path")
    output_path = kwargs.get("output_path")
    sql_query = kwargs.get("sql_query")
    app_name = kwargs.get("app_name", "prism-etl")

    if not input_path:
        return {"error": "input_path is required"}

    try:
        spark = SparkSession.builder \
            .master(_spark_master_url()) \
            .appName(app_name) \
            .config("spark.ui.showConsoleProgress", "false") \
            .getOrCreate()

        # Detect format from extension
        ext = os.path.splitext(input_path)[1].lower()
        if ext == ".parquet":
            df = spark.read.parquet(input_path)
        elif ext == ".csv":
            df = spark.read.csv(input_path, header=True, inferSchema=True)
        elif ext == ".json":
            df = spark.read.json(input_path)
        else:
            spark.stop()
            return {"error": f"Unsupported format: {ext}. Use .parquet, .csv, or .json"}

        if job_type == "sql" and sql_query:
            df.createOrReplaceTempView("data")
            result_df = spark.sql(sql_query)
        elif job_type == "describe":
            summary = {
                "columns": df.columns,
                "row_count": df.count(),
                "schema": df.schema.simpleString(),
                "sample": [row.asDict() for row in df.head(5)],
            }
            spark.stop()
            return {"result": summary}
        else:
            result_df = df

        if output_path:
            out_ext = os.path.splitext(output_path)[1].lower()
            if out_ext == ".parquet":
                result_df.write.mode("overwrite").parquet(output_path)
            elif out_ext == ".csv":
                result_df.write.mode("overwrite").csv(output_path, header=True)
            elif out_ext == ".json":
                result_df.write.mode("overwrite").json(output_path)
            else:
                spark.stop()
                return {"error": f"Unsupported output format: {out_ext}"}

            row_count = result_df.count()
            spark.stop()
            return {
                "status": "complete",
                "output_path": output_path,
                "row_count": row_count,
                "columns": result_df.columns,
            }

        # No output path — return sample
        sample = [row.asDict() for row in result_df.head(20)]
        row_count = result_df.count()
        spark.stop()
        return {
            "status": "complete",
            "row_count": row_count,
            "columns": result_df.columns,
            "sample": sample,
        }

    except Exception as e:
        return {"error": str(e)}


def _spark_status(**kwargs) -> dict:
    """Check Spark cluster status and available resources."""
    if not _check_spark_available():
        return {"error": "PySpark not installed. Run: pip install pyspark"}

    try:
        from pyspark.sql import SparkSession

        spark = SparkSession.builder \
            .master(_spark_master_url()) \
            .appName("prism-status-check") \
            .config("spark.ui.showConsoleProgress", "false") \
            .getOrCreate()

        sc = spark.sparkContext
        status = {
            "master": sc.master,
            "app_name": sc.appName,
            "spark_version": sc.version,
            "default_parallelism": sc.defaultParallelism,
            "status": "connected",
        }
        spark.stop()
        return status
    except Exception as e:
        return {"error": str(e), "master_url": _spark_master_url()}


def _spark_batch_transform(**kwargs) -> dict:
    """Run a batch transformation pipeline on materials data.

    Supports: dedup, filter, aggregate, join operations.
    """
    if not _check_spark_available():
        return {"error": "PySpark not installed. Run: pip install pyspark"}

    from pyspark.sql import SparkSession

    input_path = kwargs.get("input_path")
    output_path = kwargs.get("output_path")
    operations = kwargs.get("operations", [])

    if not input_path:
        return {"error": "input_path is required"}
    if not operations:
        return {"error": "operations list is required (e.g. ['dedup:formula', 'filter:band_gap>0.5'])"}

    try:
        spark = SparkSession.builder \
            .master(_spark_master_url()) \
            .appName("prism-batch-transform") \
            .config("spark.ui.showConsoleProgress", "false") \
            .getOrCreate()

        ext = os.path.splitext(input_path)[1].lower()
        if ext == ".parquet":
            df = spark.read.parquet(input_path)
        elif ext == ".csv":
            df = spark.read.csv(input_path, header=True, inferSchema=True)
        elif ext == ".json":
            df = spark.read.json(input_path)
        else:
            spark.stop()
            return {"error": f"Unsupported format: {ext}"}

        applied = []
        for op in operations:
            if op.startswith("dedup:"):
                col = op[len("dedup:"):]
                df = df.dropDuplicates([col])
                applied.append(f"deduplicated on {col}")
            elif op.startswith("filter:"):
                expr = op[len("filter:"):]
                df = df.filter(expr)
                applied.append(f"filtered: {expr}")
            elif op.startswith("sort:"):
                col = op[len("sort:"):]
                df = df.orderBy(col)
                applied.append(f"sorted by {col}")
            else:
                applied.append(f"unknown op: {op}")

        row_count = df.count()
        result = {
            "status": "complete",
            "row_count": row_count,
            "columns": df.columns,
            "operations_applied": applied,
        }

        if output_path:
            out_ext = os.path.splitext(output_path)[1].lower()
            if out_ext == ".parquet":
                df.write.mode("overwrite").parquet(output_path)
            elif out_ext == ".csv":
                df.write.mode("overwrite").csv(output_path, header=True)
            elif out_ext == ".json":
                df.write.mode("overwrite").json(output_path)
            result["output_path"] = output_path
        else:
            result["sample"] = [row.asDict() for row in df.head(10)]

        spark.stop()
        return result

    except Exception as e:
        return {"error": str(e)}


def create_spark_tools(registry: ToolRegistry) -> None:
    """Register Spark data processing tools."""
    registry.register(Tool(
        name="spark_submit_job",
        description="Submit a PySpark job for large-scale data processing. Supports reading/writing Parquet, CSV, JSON files. Can run SQL queries on data, describe datasets, or transform and output results. Use for datasets too large for in-memory processing.",
        input_schema={
            "type": "object",
            "properties": {
                "input_path": {
                    "type": "string",
                    "description": "Path to input data file (.parquet, .csv, .json)",
                },
                "output_path": {
                    "type": "string",
                    "description": "Path to write output (optional — returns sample if omitted)",
                },
                "job_type": {
                    "type": "string",
                    "enum": ["transform", "sql", "describe"],
                    "description": "Job type: 'transform' (passthrough/filter), 'sql' (run SQL), 'describe' (dataset summary)",
                },
                "sql_query": {
                    "type": "string",
                    "description": "SQL query to run (requires job_type='sql'). Table name is 'data'.",
                },
                "app_name": {
                    "type": "string",
                    "description": "Spark application name (default: prism-etl)",
                },
            },
            "required": ["input_path"],
        },
        func=_spark_submit_job,
    ))

    registry.register(Tool(
        name="spark_status",
        description="Check Spark cluster status — connection, version, available parallelism. Use to verify Spark is running before submitting jobs.",
        input_schema={
            "type": "object",
            "properties": {},
        },
        func=_spark_status,
    ))

    registry.register(Tool(
        name="spark_batch_transform",
        description="Run a batch transformation pipeline on materials data files. Supports operations: dedup:<column>, filter:<sql_expression>, sort:<column>. Chain multiple operations in sequence for ETL pipelines.",
        input_schema={
            "type": "object",
            "properties": {
                "input_path": {
                    "type": "string",
                    "description": "Path to input data file (.parquet, .csv, .json)",
                },
                "output_path": {
                    "type": "string",
                    "description": "Path to write transformed output (optional)",
                },
                "operations": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of operations: 'dedup:column', 'filter:expr', 'sort:column'",
                },
            },
            "required": ["input_path", "operations"],
        },
        func=_spark_batch_transform,
    ))
