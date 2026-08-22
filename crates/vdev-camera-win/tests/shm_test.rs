//! 共享帧通道往返测试。

use vdev_camera_win::SharedFrameChannel;

#[test]
fn shm_publish_latest_roundtrip() {
    let writer = SharedFrameChannel::open_or_create(true).expect("open writer");
    let reader = SharedFrameChannel::open_or_create(false).expect("open reader");

    // 从未发布 → None
    let mut out = Vec::new();
    assert!(reader.latest(&mut out).is_none());

    let w = 640u32;
    let h = 480u32;
    let mut frame = vec![0u8; (w * h * 4) as usize];
    for (i, b) in frame.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    writer.publish(w, h, &frame).expect("publish");

    let dims = reader.latest(&mut out).expect("frame available");
    assert_eq!(dims, (w, h));
    assert_eq!(out, frame);
}

#[test]
fn shm_rejects_wrong_size() {
    let writer = SharedFrameChannel::open_or_create(true).expect("open writer");
    let err = writer.publish(640, 480, &[0u8; 100]).unwrap_err();
    assert!(err.to_string().contains("frame size"));
}

#[test]
fn shm_rejects_writer_mode_on_reader() {
    let reader = SharedFrameChannel::open_or_create(false).expect("open reader");
    let err = reader.publish(10, 10, &vec![0u8; 400]).unwrap_err();
    assert!(err.to_string().contains("writer"));
}
