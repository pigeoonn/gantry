# After a release: replace the version and the SHA-256 (from the .sha256 file next to the release's macOS archive),
# then put this file in a tap (homebrew-gantry/Casks/gantry.rb) or submit it to homebrew-cask.
cask "gantry" do
  version "5.3.0"
  sha256 "b9292e96447267a5625f63c1cb7b9a4e7673572cf5f48a3a5882a17ccd090077"

  url "https://github.com/pigeoonn/gantry/releases/download/v#{version}/gantry-v#{version}-macos-arm64.tar.gz"
  name "Gantry"
  desc "Mod, trainer and crash-hunting hub for games"
  homepage "https://github.com/pigeoonn/gantry"

  depends_on arch: :arm64

  binary "gantry-v#{version}-macos-arm64/rig"
end
