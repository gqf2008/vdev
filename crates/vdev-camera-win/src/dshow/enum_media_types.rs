//! `IEnumMediaTypes` 实现：枚举输出 Pin 支持的媒体类型。

use std::sync::Mutex;

use windows::Win32::Foundation::{E_POINTER, S_FALSE, S_OK};
use windows::Win32::Media::DirectShow::{IEnumMediaTypes, IEnumMediaTypes_Impl};
use windows::Win32::Media::MediaFoundation::AM_MEDIA_TYPE;
use windows_core::{implement, HRESULT};

use super::media_type::{self, VideoFormat};

#[implement(IEnumMediaTypes)]
pub struct MediaTypeEnum {
    formats: Vec<VideoFormat>,
    index: Mutex<u32>,
}

impl MediaTypeEnum {
    pub fn new(formats: Vec<VideoFormat>) -> Self {
        Self {
            formats,
            index: Mutex::new(0),
        }
    }
}

impl IEnumMediaTypes_Impl for MediaTypeEnum_Impl {
    fn Next(
        &self,
        cmediatypes: u32,
        ppmediatypes: *mut *mut AM_MEDIA_TYPE,
        pcfetched: *mut u32,
    ) -> HRESULT {
        if ppmediatypes.is_null() {
            return E_POINTER;
        }
        let mut idx = self.index.lock().unwrap();
        let mut fetched = 0u32;
        // SAFETY: ppmediatypes 指向至少 cmediatypes 个槽位；返回的 AM_MEDIA_TYPE
        // 均为 CoTaskMem 分配，调用方用 DeleteMediaType 释放。
        unsafe {
            for i in 0..cmediatypes as usize {
                match self.formats.get(*idx as usize) {
                    Some(f) => {
                        let mt = f.to_media_type();
                        ppmediatypes
                            .add(i)
                            .write(media_type::alloc_media_type_copy(&mt));
                        *idx += 1;
                        fetched += 1;
                    }
                    None => {
                        ppmediatypes.add(i).write(std::ptr::null_mut());
                    }
                }
            }
        }
        if !pcfetched.is_null() {
            // SAFETY: pcfetched 由调用方分配。
            unsafe { *pcfetched = fetched };
        }
        if fetched == cmediatypes {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, cmediatypes: u32) -> windows_core::Result<()> {
        *self.index.lock().unwrap() += cmediatypes;
        Ok(())
    }

    fn Reset(&self) -> windows_core::Result<()> {
        *self.index.lock().unwrap() = 0;
        Ok(())
    }

    fn Clone(&self) -> windows_core::Result<IEnumMediaTypes> {
        let idx = *self.index.lock().unwrap();
        let clone = MediaTypeEnum::new(self.formats.clone());
        *clone.index.lock().unwrap() = idx;
        Ok(clone.into())
    }
}
