# vdev-camera CMIOExtension 构建：Rust 核心 → xcodegen → xcodebuild(自动签名) → 组装 Rust 宿主 App
REPO_ROOT := $(abspath ../..)
BUILD := build/extension
DERIVED := $(BUILD)/DerivedData
TEAM_ID := XFXU84HVK3

.PHONY: all build-rust install-rust test clean

all: install-rust

# ============ Rust + Slint 宿主 App（推荐）============
# 构建：cargo 编 vdev-app → xcodebuild 编扩展 → 手工组装 VDCamera.app → 签名
RUST_APP_BIN := $(REPO_ROOT)/target/release/vdev-app
RUST_EXT := $(DERIVED)/Build/Products/Release/com.vdev.camera.host.extension.systemextension
ASSEMBLE := $(BUILD)/VDCamera.app
SIGN_APP_IDENTITY := "Apple Development: qingfeng gao (7L8FV63FAP)"

build-rust:
	cd $(REPO_ROOT) && cargo build -p vdev-app --release
	cd $(REPO_ROOT) && cargo build -p vdev-camera --release
	xcodegen generate
	xcodebuild -project VDCamera.xcodeproj -scheme VDCameraExtension -configuration Release \
		-derivedDataPath $(DERIVED) \
		ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
		CODE_SIGN_STYLE=Automatic DEVELOPMENT_TEAM=$(TEAM_ID) CODE_SIGN_IDENTITY="Apple Development" \
		-allowProvisioningUpdates -allowProvisioningDeviceRegistration build
	@if [ -d $(ASSEMBLE) ]; then mv $(ASSEMBLE) /tmp/VDCamera.assemble.old.$$(date +%s); fi
	mkdir -p $(ASSEMBLE)/Contents/MacOS $(ASSEMBLE)/Contents/Resources $(ASSEMBLE)/Contents/Library/SystemExtensions
	export COPYFILE_DISABLE=1
	cp $(RUST_APP_BIN) $(ASSEMBLE)/Contents/MacOS/vdev-camera
	cp ../vdev-app/Info.plist $(ASSEMBLE)/Contents/Info.plist
	cp host/Resources/AppIcon.icns $(ASSEMBLE)/Contents/Resources/ 2>/dev/null || true
	cp -R $(RUST_EXT) $(ASSEMBLE)/Contents/Library/SystemExtensions/
	@if [ -f /Applications/VDCamera.app/Contents/embedded.provisionprofile ]; then \
		cp /Applications/VDCamera.app/Contents/embedded.provisionprofile $(ASSEMBLE)/Contents/; \
	fi
	sed 's/\$$(TeamIdentifierPrefix)/$(TEAM_ID)./g' host/vdev-camera.entitlements > $(BUILD)/app.entitlements
	find $(ASSEMBLE) -name '._*' -exec mv {} /tmp/ \;
	codesign --force --sign $(SIGN_APP_IDENTITY) --entitlements $(BUILD)/app.entitlements --options runtime --timestamp=none $(ASSEMBLE)
	codesign --verify --deep --strict $(ASSEMBLE)
	@echo "Rust app assembled & signed: $(ASSEMBLE)"

install-rust: build-rust
	@if [ -d /Applications/VDCamera.app ]; then mv /Applications/VDCamera.app /tmp/VDCamera.app.old.$$(date +%s); fi
	@if [ -d /Applications/VDCameraHost.app ]; then mv /Applications/VDCameraHost.app /tmp/VDCameraHost.app.old.$$(date +%s); fi
	cp -R $(ASSEMBLE) /Applications/
	chmod -R a+rX /Applications/VDCamera.app
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/VDCamera.app
	@echo "installed /Applications/VDCamera.app (Rust + Slint)"

# ============ 一键验证（make test）============
# 视频自测需未沙盒 dev 二进制读任意文件；其余用安装版（含沙盒）
test: build-rust
	@echo "== UI 接线（安装版）=="; /Applications/VDCamera.app/Contents/MacOS/vdev-camera --ui-selftest 2>&1 | grep ui-selftest
	@echo "== 屏幕推流（安装版）=="; /Applications/VDCamera.app/Contents/MacOS/vdev-camera --selftest-screen --dur 3 2>&1 | tail -1
	@echo "== 视频推流（dev 二进制；VDEV_TEST_VIDEO 指定视频）=="; \
		$(REPO_ROOT)/target/release/vdev-app --selftest-video --file "$${VDEV_TEST_VIDEO:-/tmp/nonexistent.mp4}" --dur 3 2>&1 | tail -1
	@echo "== sysext（安装版，须在 /Applications）=="; /Applications/VDCamera.app/Contents/MacOS/vdev-camera --selftest-sysext 2>&1 | tail -1
	@echo "测试完成（视频 0 帧 = 没设 VDEV_TEST_VIDEO 或路径不在沙盒可读范围，UI 走 NSOpenPanel 不受限）"

clean:
	rm -rf $(BUILD) VDCamera.xcodeproj
