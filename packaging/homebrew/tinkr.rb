class Tinkr < Formula
  desc "Tinkr optimizes your agent's context so you don't have to."
  homepage "https://github.com/thunderkex/tinkr"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/thunderkex/tinkr/releases/download/v0.1.0/tinkr-0.1.0-darwin-arm64.tar.gz"
      sha256 "REPLACE_WITH_DARWIN_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/thunderkex/tinkr/releases/download/v0.1.0/tinkr-0.1.0-darwin-x64.tar.gz"
      sha256 "REPLACE_WITH_DARWIN_X64_SHA256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/thunderkex/tinkr/releases/download/v0.1.0/tinkr-0.1.0-linux-arm64.tar.gz"
      sha256 "REPLACE_WITH_LINUX_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/thunderkex/tinkr/releases/download/v0.1.0/tinkr-0.1.0-linux-x64.tar.gz"
      sha256 "REPLACE_WITH_LINUX_X64_SHA256"
    end
  end

  def install
    bin.install "tinkr"
    bin.install "tinkr-proxy"
  end

  test do
    assert_match "tinkr", shell_output("#{bin}/tinkr --version")
  end
end
