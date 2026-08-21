# vdev-camera：100% Rust 构建（宿主 vdev-app + CMIOExtension 扩展 vdev-camera-ext）
REPO_ROOT := $(abspath ../..)
BUILD := build/extension
DERIVED := $(BUILD)/DerivedData
TEAM_ID := XFXU84HVK3

.PHONY: all build-rust install-rust test clean

all: install-rust

# ============ 全 Rust 构建（推荐）：宿主 + 扩展都是 Rust，无 xcodebuild ============
# 扩展：crates/vdev-camera-ext（手写 CMIOExtension objc2 绑定 + FrameChannel + 帧管线）
RUST_APP_BIN := $(REPO_ROOT)/target/release/vdev-app
RUST_EXT_BIN := $(REPO_ROOT)/target/release/vdev-camera-ext
ASSEMBLE := $(BUILD)/VDCamera.app
EXT_IN_APP := $(ASSEMBLE)/Contents/Library/SystemExtensions/com.vdev.camera.host.extension.systemextension
SIGN_APP_IDENTITY := "Apple Development: qingfeng gao (7L8FV63FAP)"
SPIKE_ROOT := ../vdev-camera-ext

build-rust:
	cd $(REPO_ROOT) && cargo build -p vdev-app --release
	cd $(REPO_ROOT) && cargo build -p vdev-camera --release
	cd $(REPO_ROOT) && cargo build -p vdev-camera-ext --bin vdev-camera-ext --release
	@if [ -d $(ASSEMBLE) ]; then mv $(ASSEMBLE) /tmp/VDCamera.assemble.old.$$(date +%s); fi
	mkdir -p $(ASSEMBLE)/Contents/MacOS $(ASSEMBLE)/Contents/Resources $(ASSEMBLE)/Contents/Library/SystemExtensions
	export COPYFILE_DISABLE=1
	cp $(RUST_APP_BIN) $(ASSEMBLE)/Contents/MacOS/vdev-camera
	cp ../vdev-app/Info.plist $(ASSEMBLE)/Contents/Info.plist
	cp host/Resources/AppIcon.icns $(ASSEMBLE)/Contents/Resources/ 2>/dev/null || true
	# Rust 系统扩展
	mkdir -p $(EXT_IN_APP)/Contents/MacOS
	cp $(RUST_EXT_BIN) $(EXT_IN_APP)/Contents/MacOS/vdev-camera-ext
	cp $(SPIKE_ROOT)/spike/ext-Info.plist $(EXT_IN_APP)/Contents/Info.plist
	@if [ -f /Applications/VDCamera.app/Contents/embedded.provisionprofile ]; then \
		cp /Applications/VDCamera.app/Contents/embedded.provisionprofile $(ASSEMBLE)/Contents/; \
	elif [ -f $(ASSEMBLE)/Contents/embedded.provisionprofile ]; then \
		: ; \
	else \
		echo "WARN: 无 embedded.provisionprofile（沙盒 App 会被 AMFI 拒启），从 /tmp/VDCamera.app.old.* 找"; \
		for d in /tmp/VDCamera.app.old.*; do \
			if [ -f "$$d/Contents/embedded.provisionprofile" ]; then \
				cp "$$d/Contents/embedded.provisionprofile" $(ASSEMBLE)/Contents/; \
				echo "restored profile from $$d"; \
				break; \
			fi; \
		done; \
	fi
	find $(ASSEMBLE) -name '._*' -exec mv {} /tmp/ \;
	sed 's/\$$(TeamIdentifierPrefix)/$(TEAM_ID)./g' host/vdev-camera.entitlements > $(BUILD)/app.entitlements
	codesign --force --sign $(SIGN_APP_IDENTITY) --entitlements $(SPIKE_ROOT)/spike/ext.entitlements --options runtime --timestamp=none $(EXT_IN_APP)
	codesign --verify --strict $(EXT_IN_APP)
	codesign --force --sign $(SIGN_APP_IDENTITY) --entitlements $(BUILD)/app.entitlements --options runtime --timestamp=none $(ASSEMBLE)
	codesign --verify --deep --strict $(ASSEMBLE)
	@echo "Rust app assembled & signed: $(ASSEMBLE)（宿主+扩展 100% Rust）"

install-rust: build-rust
	@if [ -d /Applications/VDCamera.app ]; then mv /Applications/VDCamera.app /tmp/VDCamera.app.old.$$(date +%s); fi
	@if [ -d /Applications/VDCameraHost.app ]; then mv /Applications/VDCameraHost.app /tmp/VDCameraHost.app.old.$$(date +%s); fi
	cp -R $(ASSEMBLE) /Applications/
	chmod -R a+rX /Applications/VDCamera.app
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/VDCamera.app
	@echo "installed /Applications/VDCamera.app (Rust + Slint + Rust 扩展)"

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

