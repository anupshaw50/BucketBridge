cask "bucketbridge" do
  version "0.3.1"
  sha256 "7f0faff91da00da83251e6ddd397e8ff286c33ed96c2ca35efa119b3c9125fa0"

  url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/BucketBridge_#{version}_macos_universal.dmg"
  name "BucketBridge"
  desc "Desktop client for S3 and S3-compatible object storage"
  homepage "https://github.com/anupshaw50/BucketBridge"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "BucketBridge.app"

  zap trash: "~/Library/Application Support/BucketBridge"
end
