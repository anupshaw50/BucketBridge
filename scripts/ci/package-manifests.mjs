#!/usr/bin/env node
// Regenerates the package manager manifests under packaging/ for a release.
// Usage (from the repo root): node scripts/ci/package-manifests.mjs <version> <path to SHA256SUMS>
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const [version, sumsPath] = process.argv.slice(2);
if (!version || !sumsPath || !/^\d+\.\d+\.\d+([-.][0-9A-Za-z.]+)?$/.test(version)) {
  console.error("usage: node scripts/ci/package-manifests.mjs <version> <path to SHA256SUMS>");
  process.exit(2);
}

const repo = "https://github.com/anupshaw50/BucketBridge";
const base = `${repo}/releases/download/v${version}`;
const desc = "Desktop client for S3 and S3-compatible object storage";
const sums = {};
for (const line of readFileSync(sumsPath, "utf8").split("\n")) {
  const m = line.trim().match(/^([0-9a-fA-F]{64})\s+\*?(.+)$/);
  if (m) sums[m[2]] = m[1].toLowerCase();
}
const asset = (name) => {
  if (!sums[name]) {
    console.error(`missing checksum for ${name}`);
    process.exit(1);
  }
  return { name, url: `${base}/${name}`, sha: sums[name] };
};

const files = {};
const put = (path, text) => {
  files[path] = text.endsWith("\n") ? text : text + "\n";
};

const dmg = asset(`BucketBridge_${version}_macos_universal.dmg`);
const bbMac = asset(`bb_${version}_macos_universal.tar.gz`);
const bbLinux = asset(`bb_${version}_linux_x86_64.tar.gz`);
const portable = asset(`BucketBridge_${version}_windows_x64_portable.zip`);
const bbWin = asset(`bb_${version}_windows_x64.zip`);
const msi = asset(`BucketBridge_${version}_windows_x64.msi`);

// Homebrew
put(
  "packaging/homebrew/Casks/bucketbridge.rb",
  `cask "bucketbridge" do
  version "${version}"
  sha256 "${dmg.sha}"

  url "https://github.com/anupshaw50/BucketBridge/releases/download/v#{version}/BucketBridge_#{version}_macos_universal.dmg"
  name "BucketBridge"
  desc "${desc}"
  homepage "${repo}"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "BucketBridge.app"

  zap trash: "~/Library/Application Support/BucketBridge"
end`,
);
put(
  "packaging/homebrew/Formula/bb.rb",
  `class Bb < Formula
  desc "Command line client for S3 and S3-compatible object storage (BucketBridge)"
  homepage "${repo}"
  version "${version}"
  license "GPL-3.0-only"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    url "${bbMac.url.replaceAll(version, "#{version}")}"
    sha256 "${bbMac.sha}"
  end

  on_linux do
    url "${bbLinux.url.replaceAll(version, "#{version}")}"
    sha256 "${bbLinux.sha}"
  end

  def install
    bin.install "bb"
  end

  test do
    assert_match "bb", shell_output("#{bin}/bb --help")
  end
end`,
);

// Scoop
const scoop = (name, description, a, dir, exe, extra) => ({
  version,
  description,
  homepage: repo,
  license: "GPL-3.0-only",
  architecture: { "64bit": { url: a.url, hash: a.sha } },
  extract_dir: dir,
  bin: exe,
  ...extra,
  checkver: "github",
  autoupdate: {
    architecture: {
      "64bit": {
        url: a.url.replaceAll(version, '$version'),
        hash: {
          url: `${repo}/releases/download/v$version/SHA256SUMS`,
          regex: "$sha256\\s+$basename",
        },
      },
    },
    extract_dir: dir.replaceAll(version, '$version'),
  },
});
put(
  "packaging/scoop/bucketbridge.json",
  JSON.stringify(
    scoop("bucketbridge", desc, portable, `BucketBridge_${version}_windows_x64_portable`, "BucketBridge.exe", {
      shortcuts: [["BucketBridge.exe", "BucketBridge"]],
    }),
    null,
    4,
  ),
);
put(
  "packaging/scoop/bb.json",
  JSON.stringify(
    scoop("bb", "Command line client for S3 and S3-compatible object storage", bbWin, `bb_${version}_windows_x64`, "bb.exe", {}),
    null,
    4,
  ),
);

// winget
const long =
  "BucketBridge is an open-source desktop client for S3 and S3-compatible object storage. Browse buckets, move files and folders through a resumable transfer queue, and keep local folders in sync. It sends no telemetry.";
const winget = (id, dir, name, short, installerYaml, tags) => {
  const root = `packaging/winget/manifests/a/AnupShaw/${dir}/${version}/AnupShaw.${dir}`;
  const head = `PackageIdentifier: ${id}\nPackageVersion: ${version}\n`;
  put(`${root}.yaml`, `${head}DefaultLocale: en-US\nManifestType: version\nManifestVersion: 1.6.0`);
  put(`${root}.installer.yaml`, `${head}${installerYaml}ManifestType: installer\nManifestVersion: 1.6.0`);
  put(
    `${root}.locale.en-US.yaml`,
    `${head}PackageLocale: en-US
Publisher: Anup Shaw
PublisherUrl: https://github.com/anupshaw50
PublisherSupportUrl: ${repo}/issues
PackageName: ${name}
PackageUrl: ${repo}
License: GPL-3.0-only
LicenseUrl: ${repo}/blob/main/LICENSE
ShortDescription: ${short}
Description: ${long}
Tags:
${tags.map((t) => `- ${t}`).join("\n")}
ReleaseNotesUrl: ${repo}/releases/tag/v${version}
ManifestType: defaultLocale
ManifestVersion: 1.6.0`,
  );
};
winget(
  "AnupShaw.BucketBridge",
  "BucketBridge",
  "BucketBridge",
  desc,
  `InstallerLocale: en-US
InstallerType: wix
Scope: machine
Installers:
- Architecture: x64
  InstallerUrl: ${msi.url}
  InstallerSha256: ${msi.sha.toUpperCase()}
`,
  ["s3", "object-storage", "file-transfer", "sync", "tauri", "open-source"],
);
winget(
  "AnupShaw.bb",
  "bb",
  "bb",
  "Command line client for S3 and S3-compatible object storage",
  `InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: bb_${version}_windows_x64\\bb.exe
  PortableCommandAlias: bb
Installers:
- Architecture: x64
  InstallerUrl: ${bbWin.url}
  InstallerSha256: ${bbWin.sha.toUpperCase()}
`,
  ["s3", "object-storage", "cli", "sync", "open-source"],
);

for (const [path, text] of Object.entries(files)) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
  console.log(`wrote ${path}`);
}
