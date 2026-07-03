// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Live smoke test for the native ONNX backend.
//!
//! Ignored by default: the first run downloads ~90 MB of model weights into
//! `~/.prism/models/embed/`. Run explicitly with:
//!
//! ```sh
//! cargo test -p prism-embed --test native_live -- --ignored --nocapture
//! ```

use prism_embed::{EmbedBackend, NativeOnnx, cosine_similarity};

#[tokio::test]
#[ignore = "downloads the embedding model (~90 MB) on first run"]
async fn related_sentences_score_higher_than_unrelated() {
    let backend = NativeOnnx::new().expect("native backend init (needs network on first run)");
    assert_eq!(backend.dimensions(), 384);
    assert!(backend.id().starts_with("native:"));

    let texts = vec![
        "Nickel superalloys retain strength at high temperature.".to_string(),
        "High-temperature alloys based on nickel resist creep.".to_string(),
        "The cat sat on the windowsill watching birds.".to_string(),
    ];
    let vecs = backend.embed(&texts).await.expect("embedding failed");
    assert_eq!(vecs.len(), 3);
    assert_eq!(vecs[0].len(), 384);

    let related = cosine_similarity(&vecs[0], &vecs[1]);
    let unrelated_a = cosine_similarity(&vecs[0], &vecs[2]);
    let unrelated_b = cosine_similarity(&vecs[1], &vecs[2]);
    println!("related={related:.4} unrelated_a={unrelated_a:.4} unrelated_b={unrelated_b:.4}");

    assert!(
        related > unrelated_a && related > unrelated_b,
        "related pair should out-score the unrelated pairs \
         (related={related:.4}, unrelated_a={unrelated_a:.4}, unrelated_b={unrelated_b:.4})"
    );
}
