# Homebrew formula template for dq.
#
# Lives here as the canonical source. The mazuninky/homebrew-tap repo's
# release automation copies this file on every tag, fills in `version`,
# `url`, and `sha256` placeholders, and commits the result to the tap.
#
# Install (once tap is bootstrapped):
#   brew install mazuninky/tap/dq

class Dq < Formula
  desc "Agent-friendly Rust CLI for structured data (yq/dasel drop-in) + linter platform"
  homepage "https://github.com/mazuninky/dq"
  version "0.6.0"  # rewritten by tap automation
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/mazuninky/dq/releases/download/v#{version}/dq-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE-WITH-SHA256"  # rewritten by tap automation
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/mazuninky/dq/releases/download/v#{version}/dq-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE-WITH-SHA256"
    elsif Hardware::CPU.arm?
      url "https://github.com/mazuninky/dq/releases/download/v#{version}/dq-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE-WITH-SHA256"
    end
  end

  def install
    bin.install "dq"
    man1.install Dir["man/dq*.1"] if Dir.exist?("man")
    bash_completion.install "completions/dq.bash" if File.exist?("completions/dq.bash")
    zsh_completion.install   "completions/_dq"     if File.exist?("completions/_dq")
    fish_completion.install  "completions/dq.fish" if File.exist?("completions/dq.fish")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dq --version")
  end
end
