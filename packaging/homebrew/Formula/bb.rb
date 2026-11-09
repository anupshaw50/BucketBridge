class Bb < Formula
  desc "Command line client for S3 and S3-compatible object storage (BucketBridge)"
  homepage "https://github.com/anupshaw50/BucketBridge"
  version "0.3.0"
  license "GPL-3.0-only"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_macos_universal.tar.gz"
    sha256 "bbbd8cc9f289bbb0ca4b6612191b376e00a8f61412deaaa0a171240d3b7141df"
  end

  on_linux do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_linux_x86_64.tar.gz"
    sha256 "504d6ad3f198d21ba23cbb15c03acd93daebadba77c3d2fbbf22c5357a20db12"
  end

  def install
    bin.install "bb"
  end

  test do
    assert_match "bb", shell_output("#{bin}/bb --help")
  end
end
