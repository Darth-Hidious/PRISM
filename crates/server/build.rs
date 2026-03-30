fn main() {
    // Re-run if the dashboard build output changes so rust-embed picks up new assets.
    println!("cargo:rerun-if-changed=../../dashboard/dist");
}
