use std::fs;
use std::path::Path;

/// The dashboard's build output is gitignored, so a fresh clone does not have
/// it. `rust-embed`'s derive over a missing folder generates no `get()`, and
/// `prism-server` then fails to compile with three `E0599`s — meaning
/// `cargo build` fails on a clean checkout until someone happens to know they
/// must run the dashboard's npm build first.
///
/// A build must never fail for want of an optional artifact. This creates the
/// directory when it is absent so the derive always has something to embed.
/// Serving already degrades honestly on its own: `serve_dashboard` returns
/// `404 dashboard not found` when no `index.html` is embedded, so an empty
/// directory produces a node whose API works and whose web UI says it is not
/// built — which is the truth, and is recoverable by running the npm build.
///
/// The placeholder is only written when the directory did not exist at all. A
/// real `dashboard/dist` is never touched, so this cannot shadow a genuine
/// build.
fn main() {
    println!("cargo:rerun-if-changed=../../dashboard/dist");

    let dist = Path::new("../../dashboard/dist");
    if dist.exists() {
        return;
    }

    if let Err(e) = fs::create_dir_all(dist) {
        // Do not fail the build over this. A read-only or sandboxed build tree
        // is a legitimate environment; the compile error it produces is the
        // pre-existing behaviour, and failing here would only replace one
        // build failure with a less informative one.
        println!("cargo:warning=could not create {}: {e}", dist.display());
        return;
    }

    // A minimal, honest placeholder rather than an empty directory: an empty
    // dist makes `serve_dashboard` 404 with no explanation, which reads as a
    // broken node rather than an unbuilt UI.
    let placeholder = dist.join("index.html");
    let body = "<!doctype html><meta charset=\"utf-8\">\
<title>PRISM — dashboard not built</title>\
<p>The PRISM node is running and its API is available.</p>\
<p>This web dashboard was not built into this binary. \
To build it, run <code>npm install &amp;&amp; npm run build</code> in <code>dashboard/</code> \
and rebuild.</p>\n";
    if let Err(e) = fs::write(&placeholder, body) {
        println!(
            "cargo:warning=could not write {}: {e}",
            placeholder.display()
        );
    }
}
