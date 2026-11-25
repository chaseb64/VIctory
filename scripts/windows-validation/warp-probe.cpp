// Probe the OS-provided runtime before Cargo builds either test revision.
#include <windows.h>
#include <d3d12.h>
#include <d3d12sdklayers.h>
#include <dxgi1_4.h>
#include <wrl/client.h>
#include <cstdio>

using Microsoft::WRL::ComPtr;

enum ProbeResult {
    ready = 0,
    device_unavailable = 1,
    debug_unavailable = 2,
    format_unavailable = 3,
};

static bool checked(const char* operation, HRESULT hr) {
    std::printf("%s HRESULT=0x%08lx\n", operation, static_cast<unsigned long>(hr));
    return SUCCEEDED(hr);
}

int main() {
    ComPtr<ID3D12Debug> debug;
    const bool hasDebug = checked("D3D12GetDebugInterface", D3D12GetDebugInterface(IID_PPV_ARGS(&debug)));
    if (hasDebug) debug->EnableDebugLayer();

    ComPtr<IDXGIFactory4> factory;
    ComPtr<IDXGIAdapter1> warp;
    DXGI_ADAPTER_DESC1 adapter{};
    if (!checked("CreateDXGIFactory1", CreateDXGIFactory1(IID_PPV_ARGS(&factory))) ||
        !checked("EnumWarpAdapter", factory->EnumWarpAdapter(IID_PPV_ARGS(&warp))) ||
        !checked("GetDesc1", warp->GetDesc1(&adapter))) return device_unavailable;

    std::printf("adapter=%ls vendor=%04x device=%04x flags=0x%x luid=%08lx:%08lx\n",
                adapter.Description, adapter.VendorId, adapter.DeviceId, adapter.Flags,
                static_cast<unsigned long>(adapter.AdapterLuid.HighPart), adapter.AdapterLuid.LowPart);
    if (!(adapter.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) {
        std::puts("EnumWarpAdapter returned an adapter without DXGI_ADAPTER_FLAG_SOFTWARE");
        return device_unavailable;
    }

    ComPtr<ID3D12Device> device;
    if (!checked("D3D12CreateDevice(FL12_0)", D3D12CreateDevice(warp.Get(), D3D_FEATURE_LEVEL_12_0,
                                                               IID_PPV_ARGS(&device)))) return device_unavailable;
    const LUID actual = device->GetAdapterLuid();
    if (actual.HighPart != adapter.AdapterLuid.HighPart || actual.LowPart != adapter.AdapterLuid.LowPart) {
        std::puts("D3D12 device LUID does not match the enumerated WARP adapter");
        return device_unavailable;
    }

    D3D12_FEATURE_DATA_D3D12_OPTIONS1 waves{};
    if (checked("OPTIONS1", device->CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS1, &waves, sizeof(waves)))) {
        std::printf("wave_ops=%d wave_lanes_min=%u wave_lanes_max=%u\n",
                    waves.WaveOps, waves.WaveLaneCountMin, waves.WaveLaneCountMax);
    }
    D3D12_FEATURE_DATA_FORMAT_SUPPORT format{};
    format.Format = DXGI_FORMAT_R8G8_UNORM;
    if (!checked("FORMAT_SUPPORT(R8G8_UNORM)", device->CheckFeatureSupport(
            D3D12_FEATURE_FORMAT_SUPPORT, &format, sizeof(format)))) return format_unavailable;
    const auto required = D3D12_FORMAT_SUPPORT1_TEXTURE2D | D3D12_FORMAT_SUPPORT1_SHADER_LOAD |
                          D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE | D3D12_FORMAT_SUPPORT1_MIP;
    std::printf("rg8_support1=0x%x rg8_support2=0x%x rg8_required_support1=0x%x\n",
                static_cast<unsigned>(format.Support1), static_cast<unsigned>(format.Support2),
                static_cast<unsigned>(required));
    if ((format.Support1 & required) != required) return format_unavailable;
    std::printf("WARP_SOFTWARE_DEVICE_OK debug=%s\n", hasDebug ? "enabled" : "unavailable");
    return hasDebug ? ready : debug_unavailable;
}
