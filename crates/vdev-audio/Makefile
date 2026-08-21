# vdev-audio：100% Rust 虚拟声卡（CoreAudio HAL AudioServerPlugIn）
# 一个虚拟设备：输出流 + 输入流，输出环回输入（同 BlackHole/Soundflower）。
# 安装后 QuickTime / 会议软件把「麦克风」选成 vdev-audio 即可。

REPO_ROOT := $(abspath ../..)
BUILD := build
DRIVER := $(BUILD)/vdev-audio.driver
BIN := $(DRIVER)/Contents/MacOS/vdev-audio
DEVID := "Developer ID Application: qingfeng gao (XFXU84HVK3)"

.PHONY: all build install uninstall test clean

all: install

# ============ 构建：cargo cdylib -> MH_BUNDLE + Info.plist + 签名 ============
build:
	cd $(REPO_ROOT) && cargo rustc -p vdev-audio --lib --release -- -C link-arg=-Wl,-bundle -C link-arg=-Wl,-undefined -C link-arg=-Wl,dynamic_lookup
	cd $(REPO_ROOT) && cargo build -p vdev-audio --bin vdev-audio-ctl --release
	@if [ -d $(BUILD) ]; then mv $(BUILD) /tmp/vdev-audio.build.old.$$(date +%s); fi
	mkdir -p $(DRIVER)/Contents/MacOS
	cp $(REPO_ROOT)/target/release/libvdev_audio.dylib $(BIN)
	cp Info.plist $(DRIVER)/Contents/Info.plist
	chmod 755 $(BIN)
	@if security find-identity -v -p codesigning | grep -q 'Developer ID Application'; then \
		codesign --force --sign $(DEVID) --identifier com.vdev.audio.driver $(DRIVER) && echo "signed with Developer ID"; \
	else \
		codesign --force --sign - --identifier com.vdev.audio.driver $(DRIVER) && echo "WARN: 无 Developer ID，adhoc 签名（macOS 26 coreaudiod 可能拒绝加载）"; \
	fi
	@echo "built: $(DRIVER) + target/release/vdev-audio-ctl"

# ============ 安装：/Library/Audio/Plug-Ins/HAL + 重启 coreaudiod ============
install: build
	@rm -rf /tmp/vdev-audio.driver && cp -R $(DRIVER) /tmp/vdev-audio.driver
	osascript -e 'do shell script "rm -rf /Library/Audio/Plug-Ins/HAL/vdev-audio.driver && cp -R /tmp/vdev-audio.driver /Library/Audio/Plug-Ins/HAL/vdev-audio.driver && chown -R root:wheel /Library/Audio/Plug-Ins/HAL/vdev-audio.driver && chmod -R 755 /Library/Audio/Plug-Ins/HAL/vdev-audio.driver && killall coreaudiod" with administrator privileges'
	@echo "installed vdev-audio.driver, coreaudiod restarted"

uninstall:
	osascript -e 'do shell script "rm -rf /Library/Audio/Plug-Ins/HAL/vdev-audio.driver && killall coreaudiod" with administrator privileges'
	@echo "uninstalled vdev-audio.driver"

# ============ 一键验证：440Hz 正弦 -> vdev-audio 输出，再从输入录 3s ============
test:
	./test_loopback.sh

clean:
	rm -rf $(BUILD)
