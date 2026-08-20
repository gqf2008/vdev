#ifndef VDCameraRustBridge_h
#define VDCameraRustBridge_h

#include <stddef.h>

// 图案：0 = SMPTE 彩条，1 = 渐变，2 = 棋盘
// 渲染一帧 BGRA32（每像素 4 字节：B,G,R,A），out_len 必须 >= width*height*4。
// 返回 0 成功；-1 图案未知；-2 缓冲区参数非法。
#ifdef __cplusplus
extern "C" {
#endif
int vdev_camera_render_bgra32(int pattern, unsigned int width, unsigned int height,
                              double t, unsigned char *out, size_t out_len);
#ifdef __cplusplus
}
#endif

#endif /* VDCameraRustBridge_h */
