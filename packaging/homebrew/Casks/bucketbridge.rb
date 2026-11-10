cask "bucketbridge" do
  version "0.4.0"
  sha256 "28dd004f2c051694bda4386b2a1fbb268956e9a2d43c9751a4be43e55dfc60ef"

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
