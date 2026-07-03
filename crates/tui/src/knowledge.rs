//! Knowledge pane — one flow for the knowledge verbs.
//!
//! The palette used to carry near-duplicate entries (search literature
//! & KG / ingest paper) that each fired a bare scaffold into the chat.
//! This pane consolidates them: mode tabs `[Search | Ingest]`, where
//! Search collects a query + scope toggles and Ingest is a real file
//! browser (arrow navigation, Enter to descend/select, filtered to
//! .pdf/.csv/.json) followed by optional metadata.
//!
//! State + pure helpers live here (gh.rs convention); key handling is
//! in app.rs and rendering in render.rs.

use crate::form::{Form, FormField};
use std::path::PathBuf;

/// Which mode tab is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KnowledgeTab {
    Search,
    Ingest,
}

/// Ingest tab phase: picking a file, then optional metadata.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum IngestPhase {
    #[default]
    Browse,
    Meta,
}

/// File extensions the ingest browser offers.
pub const INGEST_EXTENSIONS: &[&str] = &["pdf", "csv", "json"];

/// One row of the file browser.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
}

/// A minimal directory browser: dirs + ingestable files, dirs first.
#[derive(Debug, Clone, Default)]
pub struct FileBrowser {
    pub cwd: PathBuf,
    pub entries: Vec<FileEntry>,
    pub selected: usize,
}

impl FileBrowser {
    pub fn new(start: PathBuf) -> Self {
        let mut b = Self {
            cwd: start,
            entries: Vec::new(),
            selected: 0,
        };
        b.refresh();
        b
    }

    /// Re-read `cwd`: a `..` row (when there is a parent), then visible
    /// directories, then files matching [`INGEST_EXTENSIONS`] — each
    /// group alphabetical. Unreadable dirs collapse to just `..`.
    pub fn refresh(&mut self) {
        let mut dirs: Vec<FileEntry> = Vec::new();
        let mut files: Vec<FileEntry> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.cwd) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    dirs.push(FileEntry { name, is_dir: true });
                } else if has_ingest_extension(&name) {
                    files.push(FileEntry {
                        name,
                        is_dir: false,
                    });
                }
            }
        }
        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        files.sort_by(|a, b| a.name.cmp(&b.name));

        self.entries = Vec::new();
        if self.cwd.parent().is_some() {
            self.entries.push(FileEntry {
                name: "..".to_string(),
                is_dir: true,
            });
        }
        self.entries.extend(dirs);
        self.entries.extend(files);
        self.selected = 0;
    }

    /// Enter on the selected row: descend into a directory (returns
    /// `None`) or pick a file (returns its full path).
    pub fn enter(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selected)?.clone();
        if entry.is_dir {
            if entry.name == ".." {
                self.up();
            } else {
                self.cwd = self.cwd.join(&entry.name);
                self.refresh();
            }
            None
        } else {
            Some(self.cwd.join(&entry.name))
        }
    }

    /// Go to the parent directory (no-op at the filesystem root).
    pub fn up(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            self.cwd = parent.to_path_buf();
            self.refresh();
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, n as i32 - 1);
        self.selected = next as usize;
    }
}

