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
      url "https://github.com/simpletoolsindia/koda/releases/download/v0.1.0/koda-0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "6842bb83adea657d261f4b9e4c07a429d1e13f0a16481cc19d130cc4f3d56e99"
    end
    on_intel do
      url "https://github.com/simpletoolsindia/koda/releases/download/v0.1.0/koda-0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "b8e8f6e09eb197fd19c2f7ce25b9b7d86a1338b9535848a43700fee5827e0273"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/simpletoolsindia/koda/releases/download/v0.1.0/koda-0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c7678312337f61b8f9ce6e571ff474ec4eb2d798252a5d8f2aade4ae71741e63"
    end
    on_intel do
      url "https://github.com/simpletoolsindia/koda/releases/download/v0.1.0/koda-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "62ddce333f971c3f7c3e08a5ed0205c79dd2d55902c1fde83973df27bed2406b"
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
