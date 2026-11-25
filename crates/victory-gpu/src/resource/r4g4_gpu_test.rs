// Run explicitly on Windows. A missing device must fail this acceptance test.
#[test]
#[ignore = "requires a Windows D3D12 adapter with feature level 12_0"]
fn sampled_r4g4_gpu_upload_copy_and_sample() {
    use crate::list::CopyLocation;
    use windows::Win32::Graphics::{Direct3D12::*, Dxgi::*};

    let host = crate::host::host().expect("D3D12 device required");
    // SAFETY: query the adapter of this device, including its software flag.
    let adapter = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1().unwrap();
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapterByLuid(host.device.GetAdapterLuid())
            .unwrap();
        adapter.GetDesc1().unwrap()
    };
    let name = String::from_utf16_lossy(&adapter.Description);
    let software = adapter.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
    eprintln!(
        "adapter={} vendor={:04x} device={:04x} software={software}",
        name.trim_end_matches('\0'),
        adapter.VendorId,
        adapter.DeviceId
    );
    assert_eq!(software, crate::switch::on("VICTORY_D3D12_WARP"));

    let null = std::ptr::null();
    let create = |kind, desc: ResourceDesc| {
        let mut result = null;
        let properties = HeapProperties { kind, ..PROPERTIES };
        assert_eq!(
            create_committed(
                null,
                &properties,
                0,
                &desc,
                0,
                null.cast(),
                &RESOURCE_IIDS[0],
                &mut result
            ),
            S_OK
        );
        result
    };
    let image = ResourceDesc {
        depth_or_array_size: 2,
        ..texture(129, 3, R4G4_UNORM, 2)
    };
    let (a, b) = (create(1, image), create(1, image));
    let fp = PlacedFootprint {
        format: R4G4_UNORM,
        offset: 512,
        width: 129,
        height: 3,
        depth: 1,
        row_pitch: 256,
    };
    let size = fp.offset + fp.bytes().unwrap();
    let source = create(2, buffer(size));
    let (input, _) = buffer_bytes(source).unwrap();
    let location = |resource, index: u32| CopyLocation {
        resource,
        kind: 0,
        footprint: PlacedFootprint {
            offset: index.into(),
            ..fp
        },
    };
    let footprint = |resource| CopyLocation {
        resource,
        kind: 1,
        footprint: fp,
    };

    let mut allocator = null;
    let mut list = null;
    assert_eq!(
        crate::list::create_allocator(null, 0, &crate::object::IUNKNOWN, &mut allocator),
        S_OK
    );
    assert_eq!(
        crate::list::create_list(
            null,
            0,
            0,
            allocator,
            null,
            &crate::object::IUNKNOWN,
            &mut list
        ),
        S_OK
    );
    let hl = crate::list::host_list(list).unwrap();
    type Copy = extern "system" fn(
        This,
        *const CopyLocation,
        u32,
        u32,
        u32,
        *const CopyLocation,
        *const [u32; 6],
    );
    let copy = slot::<Copy>(list, 0x80);
    copy(
        list,
        &location(a, 2),
        0,
        0,
        0,
        &footprint(source),
        null.cast(),
    );
    slot::<extern "system" fn(This, This, This)>(list, 0x88)(list, b, a);
    // Exercise the cropped RG8 upload and both texture-region paths.
    // Stage a same-image copy to slice 0; copy its resulting 3 texels to
    // a different RG8 image, then back to the sampled slice.
    copy(
        list,
        &location(b, 2),
        1,
        0,
        0,
        &footprint(source),
        &[30, 2, 0, 33, 3, 1],
    );
    copy(
        list,
        &location(b, 0),
        7,
        1,
        0,
        &location(b, 2),
        &[1, 0, 0, 4, 1, 1],
    );
    copy(
        list,
        &location(a, 0),
        3,
        2,
        0,
        &location(b, 0),
        &[7, 1, 0, 10, 2, 1],
    );
    copy(
        list,
        &location(b, 2),
        110,
        1,
        0,
        &location(a, 0),
        &[3, 2, 0, 6, 3, 1],
    );
    // A shader reads the actual host resource through the guest-format SRV.
    // It checks the RG8 host upload and native texture copy independently of the packed mirror.
    let source_hlsl = r#"