fn has_ingest_extension(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .map(|ext| {
            INGEST_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
        .unwrap_or(false)
        && name.contains('.')
}

/// The Knowledge pane state: mode tabs + per-tab state.
#[derive(Debug, Clone, Default)]
pub struct KnowledgePane {
    pub open: bool,
    pub tab: Option<KnowledgeTab>,
    pub search_form: Form,
    pub browser: FileBrowser,
    pub phase: IngestPhase,
    /// The picked file awaiting metadata (Ingest tab, Meta phase).
    pub ingest_file: Option<PathBuf>,
    pub meta_form: Form,
}

impl KnowledgePane {
    /// Fresh pane state opened on `tab`, browsing from `start_dir`.
    pub fn opened(tab: KnowledgeTab, start_dir: PathBuf) -> Self {
        Self {
            open: true,
            tab: Some(tab),
            search_form: search_form(),
            browser: FileBrowser::new(start_dir),
            phase: IngestPhase::Browse,
            ingest_file: None,
            meta_form: meta_form(),
        }
    }

    pub fn active_tab(&self) -> KnowledgeTab {
        self.tab.unwrap_or(KnowledgeTab::Search)
    }
}

fn search_form() -> Form {
    Form::new(
        "Search",
        "search",
        vec![
            FormField::text("query", "Query", ""),
            FormField::toggle("literature", "Literature", true),
            FormField::toggle("kg", "Knowledge Graph", true),
        ],
    )
}

fn meta_form() -> Form {
    Form::new(
        "Metadata",
        "ingest",
        vec![
            FormField::text("title", "Title", "").with_note("optional"),
            FormField::text("notes", "Notes", "").with_note("optional"),
        ],
    )
}

/// Compose the search prompt from the form, or `None` when invalid
/// (empty query / no scope selected).
pub fn search_prompt(form: &Form) -> Option<String> {
    let query = form.text_value("query").trim().to_string();
    if query.is_empty() {
        return None;
    }
    let lit = form.toggle_value("literature");
    let kg = form.toggle_value("kg");
    let scope = match (kg, lit) {
        (true, true) => "the knowledge graph and literature",
        (true, false) => "the knowledge graph",
        (false, true) => "the literature",
        (false, false) => return None,
    };
    Some(format!("Search {scope} for {query}"))
}

/// Compose the ingest prompt for a picked file + optional metadata.
pub fn ingest_prompt(path: &std::path::Path, meta: &Form) -> String {
    let mut prompt = format!(
        "Ingest this file into the knowledge graph: {}",
        path.display()
    );
    let title = meta.text_value("title").trim().to_string();
    let notes = meta.text_value("notes").trim().to_string();
    if !title.is_empty() {
        prompt.push_str(&format!(" (title: {title})"));
    }
    if !notes.is_empty() {
        prompt.push_str(&format!(" — notes: {notes}"));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_filter_is_case_insensitive_and_strict() {
        assert!(has_ingest_extension("paper.pdf"));
        assert!(has_ingest_extension("DATA.CSV"));
        assert!(has_ingest_extension("graph.json"));
        assert!(!has_ingest_extension("notes.txt"));
        assert!(!has_ingest_extension("pdf"), "bare 'pdf' is not a match");
        assert!(!has_ingest_extension("archive.tar.gz"));
    }

    #[test]
    fn browser_lists_dirs_first_then_filtered_files() {
        let root =
            std::env::temp_dir().join(format!("prism-tui-browser-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("zdir")).unwrap();
        std::fs::write(root.join("a.pdf"), b"x").unwrap();
        std::fs::write(root.join("b.txt"), b"x").unwrap();
        std::fs::write(root.join("c.json"), b"x").unwrap();
        std::fs::write(root.join(".hidden.pdf"), b"x").unwrap();

        let b = FileBrowser::new(root.clone());
        let names: Vec<&str> = b.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["..", "zdir", "a.pdf", "c.json"],
            "dirs first, .txt and hidden files excluded"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn browser_enter_descends_and_picks() {
        let root =
            std::env::temp_dir().join(format!("prism-tui-browser-enter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("papers")).unwrap();
        std::fs::write(root.join("papers/x.pdf"), b"x").unwrap();

        let mut b = FileBrowser::new(root.clone());
        // Rows: [.., papers]. Descend into papers.
        b.selected = 1;
        assert_eq!(b.enter(), None, "entering a dir returns no file");
        assert!(b.cwd.ends_with("papers"));
        // Rows: [.., x.pdf]. Pick the file.
        b.selected = 1;
        assert_eq!(b.enter(), Some(root.join("papers/x.pdf")));
        // `..` climbs back out.
        b.selected = 0;
        assert_eq!(b.enter(), None);
        assert_eq!(b.cwd, root);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_prompt_reflects_scope_toggles() {
        let mut f = search_form();
        assert_eq!(search_prompt(&f), None, "empty query is invalid");
        if let crate::form::FieldKind::Text { value } = &mut f.fields[0].kind {
            value.push_str("NiTi");
        }
        assert_eq!(
            search_prompt(&f).as_deref(),
            Some("Search the knowledge graph and literature for NiTi")
        );
        if let crate::form::FieldKind::Toggle { value } = &mut f.fields[1].kind {
            *value = false; // literature off
        }
        assert_eq!(
            search_prompt(&f).as_deref(),
            Some("Search the knowledge graph for NiTi")
        );
        if let crate::form::FieldKind::Toggle { value } = &mut f.fields[2].kind {
            *value = false; // kg off too
        }
        assert_eq!(search_prompt(&f), None, "no scope selected is invalid");
    }

    #[test]
    fn ingest_prompt_includes_optional_metadata() {
        let mut m = meta_form();
        let path = std::path::Path::new("/data/papers/niti.pdf");
        assert_eq!(
            ingest_prompt(path, &m),
            "Ingest this file into the knowledge graph: /data/papers/niti.pdf"
        );
        if let crate::form::FieldKind::Text { value } = &mut m.fields[0].kind {
            value.push_str("NiTi review");
        }
        if let crate::form::FieldKind::Text { value } = &mut m.fields[1].kind {
            value.push_str("for WP4");
        }
        assert_eq!(
            ingest_prompt(path, &m),
            "Ingest this file into the knowledge graph: /data/papers/niti.pdf \
             (title: NiTi review) — notes: for WP4"
        );
    }
}
