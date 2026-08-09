//! Corpus loading for ontology induction.
//!
//! Deliberately NOT an ingest path: no connectors, no schema detection, no
//! store. A corpus is a directory (or single file) of text the model reads —
//! `.txt`/`.md`/`.markdown` verbatim, `.csv`/`.tsv` as header plus a few
//! rows (induction needs the vocabulary a table implies, not its data).
//!
//! Document order is sorted by relative path and the corpus hash covers the
//! text AS FED to the model, so a corpus hashes identically run after run
//! and the artifact's `prism:corpusHash` is a real reproducibility anchor.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Extensions accepted as corpus documents.
const TEXT_EXTENSIONS: &[&str] = &["txt", "md", "markdown"];
const TABLE_EXTENSIONS: &[&str] = &["csv", "tsv"];

/// Rows kept from a tabular file (after the header).
const TABLE_SAMPLE_ROWS: usize = 5;

/// Hard cap per document, in bytes read — induction prompts are capped far
/// below this anyway; the cap only stops a runaway file from being slurped.
const MAX_DOC_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct CorpusDoc {
    /// Path relative to the corpus root (or the file name for a single file).
    pub rel_path: String,
    /// The text as it will be fed to the model (already row-truncated for
    /// tabular files).
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub root: PathBuf,
    /// Sorted by `rel_path`.
    pub docs: Vec<CorpusDoc>,
    /// `sha256:<hex>` over every document's `rel_path` and text, in order.
    pub hash: String,
}

fn accepted_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    TEXT_EXTENSIONS
        .iter()
        .chain(TABLE_EXTENSIONS)
        .find(|e| **e == ext)
        .copied()
}

fn read_doc(path: &Path, ext: &str) -> Result<String> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("cannot stat corpus file {}", path.display()))?;
    if meta.len() > MAX_DOC_BYTES {
        tracing::warn!(
            file = %path.display(),
            "corpus file larger than {MAX_DOC_BYTES} bytes; reading only the head"
        );
    }
    let raw = std::fs::read(path)
        .with_context(|| format!("cannot read corpus file {}", path.display()))?;
    let head = &raw[..raw.len().min(MAX_DOC_BYTES as usize)];
    let text = String::from_utf8_lossy(head);
    if TABLE_EXTENSIONS.contains(&ext) {
        // Header + first rows: the column names and a feel for the values is
        // what induction needs; the full table is instance data.
        Ok(text
            .lines()
            .take(1 + TABLE_SAMPLE_ROWS)
            .collect::<Vec<_>>()
            .join("\n"))
    } else {
        Ok(text.into_owned())
    }
}

/// Load a corpus from a file or directory. Hidden files and unrecognised
/// extensions are skipped; an EMPTY corpus is a loud error, because inducing
/// from nothing would hand the model a blank cheque.
pub fn load_corpus(path: &Path) -> Result<Corpus> {
    let mut docs = Vec::new();
    if path.is_file() {
        let Some(ext) = accepted_extension(path) else {
            bail!(
                "corpus file {} has an unsupported extension (supported: {})",
                path.display(),
                supported_list()
            );
        };
        docs.push(CorpusDoc {
            rel_path: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            text: read_doc(path, ext)?,
        });
    } else if path.is_dir() {
        collect_dir(path, path, &mut docs)?;
    } else {
        bail!("corpus path {} does not exist", path.display());
    }

    docs.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    if docs.is_empty() {
        bail!(
            "corpus at {} contains no usable documents (supported: {})",
            path.display(),
            supported_list()
        );
    }

    let mut hasher = Sha256::new();
    for doc in &docs {
        hasher.update(doc.rel_path.as_bytes());
        hasher.update([0u8]);
        hasher.update(doc.text.as_bytes());
        hasher.update([0u8]);
    }
    let hash = format!("sha256:{}", hex::encode(hasher.finalize()));

    Ok(Corpus {
        root: path.to_path_buf(),
        docs,
        hash,
    })
}

fn supported_list() -> String {
    TEXT_EXTENSIONS
        .iter()
        .chain(TABLE_EXTENSIONS)
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn collect_dir(root: &Path, dir: &Path, docs: &mut Vec<CorpusDoc>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read corpus directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_dir(root, &path, docs)?;
        } else if let Some(ext) = accepted_extension(&path) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            docs.push(CorpusDoc {
                rel_path: rel,
                text: read_doc(&path, ext)?,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn corpus_is_sorted_and_hash_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "b.md", "beta doc");
        write(dir.path(), "a.txt", "alpha doc");
        write(dir.path(), "sub/c.md", "gamma doc");
        let c1 = load_corpus(dir.path()).unwrap();
        let c2 = load_corpus(dir.path()).unwrap();
        assert_eq!(
            c1.docs
                .iter()
                .map(|d| d.rel_path.as_str())
                .collect::<Vec<_>>(),
            ["a.txt", "b.md", "sub/c.md"]
        );
        assert_eq!(c1.hash, c2.hash, "same corpus must hash identically");
        assert!(c1.hash.starts_with("sha256:"));
    }

    #[test]
    fn csv_is_truncated_to_header_plus_sample_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut csv = String::from("Composition,Hardness_HV\n");
        for i in 0..50 {
            csv.push_str(&format!("Alloy{i},{}\n", 400 + i));
        }
        write(dir.path(), "alloys.csv", &csv);
        let c = load_corpus(dir.path()).unwrap();
        let lines: Vec<&str> = c.docs[0].text.lines().collect();
        assert_eq!(lines.len(), 1 + TABLE_SAMPLE_ROWS);
        assert_eq!(lines[0], "Composition,Hardness_HV");
    }

    #[test]
    fn hash_covers_text_as_fed_so_extra_rows_do_not_change_it() {
        // Rows beyond the sample are never fed to the model, so they must
        // not perturb the reproducibility anchor either.
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let base = "A,B\n1,2\n3,4\n5,6\n7,8\n9,10\n";
        write(dir1.path(), "t.csv", base);
        write(dir2.path(), "t.csv", &format!("{base}11,12\n13,14\n"));
        let c1 = load_corpus(dir1.path()).unwrap();
        let c2 = load_corpus(dir2.path()).unwrap();
        assert_eq!(c1.hash, c2.hash);
    }

    #[test]
    fn empty_corpus_is_a_loud_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ignored.pdf", "binary-ish");
        let err = load_corpus(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("no usable documents"));
    }

    #[test]
    fn missing_path_is_a_loud_error() {
        let err = load_corpus(Path::new("/nonexistent/prism-corpus")).unwrap_err();
        assert!(format!("{err:#}").contains("does not exist"));
    }

    #[test]
    fn hidden_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".hidden.md", "secret");
        write(dir.path(), "visible.md", "hello");
        let c = load_corpus(dir.path()).unwrap();
        assert_eq!(c.docs.len(), 1);
        assert_eq!(c.docs[0].rel_path, "visible.md");
    }
}
