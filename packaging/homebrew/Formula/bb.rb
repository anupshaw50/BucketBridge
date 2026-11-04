class Bb < Formula
  desc "Command line client for S3 and S3-compatible object storage (BucketBridge)"
  homepage "https://github.com/anupshaw50/BucketBridge"
  version "0.2.0"
  license "GPL-3.0-only"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_macos_universal.tar.gz"
    sha256 "89eea8451cc093ba17f6eb0d539d4bc7945ea3eb32ab6ba45f4d0ac540a4f7e2"
  end

  on_linux do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_linux_x86_64.tar.gz"
    sha256 "87b24ff29f2ef136fc2511c00b2af4605f601e7a12077eade80034710b6a22bf"
  end

  def install
    bin.install "bb"
  end

  test do
    assert_match "bb", shell_output("#{bin}/bb --help")
  end
end
