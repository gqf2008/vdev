//! 摄像头检测：AVFoundation（沙盒内可枚举设备）。
use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeVideo};

pub fn camera_names() -> Vec<String> {
    unsafe {
        let devices = AVCaptureDevice::devicesWithMediaType(AVMediaTypeVideo.unwrap());
        devices.iter().map(|d| d.localizedName().to_string()).collect()
    }
}

pub fn find_vdev() -> bool {
    camera_names()
        .iter()
        .any(|n| n.to_lowercase().contains("vdev-camera"))
}
