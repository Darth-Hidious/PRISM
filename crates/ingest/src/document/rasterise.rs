//! Turning a PDF page into pixels.
//!
//! PRISM does not link a PDF rasteriser. That is a licence decision as much as
//! a dependency one: the capable renderers are variously BSD (pdfium, whose
//! binaries are not distributed through crates.io and must be provisioned),
//! GPL (poppler) and AGPL (mupdf), and a source-available product under an
//! ESA BIPR cannot quietly link the last two. Running an installed renderer as
//! a SEPARATE PROCESS keeps that boundary clean and keeps the choice the
//! operator's, which is why [`CommandRasteriser`] describes a command rather
//! than binding a library.
//!
//! It also keeps the promise that `build`/`install` never fails: a missing
//! renderer is a [`Readiness::Unavailable`] with an install line in it, found
//! before any page is read, not a build error and not a crash mid-ingest.

use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{PageRasteriser, Readiness};

/// A rasteriser that shells out to an installed renderer.
///
/// The command and its argument shape live HERE, in one declaration, so
/// pointing PRISM at a different renderer is data rather than a code change.
pub struct CommandRasteriser {
    id: &'static str,
    /// Executable name, looked up on `PATH`.
    program: &'static str,
    /// How to say "install this" when it is missing.
    install_hint: &'static str,
    /// Builds the argument list for one page at one resolution. Writes PNG to
    /// stdout — no temporary files, so a failed render leaves nothing behind
    /// and concurrent reads cannot collide on a path.
    args: fn(page: u32, dpi: u32) -> Vec<String>,
}

impl CommandRasteriser {
    /// Poppler's `pdftoppm`, the renderer most likely to be present already
    /// (it ships with poppler-utils on Linux and `brew install poppler` on
    /// macOS) and the one PRISM's own PDF tooling was verified against.
    pub fn poppler() -> Self {
        Self {
            id: "poppler",
            program: "pdftoppm",
            install_hint: "install poppler (`brew install poppler`, or \
                           `apt install poppler-utils`)",
            args: |page, dpi| {
                vec![
                    "-png".into(),
                    // Single page: first == last.
                    "-f".into(),
                    page.to_string(),
                    "-l".into(),
                    page.to_string(),
                    "-r".into(),
                    dpi.to_string(),
                    // Read the PDF from stdin, write the image to stdout.
                    "-".into(),
                ]
            },
        }
    }
}

impl PageRasteriser for CommandRasteriser {
    fn id(&self) -> &'static str {
        self.id
    }

    fn readiness(&self) -> Readiness {
        // Probed by asking the OS to resolve it, not by running it: a
        // renderer that is present but slow must not cost a subprocess on
        // every readiness check.
        match which_on_path(self.program) {
            true => Readiness::Ready,
            false => Readiness::Unavailable(format!(
                "'{}' is not on PATH, so pages cannot be rendered for a vision model — {}",
                self.program, self.install_hint
            )),
        }
    }

    fn render(&self, pdf: &[u8], page: u32, dpi: u32) -> Result<Vec<u8>> {
        use std::io::Write as _;
        let mut child = Command::new(self.program)
            .args((self.args)(page, dpi))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", self.program))?;
        child
            .stdin
            .take()
            .context("renderer stdin was not piped")?
            .write_all(pdf)
            .with_context(|| format!("sending the PDF to {}", self.program))?;
        let out = child
            .wait_with_output()
            .with_context(|| format!("waiting for {}", self.program))?;
        if !out.status.success() {
            bail!(
                "{} failed rendering page {page}: {}",
                self.program,
                String::from_utf8_lossy(&out.stderr).trim(),
            );
        }
        if out.stdout.is_empty() {
            bail!(
                "{} produced no image for page {page} — the page may not exist",
                self.program
            );
        }
        Ok(out.stdout)
    }
}

/// Whether `program` resolves on `PATH`.
fn which_on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        // Existence is enough: an entry on PATH that is not executable is a
        // broken installation, and the render call reports that far more
        // precisely than a permissions guess here would.
        candidate.is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declaration must ask for ONE page as PNG at the requested
    /// resolution, reading stdin. A rasteriser that quietly rendered the
    /// whole document would make every page cost the whole paper.
    #[test]
    fn the_poppler_declaration_asks_for_one_page_as_png() {
        let r = CommandRasteriser::poppler();
        let args = (r.args)(7, 200);
        assert!(args.contains(&"-png".to_string()));
        assert_eq!(
            args.iter().filter(|a| *a == "7").count(),
            2,
            "first == last"
        );
        let first = args.iter().position(|a| a == "-f").expect("-f present");
        let last = args.iter().position(|a| a == "-l").expect("-l present");
        assert_eq!(args[first + 1], "7");
        assert_eq!(args[last + 1], "7");
        let res = args.iter().position(|a| a == "-r").expect("-r present");
        assert_eq!(args[res + 1], "200");
        assert_eq!(args.last().map(String::as_str), Some("-"), "reads stdin");
    }

    /// An absent renderer is reported BEFORE any page is read, with a line
    /// the user can act on — not discovered as a mid-ingest crash.
    #[test]
    fn a_missing_renderer_reports_how_to_install_it() {
        let missing = CommandRasteriser {
            id: "nope",
            program: "prism-definitely-not-a-real-renderer",
            install_hint: "install the thing",
            args: |_, _| vec![],
        };
        match missing.readiness() {
            Readiness::Unavailable(reason) => {
                assert!(reason.contains("not on PATH"), "{reason}");
                assert!(reason.contains("install the thing"), "{reason}");
            }
            Readiness::Ready => panic!("a nonexistent program must not be Ready"),
        }
    }

    /// Renders a real page if poppler happens to be installed. Ignored by
    /// default so the suite does not depend on the host's tooling; this is
    /// the check that the argument declaration above is actually CORRECT and
    /// not merely self-consistent.
    #[test]
    #[ignore = "requires poppler on PATH"]
    fn renders_a_real_page_to_png() {
        let r = CommandRasteriser::poppler();
        assert!(r.readiness().is_ready(), "poppler must be installed");
        let pdf = std::fs::read(
            std::env::var("PRISM_TEST_PDF").expect("set PRISM_TEST_PDF to a real pdf"),
        )
        .expect("read the pdf");
        let png = r.render(&pdf, 1, 100).expect("render page 1");
        assert_eq!(&png[1..4], b"PNG", "output must be a PNG");
    }
}
