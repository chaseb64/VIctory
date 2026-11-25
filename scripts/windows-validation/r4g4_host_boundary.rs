// Included alongside the resource implementation in both disposable checkouts.
#[cfg(all(test, windows, feature = "diagnostics"))]
mod windows_validation_r4g4 {
    use super::*;
    use crate::object::slot;
    use windows::Win32::Graphics::{Direct3D12::ID3D12InfoQueue, Dxgi::*};
    use windows::core::Interface;

    #[test]
    #[ignore = "requires WARP and the D3D12 debug layer"]
    fn shared_r4g4_host_boundary() {
        const RG8: u32 = 49;
        const R4G4: u32 = 190;
        assert!(crate::switch::on("VICTORY_D3D12_WARP"));
        assert!(crate::switch::on("VICTORY_D3D12_DEBUG"));
        let host = crate::host::host().expect("D3D12 device required");
        let _debug = host
            .device
            .cast::<ID3D12InfoQueue>()
            .expect("D3D12 debug info queue required");
        // SAFETY: each DXGI object and the D3D12 device are live.
        let (warp, device, adapter) = unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory1().unwrap();
            let warp: IDXGIAdapter1 = factory.EnumWarpAdapter().unwrap();
            let device = host.device.GetAdapterLuid();
            let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(device).unwrap();
            (warp.GetDesc1().unwrap(), device, adapter.GetDesc1().unwrap())
        };
        assert_eq!(
            (device.HighPart, device.LowPart),
            (warp.AdapterLuid.HighPart, warp.AdapterLuid.LowPart),
            "D3D12 device must use the enumerated WARP adapter"
        );
        assert_ne!(adapter.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32, 0);
        let startup = host.log_messages();
        assert!(startup.is_none(), "D3D12 startup validation error: {startup:?}");
        eprintln!(
            "VICTORY_ORACLE_WARP debug=active vendor={:04x} device={:04x} luid={:08x}:{:08x} name={}",
            adapter.VendorId,
            adapter.DeviceId,
            device.HighPart,
            device.LowPart,
            String::from_utf16_lossy(&adapter.Description).trim_end_matches('\0'),
        );

        let create = |format| {
            let null = std::ptr::null();
            let heap = HeapProperties {
                kind: 1,
                cpu_pages: 0,
                pool: 0,
                creation_nodes: 0,
                visible_nodes: 0,
            };
            let desc = ResourceDesc {
                dimension: 3,
                alignment: 0,
                width: 129,
                height: 3,
                depth_or_array_size: 2,
                mip_levels: 2,
                format,
                sample_count: 1,
                sample_quality: 0,
                layout: 0,
                flags: 0,
            };
            let mut out = null;
            let hr = create_committed(null, &heap, 0, &desc, 0, null.cast(), &RESOURCE_IIDS[0], &mut out);
            assert_eq!(hr, S_OK, "guest texture creation for format {format}");
            assert!(!out.is_null(), "guest texture pointer for format {format}");
            assert_eq!(resource(out).desc.format, format);
            // SAFETY: read the live resource's host description before release.
            let observed = host_resource(out).map(|texture| unsafe { texture.GetDesc().Format.0 });
            slot::<extern "system" fn(This) -> u32>(out, 0x10)(out);
            observed
        };

        let native = create(RG8);
        assert_eq!(native, Some(RG8 as i32), "ordinary RG8 host control");
        assert!(host.log_messages().is_none(), "RG8 control validation error");
        eprintln!("VICTORY_ORACLE_RG8_CONTROL guest=49 host=49");

        let observed = create(R4G4);
        let debug_error = host.log_messages();
        // SAFETY: the D3D12 device is live.
        unsafe { host.device.GetDeviceRemovedReason().unwrap() };
        match observed {
            None => eprintln!("VICTORY_ORACLE_R4G4 guest=190 host=absent debug_error={debug_error:?}"),
            Some(format) => eprintln!("VICTORY_ORACLE_R4G4 guest=190 host={format} debug_error={debug_error:?}"),
        }
        assert_eq!(observed, Some(RG8 as i32), "R4G4 must have an RG8 host image");
        assert!(debug_error.is_none(), "R4G4 host texture validation error");
    }
}