#define RS "DescriptorTable(SRV(t0)), UAV(u0), StaticSampler(s0, filter=FILTER_MIN_MAG_MIP_POINT), StaticSampler(s1, filter=FILTER_MIN_MAG_MIP_LINEAR, addressU=TEXTURE_ADDRESS_CLAMP)"
Texture2DArray<float4> tex : register(t0);
SamplerState pointSampler : register(s0);
SamplerState linearSampler : register(s1);
RWByteAddressBuffer dst : register(u0);
[RootSignature(RS)]
[numthreads(1,1,1)]
void main(uint3 p : SV_DispatchThreadID) {
    float4 a = tex.Load(int4(p.xy, 0, 0));
    float4 b = tex.SampleLevel(pointSampler, float3((float2(p.xy)+0.5)/float2(129,3),0), 0);
    float4 c = tex.SampleLevel(linearSampler, float3((float2(p.xy)+float2(1,0.5))/float2(129,3),0), 0);
    uint at = 48*(129*p.y+p.x);
    dst.Store4(at, asuint(a)); dst.Store4(at+16, asuint(b)); dst.Store4(at+32, asuint(c));
}"#;
    let (root, pso) = crate::draw::tests::compile_cs(source_hlsl);
    const RESULT_BYTES: u64 = 48 * 129 * 3;
    let output_buffer = |kind, flags, state| {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: kind,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: RESULT_BYTES,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: flags,
            ..Default::default()
        };
        let mut r: Option<ID3D12Resource> = None;
        // SAFETY: native buffer descriptor and a live output pointer.
        unsafe {
            host.device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut r)
                .unwrap();
        }
        r.unwrap()
    };
    let results = output_buffer(
        D3D12_HEAP_TYPE_DEFAULT,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
    );
    let readback = output_buffer(
        D3D12_HEAP_TYPE_READBACK,
        D3D12_RESOURCE_FLAG_NONE,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    let described = [R4G4_UNORM, 5, 0x1688, 0, 0, 1, 1, 1, 0, 0];
    // SAFETY: one live heap/descriptor and a mapped buffer, kept through execution.
    let heap: ID3D12DescriptorHeap = unsafe {
        host.device
            .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 1,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })
            .unwrap()
    };
    let mut slot_memory = [0u64; 4];
    let mut set_view = |words: &[u32; 10]| {
        let handle = slot_memory.as_mut_ptr() as u64;
        crate::descriptor::create_srv(null, b, words, handle);
        let view = crate::descriptor::view(handle).unwrap();
        // SAFETY: heap and resource are live.
        let cpu = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
        assert!(crate::descriptor::host::make_view(&view, cpu).is_some());
        view
    };
    set_view(&described);
    let dispatch = |cmd: &crate::host::HostList| {
        // SAFETY: open command list and resources retained until the fence.
        unsafe {
            cmd.list.SetDescriptorHeaps(&[Some(heap.clone())]);
            cmd.list.SetComputeRootSignature(&root);
            cmd.list
                .SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
            cmd.list
                .SetComputeRootUnorderedAccessView(1, results.GetGPUVirtualAddress());
            cmd.list.SetPipelineState(&pso);
            cmd.list.Dispatch(129, 3, 1);
            cmd.list.ResourceBarrier(&[crate::host::transition(
                &results,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            cmd.list
                .CopyBufferRegion(&readback, 0, &results, 0, RESULT_BYTES);
            cmd.list.ResourceBarrier(&[crate::host::transition(
                &results,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )]);
        }
    };
    dispatch(hl);
    hl.close();
    let source_value = |x: usize, y: usize| ((x + 129 * y) % 256) as u8;
    // SAFETY: allocated guest buffers contain exactly size bytes.
    unsafe {
        for y in 0..3 {
            for x in 0..129 {
                input.add(512 + y * 256 + x).write(source_value(x, y));
            }
        }
    }
    host.execute(&[hl]);
    let check = |get: &dyn Fn(usize, usize) -> u8, swapped: bool| {
        let mut data = std::ptr::null_mut();
        // SAFETY: the queue completed its writes to this diagnostic readback.
        let values = unsafe {
            readback
                .Map(
                    0,
                    Some(&D3D12_RANGE {
                        Begin: 0,
                        End: RESULT_BYTES as usize,
                    }),
                    Some(&mut data),
                )
                .unwrap();
            std::slice::from_raw_parts(data.cast::<f32>(), RESULT_BYTES as usize / 4)
        };
        let color = |byte: u8| {
            let (r, g) = (f32::from(byte & 15) / 15.0, f32::from(byte >> 4) / 15.0);
            if swapped {
                [g, r, 1.0, 0.0]
            } else {
                [r, g, 0.0, 1.0]
            }
        };
        for y in 0..3 {
            for x in 0..129 {
                let expected = color(get(x, y));
                let right = color(get((x + 1).min(128), y));
                for (n, rgba) in values[12 * (129 * y + x)..][..12]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .enumerate()
                {
                    for k in 0..4 {
                        let want = if n == 2 {
                            (expected[k] + right[k]) * 0.5
                        } else {
                            expected[k]
                        };
                        let eps = if n == 2 && k < 2 {
                            2.0 / 255.0
                        } else if k < 2 {
                            2e-6
                        } else {
                            0.0
                        };
                        assert!(
                            (rgba[k] - want).abs() <= eps,
                            "texel {x},{y}, operation {n}, channel {k}: {} vs {want}",
                            rgba[k]
                        );
                    }
                }
            }
        }
        // SAFETY: no CPU writes to the mapped range.
        unsafe {
            readback.Unmap(0, Some(&D3D12_RANGE::default()));
        }
    };
    let value = |x: usize, y: usize| match (x, y) {
        (1..=3, 0) => source_value(30 + x - 1, 2),
        (110..=112, 1) => source_value(30 + x - 110, 2),
        _ => source_value(x, y),
    };
    check(&value, false);
    let watch = host.new_list().unwrap();
    crate::texture::upload(&watch, b, host_resource(b).unwrap());
    crate::list::ensure_state(&watch, b, 0x40);
    dispatch(&watch);
    watch.close();
    let (mem, desc) = texture_memory(b).unwrap();
    let tex = texture_layout(&desc).unwrap();
    let offset = tex.planes[0].layout.offset(0, 128, 2, 1).unwrap() as usize;
    // SAFETY: the packed texel in array slice 1, changed after recording.
    unsafe {
        mem.cast_mut().add(offset).write(0x4d);
    }
    host.execute(&[&watch]);
    let updated = |x, y| {
        if (x, y) == (128, 2) {
            0x4d
        } else {
            value(x, y)
        }
    };
    check(&updated, false);
    // The same actual resource, refreshed through the bindless submission
    // prelude and viewed with guest UNKNOWN and explicit (G,R,1,0) routing.
    let mut swizzle = described;
    swizzle[0] = 0;
    swizzle[2] = 1 | (5 << 6) | (4 << 9) | (1 << 12);
    let view = set_view(&swizzle);
    let viewed = std::sync::Arc::<crate::bindless::Viewed>::default();
    viewed.set(0, Some(&view), true);
    let bindless = host.new_list().unwrap();
    bindless.defer_heap(&viewed);
    dispatch(&bindless);
    bindless.close();
    // SAFETY: same live packed array texel, changed before submission.
    unsafe {
        mem.cast_mut().add(offset).write(0xe1);
    }
    host.execute(&[&bindless]);
    check(
        &|x, y| {
            if (x, y) == (128, 2) {
                0xe1
            } else {
                value(x, y)
            }
        },
        true,
    );
    drop(bindless);
    drop(watch);
    for object in [list, allocator, a, b, source] {
        slot::<Release>(object, 0x10)(object);
    }
}
