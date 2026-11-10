class Bb < Formula
  desc "Command line client for S3 and S3-compatible object storage (BucketBridge)"
  homepage "https://github.com/anupshaw50/BucketBridge"
  version "0.4.0"
  license "GPL-3.0-only"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_macos_universal.tar.gz"
    sha256 "167363962c000eb05c94ad426d1f7aceebea4906226c5639f3666e3e5064ba63"
  end

  on_linux do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_linux_x86_64.tar.gz"
    sha256 "b97e3c388c3fa619f89bf4a3141e5148c14245deadf0efb1f60ebc5c2c76d6e6"
  end

  def install
    bin.install "bb"
  end

  test do
    assert_match "bb", shell_output("#{bin}/bb --help")
  end
end
