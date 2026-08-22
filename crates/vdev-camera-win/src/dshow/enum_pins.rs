//! `IEnumPins` 实现：枚举过滤器的唯一输出 Pin。

use std::sync::Mutex;

use windows::Win32::Foundation::{E_POINTER, S_FALSE, S_OK};
use windows::Win32::Media::DirectShow::{IEnumPins, IEnumPins_Impl, IPin};
use windows_core::{implement, HRESULT};

#[implement(IEnumPins)]
pub struct PinEnum {
    pin: IPin,
    index: Mutex<u32>,
}

impl PinEnum {
    pub fn new(pin: IPin) -> Self {
        Self {
            pin,
            index: Mutex::new(0),
        }
    }
}

impl IEnumPins_Impl for PinEnum_Impl {
    fn Next(&self, cpins: u32, pppins: *mut Option<IPin>, pcfetched: *mut u32) -> HRESULT {
        if pppins.is_null() {
            return E_POINTER;
        }
        let mut idx = self.index.lock().unwrap();
        let mut fetched = 0u32;
        // SAFETY: pppins 指向至少 cpins 个槽位（调用方保证）。
        unsafe {
            for i in 0..cpins as usize {
                if *idx == 0 {
                    pppins.add(i).write(Some(self.pin.clone()));
                    *idx += 1;
                    fetched += 1;
                } else {
                    pppins.add(i).write(None);
                }
            }
        }
        if !pcfetched.is_null() {
            // SAFETY: pcfetched 由调用方分配。
            unsafe { *pcfetched = fetched };
        }
        if fetched == cpins {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, cpins: u32) -> windows_core::Result<()> {
        *self.index.lock().unwrap() += cpins;
        Ok(())
    }

    fn Reset(&self) -> windows_core::Result<()> {
        *self.index.lock().unwrap() = 0;
        Ok(())
    }

    fn Clone(&self) -> windows_core::Result<IEnumPins> {
        let idx = *self.index.lock().unwrap();
        let clone = PinEnum::new(self.pin.clone());
        *clone.index.lock().unwrap() = idx;
        Ok(clone.into())
    }
}
