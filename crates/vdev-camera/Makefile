# vdev-camera CMIOExtension 构建：Rust 核心 → xcodegen → xcodebuild(不签名) → 手动 Developer ID 签名
REPO_ROOT := $(abspath ../..)
BUILD := build/extension
DERIVED := $(BUILD)/DerivedData
PRODUCTS := $(DERIVED)/Build/Products/Release
APP := $(PRODUCTS)/VDCamera.app
EXT := $(PRODUCTS)/VDCameraExtension.systemextension
SIGN_IDENTITY := "Developer ID Application: qingfeng gao (XFXU84HVK3)"
TEAM_ID := XFXU84HVK3

.PHONY: all rust project build sign verify install run clean

all: verify

rust:
	cd $(REPO_ROOT) && cargo build -p vdev-camera --release

project: rust
	xcodegen generate

build: project
	xcodebuild -project VDCamera.xcodeproj -scheme vdev-camera -configuration Release \
		-derivedDataPath $(DERIVED) \
		CODE_SIGNING_ALLOWED=NO ARCHS=arm64 ONLY_ACTIVE_ARCH=YES build

sign: build
	mkdir -p $(BUILD)
	sed 's/\$$(TeamIdentifierPrefix)/$(TEAM_ID)./g' extension/VDCameraExtension.entitlements > $(BUILD)/ext.entitlements
	codesign --force --sign $(SIGN_IDENTITY) --entitlements $(BUILD)/ext.entitlements $(EXT)
	codesign --force --sign $(SIGN_IDENTITY) --entitlements host/vdev-camera.entitlements $(APP)

verify: sign
	codesign --verify --deep --strict $(APP)
	codesign --verify --strict $(EXT)
	@echo "OK: signed & verified"

install: verify
	cp -R $(APP) /Applications/
	open /Applications/VDCamera.app

run: verify
	open $(APP)

clean:
	rm -rf $(BUILD) VDCamera.xcodeproj

# 自动签名（需要：Xcode 已登录 Apple 开发者账号，团队有 System Extension capability）
# 用法：make build-autosign
build-autosign: rust
	xcodegen generate
	xcodebuild -project VDCamera.xcodeproj -scheme vdev-camera -configuration Release \
		-derivedDataPath $(BUILD)/DerivedData \
		ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
		CODE_SIGN_STYLE=Automatic DEVELOPMENT_TEAM=$(TEAM_ID) CODE_SIGN_IDENTITY="Apple Development" \
		-allowProvisioningUpdates -allowProvisioningDeviceRegistration build
	@if [ -d /Applications/VDCamera.app ]; then mv /Applications/VDCamera.app /tmp/VDCamera.app.old.$$(date +%s); fi
	@if [ -d /Applications/vdev-camera.app ]; then mv /Applications/vdev-camera.app /tmp/vdev-camera.app.old.$$(date +%s); fi
	@if [ -d /Applications/VDCameraHost.app ]; then mv /Applications/VDCameraHost.app /tmp/VDCameraHost.app.old.$$(date +%s); fi
	cp -R $(APP) /Applications/
	chmod -R a+rX /Applications/VDCamera.app
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/VDCamera.app
	@echo "autosigned & installed"
