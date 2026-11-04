cask "bucketbridge" do
  version "0.2.0"
  sha256 "9b7baee49749963d01db37a5bb9aa6d81452260762c287e0930067743b3eb069"

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
