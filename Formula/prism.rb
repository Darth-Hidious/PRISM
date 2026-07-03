class Prism < Formula
  desc "AI-native autonomous materials discovery platform node"
  homepage "https://github.com/marc27/prism"
  url "https://github.com/marc27/prism/archive/refs/tags/v2.5.0.tar.gz"
  sha256 "PLACEHOLDER" # Updated on release
  license "LicenseRef-MARC27-Dual"
  head "https://github.com/marc27/prism.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/cli")
    # Install the node binary
    system "cargo", "install", *std_cargo_args(path: "crates/node")

    # Install workflow templates
    (share/"prism/workflows").install Dir["app/workflows/builtin/*.yaml"]

    # Generate shell completions
    output = Utils.safe_popen_read(bin/"prism", "completions", "bash")
    (bash_completion/"prism").write output unless output.empty?
    output = Utils.safe_popen_read(bin/"prism", "completions", "zsh")
    (zsh_completion/"_prism").write output unless output.empty?
    output = Utils.safe_popen_read(bin/"prism", "completions", "fish")
    (fish_completion/"prism.fish").write output unless output.empty?
  end

  def caveats
    <<~EOS
      PRISM requires Docker for managed services (Neo4j, Qdrant, Kafka).
      Install Docker Desktop or Podman before running `prism node up`.

      For AI features, you need either:
        - Local Ollama: brew install ollama
        - MARC27 platform account: prism login

      Quick start:
        prism login                          # Authenticate with MARC27
        prism node up                        # Start local node
        prism ingest ./data.csv              # Ingest data
        prism query "alloys with hardness > 500"  # Query

      Configuration: ~/.prism/prism.toml
    EOS
  end

  test do
    assert_match "PRISM", shell_output("#{bin}/prism --version")
  end
end
