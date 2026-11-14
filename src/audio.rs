//! 再生デバイス（レンダーエンドポイント）の列挙と、既定デバイスの取得・設定。

use std::ffi::c_void;

use anyhow::{Context, Result};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, ERole, IMMDeviceEnumerator, MMDeviceEnumerator, eCommunications,
    eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR};

use crate::hid::{PID, VID};

/// エンドポイントを提供している USB インターフェイスのインスタンス ID を持つ非公開のプロパティ。
/// エンドポイント自身のインスタンス ID（`SWD\MMDEVAPI\...`）には VID/PID が入らないため、これを使う。
/// 実機で `{1}.USB\VID_054C&PID_0EC2&MI_03\...` の形で取れることを確認済み。
const PKEY_ENDPOINT_PARENT_INSTANCE_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xb3f8fa53_0004_438e_9003_51a46e139bfc),
    pid: 2,
};

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// ドングルの再生デバイスの種類。表示名は言語で変わるため、インターフェイス番号で見分ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DongleKind {
    /// `MI_03`。「INZONE Buds - Game」
    Game,
    /// `MI_00`。「INZONE Buds - Chat」
    Chat,
    Other,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    /// ドングル以外は `None`
    pub kind: Option<DongleKind>,
}

pub fn dongle_kind(instance_id: &str) -> Option<DongleKind> {
    let id = instance_id.to_ascii_uppercase();
    if !id.contains(&format!("VID_{VID:04X}&PID_{PID:04X}")) {
        return None;
    }
    Some(if id.contains("&MI_03") {
        DongleKind::Game
    } else if id.contains("&MI_00") {
        DongleKind::Chat
    } else {
        DongleKind::Other
    })
}

/// スレッドの COM 初期化（STA）。`Audio` より長く生かすこと。
pub struct ComGuard;

impl ComGuard {
    pub fn new() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

// Windows 10 以降の IPolicyConfig。SetDefaultEndpoint 以外は呼ばないが、
// vtable の位置を合わせるために前のスロットを順番どおりに置く必要がある。
#[windows_core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn get_mix_format(&self) -> HRESULT;
    fn get_device_format(&self) -> HRESULT;
    fn reset_device_format(&self) -> HRESULT;
    fn set_device_format(&self) -> HRESULT;
    fn get_processing_period(&self) -> HRESULT;
    fn set_processing_period(&self) -> HRESULT;
    fn get_share_mode(&self) -> HRESULT;
    fn set_share_mode(&self) -> HRESULT;
    fn get_property_value(&self) -> HRESULT;
    fn set_property_value(&self) -> HRESULT;
    fn set_default_endpoint(&self, device_id: PCWSTR, role: ERole) -> HRESULT;
    fn set_endpoint_visibility(&self, device_id: PCWSTR, visible: i32) -> HRESULT;
}

/// `App` が使う再生デバイスの操作。シナリオテストでメモリ上の偽物に差し替えるために切り出している
pub trait Devices {
    /// 有効な再生デバイスの一覧
    fn list(&self) -> Result<Vec<Endpoint>>;
    /// 既定の再生デバイス（eConsole）のエンドポイント ID。なければ `None`
    fn default_id(&self) -> Result<Option<String>>;
    /// 既定の通信デバイス（eCommunications）のエンドポイント ID。なければ `None`
    fn communications_id(&self) -> Result<Option<String>>;
    /// eConsole と eMultimedia を設定する
    fn set_default(&self, id: &str) -> Result<()>;
    /// eCommunications を設定する
    fn set_communications(&self, id: &str) -> Result<()>;
}

/// Core Audio による実装
pub struct Audio {
    enumerator: IMMDeviceEnumerator,
}

impl Audio {
    pub fn new() -> Result<Self> {
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .context("IMMDeviceEnumerator の作成に失敗")?;
        Ok(Self { enumerator })
    }
}

impl Devices for Audio {
    fn list(&self) -> Result<Vec<Endpoint>> {
        unsafe {
            let devices = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            (0..devices.GetCount()?)
                .map(|i| {
                    let dev = devices.Item(i)?;
                    let id = dev.GetId()?;
                    let id_str = id.to_string()?;
                    CoTaskMemFree(Some(id.0 as *const c_void));
                    let props = dev.OpenPropertyStore(STGM_READ)?;
                    Ok(Endpoint {
                        id: id_str,
                        name: prop_string(&props, &PKEY_Device_FriendlyName)?.unwrap_or_default(),
                        kind: prop_string(&props, &PKEY_ENDPOINT_PARENT_INSTANCE_ID)?
                            .and_then(|s| dongle_kind(&s)),
                    })
                })
                .collect()
        }
    }

    fn default_id(&self) -> Result<Option<String>> {
        self.default_for(eConsole)
    }

    fn communications_id(&self) -> Result<Option<String>> {
        self.default_for(eCommunications)
    }

    fn set_default(&self, id: &str) -> Result<()> {
        self.set_roles(id, &[eConsole, eMultimedia])
    }

    fn set_communications(&self, id: &str) -> Result<()> {
        self.set_roles(id, &[eCommunications])
    }
}

impl Audio {
    fn default_for(&self, role: ERole) -> Result<Option<String>> {
        unsafe {
            let Ok(dev) = self.enumerator.GetDefaultAudioEndpoint(eRender, role) else {
                return Ok(None);
            };
            let id = dev.GetId()?;
            let s = id.to_string()?;
            CoTaskMemFree(Some(id.0 as *const c_void));
            Ok(Some(s))
        }
    }

    fn set_roles(&self, id: &str, roles: &[ERole]) -> Result<()> {
        let policy: IPolicyConfig =
            unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) }
                .context("IPolicyConfig の作成に失敗")?;
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        for &role in roles {
            unsafe { policy.set_default_endpoint(PCWSTR(wide.as_ptr()), role).ok()? };
        }
        Ok(())
    }
}

fn prop_string(props: &IPropertyStore, key: &PROPERTYKEY) -> Result<Option<String>> {
    unsafe {
        let value = props.GetValue(key)?;
        // 値がない場合は VT_EMPTY で、文字列への変換が失敗する
        let Ok(s) = PropVariantToStringAlloc(&value) else {
            return Ok(None);
        };
        let out = s.to_string()?;
        CoTaskMemFree(Some(s.0 as *const c_void));
        Ok(Some(out).filter(|s| !s.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dongle_kind_by_interface() {
        let game = r"{1}.USB\VID_054C&PID_0EC2&MI_03\7&1B2450B8&0&0003";
        let chat = r"{1}.usb\vid_054c&pid_0ec2&mi_00\7&1b2450b8&0&0000";
        assert_eq!(dongle_kind(game), Some(DongleKind::Game));
        assert_eq!(dongle_kind(chat), Some(DongleKind::Chat));
        assert_eq!(dongle_kind(r"{1}.USB\VID_054C&PID_0EC2&MI_05\x"), Some(DongleKind::Other));
    }

    #[test]
    fn other_devices_are_not_dongle() {
        assert_eq!(dongle_kind(r"{1}.TUSBAUDIO_ENUM\VID_2708&PID_0006&KS\8&10F9DF5D&2&3"), None);
        assert_eq!(dongle_kind(r"{1}.USB\VID_054C&PID_0CE6&MI_03\x"), None);
    }
}
