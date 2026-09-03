//! The source table and the descriptor card, read off a tool result.
//!
//! Every row is built from a field the tool itself wrote — the federation's
//! query log, a provenance bundle's engine and inputs, a source list — and
//! nothing else. A result that stamped none of them yields no rows, and the
//! renderer says so by name. Nothing here guesses a source.
//!
//! The rows ride the `ui.card` notification under `data.sources` and
//! `data.descriptors`, because the card's `content` is the tool's SUMMARY
//! when it has one, and the summary is where the origin of the numbers used
//! to disappear.

use serde_json::{Map, Value, json};

/// One row per source the result names: `{source, kind, count, fetched,
/// status, record}`. `record` is the tool's own entry for that source, kept
/// whole so the panel can show it.
#[must_use]
pub fn source_rows(content: &str) -> Vec<Value> {
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let data_kind = object
        .get("data_kind")
        .and_then(Value::as_str)
        .map(str::to_string);
    let fetched = fetched_stamp(object);
    let mut rows = Vec::new();

    // The federation's query log: one row per provider that was asked,
    // including the ones that failed — a reader deserves to know who was
    // asked and did not answer.
    if let Some(log) = object.get("providers_queried").and_then(Value::as_array) {
        for entry in log {
            let Some(e) = entry.as_object() else {
                continue;
            };
            let name = e
                .get("provider")
                .and_then(Value::as_str)
                .or_else(|| e.get("provider_id").and_then(Value::as_str));
            let Some(name) = name else {
                continue;
            };
            rows.push(json!({
                "source": name,
                "kind": data_kind,
                "count": e.get("result_count").and_then(Value::as_u64),
                "fetched": fetched,
                "status": e.get("status").and_then(Value::as_str),
                "record": entry,
            }));
        }
    }

    // A provenance bundle: the engine that computed the numbers, then every
    // input file it was derived from, each with its content hash.
    if let Some(prov) = object.get("provenance").and_then(Value::as_object) {
        let generated = prov.get("wasGeneratedBy").and_then(Value::as_object);
        if let Some(engine) = generated
            .and_then(|g| g.get("engine"))
            .and_then(Value::as_str)
        {
            let version = generated
                .and_then(|g| g.get("engine_version"))
                .and_then(Value::as_str)
                .unwrap_or("version absent");
            let activity = generated
                .and_then(|g| g.get("activity"))
                .and_then(Value::as_str)
                .map(|a| format!("computed: {a}"))
                .or_else(|| data_kind.clone());
            rows.push(json!({
                "source": format!("{engine} {version}"),
                "kind": activity,
                "count": Value::Null,
                "fetched": prov.get("created_at_iso8601").and_then(Value::as_str),
                "status": "computed",
                "record": Value::Object(prov.clone()),
            }));
        }
        if let Some(inputs) = prov.get("wasDerivedFrom").and_then(Value::as_array) {
            for input in inputs {
                let Some(i) = input.as_object() else {
                    continue;
                };
                let role = i.get("role").and_then(Value::as_str).unwrap_or("input");
                let source = match i.get("path").and_then(Value::as_str).map(basename) {
                    Some(path) => format!("{role}: {path}"),
                    None => role.to_string(),
                };
                let status = i
                    .get("sha256")
                    .and_then(Value::as_str)
                    .map(|hash| format!("sha256 {}", &hash[..hash.len().min(12)]))
                    .or_else(|| {
                        i.get("error")
                            .and_then(Value::as_str)
                            .map(|error| format!("unreadable: {error}"))
                    });
                rows.push(json!({
                    "source": source,
                    "kind": "input file",
                    "count": Value::Null,
                    "fetched": Value::Null,
                    "status": status,
                    "record": input,
                }));
            }
        }
    }

    // A plain source list or a single source name — what the cache and an
    // import carry — only when no richer record already covered it.
    if rows.is_empty() {
        let mut names: Vec<String> = Vec::new();
        if let Some(list) = object.get("sources").and_then(Value::as_array) {
            names.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
        if let Some(one) = object.get("source").and_then(Value::as_str) {
            names.push(one.to_string());
        }
        for name in names {
            rows.push(json!({
                "source": name,
                "kind": data_kind,
                "count": object.get("count").and_then(Value::as_u64),
                "fetched": fetched,
                "status": Value::Null,
                "record": { "source": name },
            }));
        }
    }
    rows
}

/// When the data was fetched, as the tool said it: a timestamp, or "cached"
/// with the time it was cached when the tool recorded one.
fn fetched_stamp(object: &Map<String, Value>) -> Value {
    if let Some(at) = object.get("fetched_at_iso8601").and_then(Value::as_str) {
        return json!(at);
    }
    if object.get("cached").and_then(Value::as_bool) == Some(true) {
        return match object.get("cached_at").and_then(Value::as_str) {
            Some(when) => json!(format!("cached from {when}")),
            None => json!("cached"),
        };
    }
    Value::Null
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// The descriptor card's rows, `{name, value, unit, origin}`, exactly as the
/// tool listed them under `descriptor_provenance`. A row without a name is
/// not a descriptor and is dropped; a row without an origin is kept, and the
/// renderer says the origin was not reported.
#[must_use]
pub fn descriptor_rows(content: &str) -> Vec<Value> {
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return Vec::new();
    };
    value
        .get("descriptor_provenance")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|row| row.get("name").and_then(Value::as_str).is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The federation's query log becomes one row per provider asked, failed
    /// ones included, each carrying the tool's kind and fetch time.
    #[test]
    fn a_query_log_is_one_row_per_provider_asked() {
        let content = json!({
            "formula": "Si",
            "data_kind": "crystal structure and computed properties",
            "fetched_at_iso8601": "2026-09-02T14:10:03+00:00",
            "providers_queried": [
                {"provider": "Materials Project", "provider_id": "mp", "status": "success",
                 "result_count": 1, "endpoint": "https://api.materialsproject.org"},
                {"provider": "OQMD", "provider_id": "oqmd", "status": "timeout",
                 "result_count": 0, "error": "Timed out after 8s"}
            ],
            "sources": ["Materials Project"]
        })
        .to_string();
        let rows = source_rows(&content);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0]["source"], "Materials Project");
        assert_eq!(rows[0]["kind"], "crystal structure and computed properties");
        assert_eq!(rows[0]["count"], 1);
        assert_eq!(rows[0]["fetched"], "2026-09-02T14:10:03+00:00");
        assert_eq!(rows[0]["status"], "success");
        assert_eq!(
            rows[0]["record"]["endpoint"],
            "https://api.materialsproject.org"
        );
        assert_eq!(rows[1]["source"], "OQMD");
        assert_eq!(rows[1]["status"], "timeout");
        assert_eq!(rows[1]["count"], 0);
    }

    /// A provenance bundle names the engine that computed the numbers and
    /// every input file it was derived from, hash and all.
    #[test]
    fn a_provenance_bundle_names_the_engine_and_its_inputs() {
        let content = json!({
            "gibbs_energy": -12.5,
            "provenance": {
                "created_at_iso8601": "2026-09-02T14:12:00+00:00",
                "wasGeneratedBy": {"engine": "pycalphad", "engine_version": "0.11.2",
                                   "activity": "pycalphad.equilibrium"},
                "wasDerivedFrom": [
                    {"role": "thermodynamic_database", "path": "/data/dbs/al-cu.tdb",
                     "sha256": "0123456789abcdef0123456789abcdef", "size_bytes": 4096},
                    {"role": "thermodynamic_database", "error": "no such file"}
                ],
                "reproduce": "calphad_compute(...)"
            }
        })
        .to_string();
        let rows = source_rows(&content);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows[0]["source"], "pycalphad 0.11.2");
        assert_eq!(rows[0]["kind"], "computed: pycalphad.equilibrium");
        assert_eq!(rows[0]["fetched"], "2026-09-02T14:12:00+00:00");
        assert_eq!(rows[0]["status"], "computed");
        assert_eq!(rows[0]["record"]["reproduce"], "calphad_compute(...)");
        assert_eq!(rows[1]["source"], "thermodynamic_database: al-cu.tdb");
        assert_eq!(rows[1]["kind"], "input file");
        assert_eq!(rows[1]["status"], "sha256 0123456789ab");
        assert_eq!(rows[2]["status"], "unreadable: no such file");
    }

    /// A bare source list is still a source; a result that names none
    /// yields no rows — the renderer says so, this never invents one.
    #[test]
    fn a_source_list_is_rows_and_silence_is_no_rows() {
        let listed = json!({"count": 3, "cached": true, "sources": ["local cache", "OPTIMADE:mp"]});
        let rows = source_rows(&listed.to_string());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["source"], "local cache");
        assert_eq!(rows[0]["count"], 3);
        assert_eq!(rows[0]["fetched"], "cached");
        assert!(
            rows[0]["kind"].is_null(),
            "no kind was reported, none is invented"
        );
        let imported =
            json!({"source": "user_import", "cached": true, "cached_at": "2026-09-01T09:00:00Z"});
        let rows = source_rows(&imported.to_string());
        assert_eq!(rows[0]["source"], "user_import");
        assert_eq!(rows[0]["fetched"], "cached from 2026-09-01T09:00:00Z");
        assert!(source_rows(&json!({"value": 42}).to_string()).is_empty());
        assert!(source_rows("not json").is_empty());
        assert!(source_rows("[1, 2]").is_empty());
    }

    /// Descriptor rows pass through as the tool listed them; a row with no
    /// name is not a descriptor.
    #[test]
    fn descriptor_rows_are_the_tool_s_own_list() {
        let content = json!({
            "VEC": 8.0,
            "descriptor_provenance": [
                {"name": "VEC", "value": 8.0, "unit": "electrons/atom",
                 "origin": "valence electron concentration table _VEC (Guo & Liu 2011)"},
                {"name": "omega", "value": null, "unit": "dimensionless"},
                {"value": 1.0}
            ]
        })
        .to_string();
        let rows = descriptor_rows(&content);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0]["name"], "VEC");
        assert!(rows[0]["origin"].as_str().unwrap().contains("Guo & Liu"));
        assert!(rows[1]["origin"].is_null());
        assert!(descriptor_rows(&json!({"VEC": 8.0}).to_string()).is_empty());
    }
}
