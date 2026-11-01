class Bb < Formula
  desc "Command line client for S3 and S3-compatible object storage (BucketBridge)"
  homepage "https://github.com/anupshaw50/BucketBridge"
  version "0.1.0"
  license "GPL-3.0-only"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_macos_universal.tar.gz"
    sha256 "0380d5c53d990f4c678daeabe7a1e40a22aad07d95284ad5f371ef06453918aa"
  end

  on_linux do
    url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/bb_#{version}_linux_x86_64.tar.gz"
    sha256 "261be2b6a9d6c405a9cb7513443d662ceadbc03ea6c05f9fa71a78b22752a011"
  end

  def install
    bin.install "bb"
  end

  test do
    assert_match "bb", shell_output("#{bin}/bb --help")
  end
end
