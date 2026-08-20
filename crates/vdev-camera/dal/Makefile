# vdev-camera DAL 插件构建：Rust 帧核心 (staticlib) + ObjC++ 薄壳 -> .plugin bundle
SDKROOT    := $(shell xcrun --sdk macosx --show-sdk-path)
REPO_ROOT  := $(abspath ../../..)
DAL_DIR    := $(abspath .)
BUILD      := $(DAL_DIR)/build
PLUGIN     := $(BUILD)/vdev-camera.plugin
RUST_LIB   := $(REPO_ROOT)/target/release/libvdev_camera.a
INSTALL_DIR := $(HOME)/Library/CoreMediaIO/Plug-Ins/DAL

# 显式列出源文件（ExFAT 卷上的 ._* AppleDouble 文件不能混进来）
OBJCXX_SRCS := VDStream.mm VDDevice.mm VDPlugIn.mm PlugInInterface.mm PlugInMain.mm VDObjectStore.mm

.PHONY: all rust plugin install clean

all: plugin

rust:
	cd $(REPO_ROOT) && cargo build -p vdev-camera --release

plugin: rust
	rm -rf $(BUILD)
	mkdir -p $(PLUGIN)/Contents/MacOS
	cp Info.plist $(PLUGIN)/Contents/
	clang++ -std=c++17 -fobjc-arc -fobjc-weak -O2 \
		-I$(DAL_DIR) \
		-isysroot $(SDKROOT) \
		-F$(SDKROOT)/System/Library/Frameworks \
		-framework Foundation -framework CoreMediaIO -framework CoreMedia -framework CoreVideo \
		-bundle -o $(PLUGIN)/Contents/MacOS/vdev-camera \
		$(OBJCXX_SRCS:%= $(DAL_DIR)/%) \
		$(RUST_LIB)
	codesign --force --sign - $(PLUGIN)
	@echo "built: $(PLUGIN)"

install: plugin
	mkdir -p $(INSTALL_DIR)
	rm -rf $(INSTALL_DIR)/vdev-camera.plugin
	cp -R $(PLUGIN) $(INSTALL_DIR)/
	@echo "installed: $(INSTALL_DIR)/vdev-camera.plugin"

# 系统级安装（cameracaptured 以 _cmiodalassistants 用户运行，只能读 /Library；需要 sudo）
install-system: plugin
	sudo mkdir -p /Library/CoreMediaIO/Plug-Ins/DAL
	sudo rm -rf /Library/CoreMediaIO/Plug-Ins/DAL/vdev-camera.plugin
	sudo cp -R $(PLUGIN) /Library/CoreMediaIO/Plug-Ins/DAL/
	sudo killall cameracaptured 2>/dev/null || true
	@echo "installed to /Library/CoreMediaIO/Plug-Ins/DAL/ (cameracaptured restarted)"

clean:
	rm -rf $(BUILD)
