# Homebrew formula for koda.
#
# This is the file the tap serves. It installs the prebuilt binary from the
# GitHub release rather than building from source, so `brew install` is a
# download rather than a Rust toolchain and a two-minute compile.
#
# There is no `version` line: Homebrew reads it off the release URL, and
# `brew audit --strict` rejects declaring it twice.
#
# Publishing: copy this to Formula/koda.rb in the simpletoolsindia/homebrew-koda
# repository. `packaging/update.py <tag>` regenerates it with the new version
# and checksums when a release goes out. See packaging/homebrew/README.md.
class Koda < Formula
  desc "Terminal coding agent that drives local LLMs and never leaves your machine"
  homepage "https://github.com/simpletoolsindia/koda"
  license "MIT"

  # ripgrep is optional at runtime: koda's `search` tool uses it when present
  # and falls back to an in-process search when it is not. Declaring it here
  # means a Homebrew install gets the fast path without a second command.
  depends_on "ripgrep" => :recommended

  on_macos do
    on_arm do
      url "https://github.com/simpletoolsindia/koda/releases/download/v1.0.0/koda-1.0.0-aarch64-apple-darwin.tar.gz"
      sha256 "46f50b4eeaacf48e969f29e13a48362a8c179c1d7a234ae2b95434eec7b7ed16"
    end
    on_intel do
      url "https://github.com/simpletoolsindia/koda/releases/download/v1.0.0/koda-1.0.0-x86_64-apple-darwin.tar.gz"
      sha256 "52f120d191ce585425faf81a4a7c06e13084cddab37c114b0bcbd9574571c059"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/simpletoolsindia/koda/releases/download/v1.0.0/koda-1.0.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "fb156e70094dca6c8c98febb39894aaf9ff06d48dc52aa05a22ea7aa33c82995"
    end
    on_intel do
      url "https://github.com/simpletoolsindia/koda/releases/download/v1.0.0/koda-1.0.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "976b9d2086987f7c4dd53335d4d1adcc78c647a4e9a38ef0aa1c09c6607b742e"
    end
  end

  def install
    bin.install "koda"
  end

  def caveats
    <<~EOS
      koda talks to a model server you run yourself. Point it at one with:
        koda   # then /setup

      Ollama, LM Studio, llama.cpp, vLLM and MLX all work, as does anything
      else speaking the OpenAI chat API.
    EOS
  end

  test do
    assert_match "koda #{version}", shell_output("#{bin}/koda --version")
    # `config` reads and prints the effective configuration without contacting
    # a model server, so it exercises real startup in a sandbox with no network.
    assert_match "base_url", shell_output("#{bin}/koda config")
  end
end
