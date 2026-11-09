cask "bucketbridge" do
  version "0.3.0"
  sha256 "14e064b159a36c0920f02d35725fba0eccaeaa412aed34cde97d4bd191d2489b"

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
