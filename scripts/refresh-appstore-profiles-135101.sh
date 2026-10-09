#!/bin/bash
set -euo pipefail
cd "$HOME/Herald"
mkdir -p "$HOME/Hermes-iOS-Builds/testflight-135.101"
exec > >(tee -a "$HOME/Hermes-iOS-Builds/testflight-135.101/provision-refresh.log") 2>&1
echo "$(date) provisioning refresh start"
xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Release -destination generic/platform=iOS -allowProvisioningUpdates DEVELOPMENT_TEAM=58U7UPFS53 CODE_SIGN_STYLE=Automatic build
echo "$(date) provisioning refresh succeeded"
