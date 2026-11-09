class Bb < Formula
  desc "Command line client for S3 and S3-compatible object storage (BucketBridge)"
  homepage "https://github.com/anupshaw50/BucketBridge"
  version "0.3.1"
  license "GPL-3.0-only"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_macos_universal.tar.gz"
    sha256 "ca89dd8e98e1d2d363b8efe79fd27c8336fd88274894610bbe2099dc928578cf"
  end

  on_linux do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_linux_x86_64.tar.gz"
    sha256 "6172e8d477c88f8fd32cecd622c0a5b332c4bf6ffff2239c53294510958fcd8d"
  end

  def install
    bin.install "bb"
  end

  test do
    assert_match "bb", shell_output("#{bin}/bb --help")
  end
end
