cask "bucketbridge" do
  version "0.1.0"
  sha256 "4667d13f174fb1ab9393acb4d366f516f40b75706359941d19c900a6b9f9e2e2"

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
