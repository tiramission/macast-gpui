//! The shared D3D11 device Media Foundation's decoders use for GPU (DXVA) decoding, and which
//! codec profiles that GPU can decode.

use std::collections::HashSet;
use std::sync::OnceLock;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11Multithread, ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};
use windows::core::{GUID, Interface};

use crate::hw::select::HwCodec;

/// The device, its Media Foundation device manager, and the GPU's decoder profiles.
pub struct Gpu {
    pub manager: IMFDXGIDeviceManager,
    profiles: HashSet<u128>,
    _device: ID3D11Device,
}

// SAFETY: the device is created with multithread protection (`ID3D11Multithread`), and the
// DXGI device manager is free-threaded by design (it hands out locked device access); both are
// documented as usable from any thread.
unsafe impl Send for Gpu {}
// SAFETY: as above.
unsafe impl Sync for Gpu {}

/// The process-wide GPU device, or `None` without a D3D11 hardware device with video support.
pub fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| create().map_err(|e| log::info!("no D3D11 video device: {e}")).ok()).as_ref()
}

fn create() -> windows::core::Result<Gpu> {
    let mut device = None;
    // Local patch (vs crates.io 0.6.0): `D3D11CreateDevice(None, HARDWARE)`
    // picks the default adapter, which on Optimus laptops is the integrated
    // GPU - the very GPU the UI renderer uses, so DXVA decoding and gpui's
    // texture uploads fight over the same execution units (UI drops to
    // ~20 fps). Prefer a discrete NVIDIA adapter when one is present and
    // fall back to the default adapter otherwise.
    let adapter = pick_discrete_adapter();
    // SAFETY: FFI with valid out-pointers; the result is checked.
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            if adapter.is_some() {
                D3D_DRIVER_TYPE_UNKNOWN
            } else {
                D3D_DRIVER_TYPE_HARDWARE
            },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    let device: ID3D11Device = device.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
    // SAFETY: COM calls on a live device.
    unsafe {
        let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
        let video: ID3D11VideoDevice = device.cast()?;
        let mut profiles = HashSet::new();
        for i in 0..video.GetVideoDecoderProfileCount() {
            if let Ok(g) = video.GetVideoDecoderProfile(i) {
                profiles.insert(g.to_u128());
            }
        }
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
        manager.ResetDevice(&device, token)?;
        Ok(Gpu { manager, profiles, _device: device })
    }
}

/// Picks a discrete NVIDIA adapter when the machine has one (Optimus laptops
/// otherwise hand the default adapter to the integrated GPU). `None` when only
/// an integrated/software adapter exists, so the caller keeps the default.
fn pick_discrete_adapter() -> Option<IDXGIAdapter> {
    // SAFETY: COM factory creation and enumeration with checked results.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut first_hardware = None;
        for i in 0.. {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(i) {
                Ok(a) => a,
                Err(_) => break,
            };
            let desc = match adapter.GetDesc1() {
                Ok(d) => d,
                Err(_) => continue,
            };
            if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                continue; // WARP & friends
            }
            let base: IDXGIAdapter = match adapter.cast() {
                Ok(a) => a,
                Err(_) => continue,
            };
            if desc.VendorId == 0x10DE {
                log::info!(
                    "MF video device: discrete NVIDIA adapter ({})",
                    String::from_utf16_lossy(&desc.Description)
                );
                return Some(base); // NVIDIA: discrete or the only dGPU
            }
            if first_hardware.is_none() {
                first_hardware = Some(base);
            }
        }
        if let Some(a) = first_hardware.as_ref() {
            if let Ok(d) = a.GetDesc() {
                log::info!(
                    "MF video device: fallback adapter ({})",
                    String::from_utf16_lossy(&d.Description)
                );
            }
        } else {
            log::info!("MF video device: no hardware adapter found, using default");
        }
        first_hardware
    }
}

impl Gpu {
    /// Whether the GPU decodes `codec` (its main DXVA profile).
    pub fn decodes(&self, codec: HwCodec) -> bool {
        profiles(codec).iter().any(|p| self.profiles.contains(&p.to_u128()))
    }
}

/// DXVA decoder profile GUIDs per codec (any one suffices).
fn profiles(codec: HwCodec) -> &'static [GUID] {
    use windows::Win32::Graphics::Direct3D11::*;
    match codec {
        HwCodec::H264 => &[D3D11_DECODER_PROFILE_H264_VLD_NOFGT, D3D11_DECODER_PROFILE_H264_VLD_FGT],
        HwCodec::Hevc => &[D3D11_DECODER_PROFILE_HEVC_VLD_MAIN, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10],
        HwCodec::Vp9 => &[D3D11_DECODER_PROFILE_VP9_VLD_PROFILE0, D3D11_DECODER_PROFILE_VP9_VLD_10BIT_PROFILE2],
        HwCodec::Av1 => &[D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0],
        _ => &[],
    }
}
