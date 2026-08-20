//
//  VDRustBridge.h
//  vdev-camera DAL 插件与 Rust 核心之间的 C ABI。
//
#ifndef VDRustBridge_h
#define VDRustBridge_h

#include <stddef.h>

#define VDCAMERA_WIDTH  1280
#define VDCAMERA_HEIGHT 720

// 图案：0 = SMPTE 彩条，1 = 渐变，2 = 棋盘
// 渲染一帧 ARGB32（每像素 4 字节：A,R,G,B），out_len 必须 >= width*height*4。
// 返回 0 成功；-1 图案未知；-2 缓冲区参数非法。
#ifdef __cplusplus
extern "C" {
#endif
int vdev_camera_render_argb32(int pattern, unsigned int width, unsigned int height,
                              double t, unsigned char *out, size_t out_len);
#ifdef __cplusplus
}
#endif

#endif /* VDRustBridge_h */
