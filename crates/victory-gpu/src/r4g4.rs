//! Packed Xbox R4G4 is authoritative. Only CPU uploads and copies between
//! resources with this encoding may write it. Submission-time copies keep
//! packed memory current for later CPU writes and watched texture uploads.
use victory_formats::texture::Layout;

/// Detiling put the packed bytes at the beginning of each host row.
/// Expand backwards so no unread nibble is overwritten. Padding is untouched.
pub(crate) fn expand_rows(data: &mut [u8], width: usize, rows: usize, pitch: usize) -> bool {
    let Some(active) = width.checked_mul(2) else {
        return false;
    };
    if width == 0
        || rows == 0
        || pitch < active
        || (rows - 1)
            .checked_mul(pitch)
            .and_then(|n| n.checked_add(active))
            .is_none_or(|n| n > data.len())
    {
        return false;
    }
    for row in data.chunks_mut(pitch).take(rows) {
        for x in (0..width).rev() {
            let packed = row[x];
            row[2 * x] = (packed & 15) * 17;
            row[2 * x + 1] = (packed >> 4) * 17;
        }
    }
    true
}

enum Plane {
    Buffer {
        width: u32,
        height: u32,
        pitch: u32,
    },
    Texture {
        layout: std::sync::Arc<Layout>,
        mip: u32,
        slice: u32,
    },
}

impl Plane {
    fn size(&self) -> [u32; 2] {
        match self {
            Self::Buffer { width, height, .. } => [*width, *height],
            Self::Texture { layout, mip, .. } => {
                let m = &layout.mips[*mip as usize];
                [m.width, m.height]
            }
        }
    }

    /// Packed runs in one rectangle, in reading order. Tiled guest padding
    /// and neighboring subresources never appear among the destination runs.
    fn runs(
        &self,
        [x, y]: [u32; 2],
        [width, height]: [u32; 2],
        mut visit: impl FnMut(usize, usize, usize),
    ) {
        for row in 0..height {
            let linear = row as usize * width as usize;
            match self {
                Self::Buffer { pitch, .. } => visit(
                    (y + row) as usize * *pitch as usize + x as usize,
                    linear,
                    width as usize,
                ),
                Self::Texture { layout, mip, slice } => {
                    let offset = |col| {
                        layout
                            .offset(*mip, x + col, y + row, *slice)
                            .expect("validated single-sample layout and rectangle")
                            as usize
                    };
                    let mut start = 0;
                    let mut first = offset(0);
                    if layout.desc.swizzle != victory_formats::texture::Swizzle::LINEAR {
                        for col in 1..width {
                            let next = offset(col);
                            if next != first + (col - start) as usize {
                                visit(first, linear + start as usize, (col - start) as usize);
                                first = next;
                                start = col;
                            }
                        }
                    }
                    visit(first, linear + start as usize, (width - start) as usize);
                }
            }
        }
    }
}

struct Region {
    from: [u32; 2],
    to: [u32; 2],
    size: [u32; 2],
}

impl Region {
    fn new(src: &Plane, dst: &Plane, origin: [u32; 3], area: Option<[u32; 6]>) -> Option<Self> {
        let [sw, sh] = src.size();
        let [dw, dh] = dst.size();
        let [x, y, front, r, b, back] = area.unwrap_or([0, 0, 0, sw, sh, 1]);
        let [dx, dy, dz] = origin;
        let (w, h) = (r.checked_sub(x)?, b.checked_sub(y)?);
        if front != 0
            || back != 1
            || dz != 0
            || r > sw
            || b > sh
            || dx.checked_add(w)? > dw
            || dy.checked_add(h)? > dh
        {
            return None;
        }
        Some(Self {
            from: [x, y],
            to: [dx, dy],
            size: [w, h],
        })
    }
}

#[cfg(windows)]
mod commands {
    use super::*;
    use crate::{host::HostList, list::CopyLocation, object::This, resource, texture};
    use std::sync::Arc;

    pub(crate) fn involved(loc: &CopyLocation) -> bool {
        resource::title_format(loc.resource) == Some(resource::R4G4_UNORM)
            || (loc.kind == 1 && loc.footprint.format == resource::R4G4_UNORM)
    }

    struct Surface {
        base: usize,
        len: usize,
        plane: Plane,
    }

    impl Surface {
        fn location(loc: &CopyLocation) -> Option<Self> {
            match loc.kind {
                0 => {
                    let (base, desc) = resource::texture_memory(loc.resource)?;
                    if resource::host_copy(loc.resource)?.encoding
                        != resource::TextureEncoding::R4G4Expanded
                    {
                        return None;
                    }
                    let layout = Arc::new(
                        resource::texture_layout(&desc)?
                            .planes
                            .into_iter()
                            .next()?
                            .layout,
                    );
                    Self::subresource(base as usize, layout, loc.footprint.offset as u32)
                }
                1 => {
                    let f = loc.footprint;
                    let (data, capacity) = resource::sampled_copy_buffer(loc.resource)?;
                    if f.format != resource::R4G4_UNORM
                        || f.depth != 1
                        || f.width == 0
                        || f.height == 0
                        || f.width > f.row_pitch
                        || !f.row_pitch.is_multiple_of(256)
                        || !f.offset.is_multiple_of(512)
                    {
                        return None;
                    }
                    let len = f.bytes()?;
                    if f.offset.checked_add(len)? > capacity {
                        return None;
                    }
                    Some(Self {
                        base: (data as usize).checked_add(f.offset as usize)?,
                        len: usize::try_from(len).ok()?,
                        plane: Plane::Buffer {
                            width: f.width,
                            height: f.height,
                            pitch: f.row_pitch,
                        },
                    })
                }
                _ => None,
            }
        }

        fn subresource(base: usize, layout: Arc<Layout>, index: u32) -> Option<Self> {
            let mips = u32::try_from(layout.mips.len()).ok()?;
            let (mip, slice) = (index % mips, index / mips);
            if slice >= layout.slices || !layout.addressable() {
                return None;
            }
            Some(Self {
                base,
                len: usize::try_from(layout.size).ok()?,
                plane: Plane::Texture { layout, mip, slice },
            })
        }
    }

    struct Transfer {
        src: Surface,
        dst: Surface,
        region: Region,
    }

    impl Transfer {
        fn new(
            src: Surface,
            dst: Surface,
            origin: [u32; 3],
            area: Option<[u32; 6]>,
        ) -> Option<Self> {
            let region = Region::new(&src.plane, &dst.plane, origin, area)?;
            if src.base < dst.base.checked_add(dst.len)?
                && dst.base < src.base.checked_add(src.len)?
            {
                let different_subresources = matches!((&src.plane, &dst.plane),
                    (Plane::Texture { mip:a, slice:b, .. }, Plane::Texture { mip:c, slice:d, .. }) if (a,b) != (c,d));
                if src.base != dst.base || !different_subresources {
                    return None;
                }
            }
            Some(Self { src, dst, region })
        }

        fn run(&self) -> Vec<u8> {
            let Self { src, dst, region } = self;
            // Resource references retained by the list keep committed/placed backing
            // alive. Runtime page guards must be opened before checking writability.
            let _write = crate::watch::writing(dst.base, dst.len);
            if !texture::committed(src.base as *const u8, src.len)
                || !texture::writable(dst.base as *const u8, dst.len)
            {
                crate::host::fail(format_args!(
                    "retained R4G4 copy lost its mapped backing before submission"
                ));
            }
            let [w, h] = region.size;
            let mut packed = vec![0; w as usize * h as usize];
            src.plane.runs(region.from, region.size, |at, off, n| {
                // SAFETY: both runs lie within the validated, distinct allocations.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        (src.base + at) as *const u8,
                        packed.as_mut_ptr().add(off),
                        n,
                    )
                };
            });
            dst.plane.runs(region.to, region.size, |at, off, n| {
                // SAFETY: only the destination texels of the checked rectangle are writable here.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        packed.as_ptr().add(off),
                        (dst.base + at) as *mut u8,
                        n,
                    )
                };
            });
            packed
        }
    }

    const SAMPLE: u32 = 0x40 | 0x80;
    const COPY_DEST: u32 = 0x400;
    const COPY_SOURCE: u32 = 0x800;

    pub(crate) fn region(
        list: &HostList,
        dst: &CopyLocation,
        origin: [u32; 3],
        src: &CopyLocation,
        area: Option<[u32; 6]>,
    ) -> Option<()> {
        use windows::Win32::Graphics::Direct3D12::{D3D12_BOX, D3D12_TEXTURE_COPY_LOCATION};
        use windows::core::Interface;
        if dst.kind != 0 {
            return None;
        }
        let copy = Transfer::new(
            Surface::location(src)?,
            Surface::location(dst)?,
            origin,
            area,
        )?;
        let [w, h] = copy.region.size;
        if w == 0 || h == 0 {
            return Some(());
        }
        let to = resource::host_resource(dst.resource)?;
        let from = resource::host_resource(src.resource);
        let mut host_src = *src;
        let mut host_dst = *dst;
        let mut host_box = area;
        host_dst.resource = to.as_raw().cast_const().cast();
        let staging = if let Some(from) = from
            && from != to
        {
            host_src.resource = from.as_raw().cast_const().cast();
            None
        } else {
            // The tracker transitions whole resources. Stage a copy between
            // subresources of one image to avoid simultaneous SRC/DEST states.
            let pitch = w.checked_mul(2)?.checked_next_multiple_of(256)?;
            let size =
                usize::try_from(u64::from(h - 1) * u64::from(pitch) + 2 * u64::from(w)).ok()?;
            let (buffer, mapped) = list.upload(crate::host::host()?, size as u64)?;
            host_src = CopyLocation {
                resource: buffer.as_raw().cast_const().cast(),
                kind: 1,
                footprint: resource::PlacedFootprint {
                    offset: 0,
                    format: resource::R4G4_HOST_FORMAT,
                    width: w,
                    height: h,
                    depth: 1,
                    row_pitch: pitch,
                },
            };
            host_box = None;
            Some((mapped as usize, size, pitch as usize))
        };
        super::keep(list, src.resource);
        super::keep(list, dst.resource);
        // A first partial destination write must retain the other texels,
        // array slices and mips, including title writes made before submit.
        texture::upload(list, dst.resource, to);
        if let Some(from) = from
            && from != to
        {
            texture::upload(list, src.resource, from);
            crate::list::ensure_state(list, src.resource, COPY_SOURCE);
        }
        let upload = (src.kind == 1).then_some(src.resource as usize);
        list.defer(move || {
            if upload.is_some_and(|r| resource::sampled_copy_buffer(r as This).is_none()) {
                crate::host::fail(format_args!(
                    "R4G4 upload source acquired an incompatible alias before execution"
                ));
            }
            let packed = copy.run();
            if let Some((mapped, size, pitch)) = staging {
                // SAFETY: list.upload retains the mapping until reset/fence.
                let rows = unsafe { std::slice::from_raw_parts_mut(mapped as *mut u8, size) };
                for (row, input) in rows.chunks_mut(pitch).zip(packed.chunks_exact(w as usize)) {
                    row[..input.len()].copy_from_slice(input);
                }
                // Sizes came from the checked RG8 footprint above.
                assert!(expand_rows(rows, w as usize, h as usize, pitch));
            }
        });
        crate::list::ensure_state(list, dst.resource, COPY_DEST);
        // SAFETY: validated guest locations have the D3D12 ABI. Cropped
        // upload footprints start at zero and are retained by the list.
        unsafe {
            list.list.CopyTextureRegion(
                (&raw const host_dst).cast::<D3D12_TEXTURE_COPY_LOCATION>(),
                origin[0],
                origin[1],
                0,
                (&raw const host_src).cast(),
                host_box
                    .as_ref()
                    .map(|b| std::ptr::from_ref(b).cast::<D3D12_BOX>()),
            )
        };
        crate::list::ensure_state(list, dst.resource, SAMPLE);
        if let Some(from) = from
            && from != to
        {
            crate::list::ensure_state(list, src.resource, SAMPLE);
        }
        Some(())
    }

    pub(crate) fn whole(list: &HostList, dst: This, src: This) -> Option<()> {
        let image = |r| {
            if resource::host_copy(r)?.encoding != resource::TextureEncoding::R4G4Expanded {
                return None;
            }
            let (base, desc) = resource::texture_memory(r)?;
            Some((
                base as usize,
                Arc::new(
                    resource::texture_layout(&desc)?
                        .planes
                        .into_iter()
                        .next()?
                        .layout,
                ),
            ))
        };
        let (a, from) = image(src)?;
        let (b, to) = image(dst)?;
        if from.slices != to.slices
            || from.mips.len() != to.mips.len()
            || from.desc.width != to.desc.width
            || from.desc.height != to.desc.height
        {
            return None;
        }
        let copies = (0..from.slices * from.mips.len() as u32)
            .map(|index| {
                Transfer::new(
                    Surface::subresource(a, from.clone(), index)?,
                    Surface::subresource(b, to.clone(), index)?,
                    [0; 3],
                    None,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        let (a, b) = (resource::host_resource(src)?, resource::host_resource(dst)?);
        super::keep(list, src);
        super::keep(list, dst);
        texture::upload(list, src, a);
        list.defer(move || {
            for copy in &copies {
                copy.run();
            }
        });
        crate::list::ensure_state(list, src, COPY_SOURCE);
        crate::list::ensure_state(list, dst, COPY_DEST);
        // SAFETY: distinct typed RG8 images with identical sizes and subresource counts.
        unsafe {
            list.list.CopyResource(b, a);
        }
        crate::list::ensure_state(list, dst, SAMPLE);
        crate::list::ensure_state(list, src, SAMPLE);
        Some(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use victory_formats::texture::{Desc, Dimension, Swizzle};

        #[test]
        fn ordered_packed_upload_and_copy_between_array_slices() {
            let layout = Arc::new(
                Layout::new(&Desc {
                    dimension: Dimension::Tex2d,
                    width: 19,
                    height: 9,
                    depth: 2,
                    mips: 5,
                    samples: 1,
                    element_bytes: 1,
                    block: 1,
                    swizzle: Swizzle::SW_4KB_S,
                })
                .unwrap(),
            );
            let mut bytes = vec![0x9a; layout.size as usize];
            let base = bytes.as_mut_ptr() as usize;
            let tex = |i| Surface::subresource(base, layout.clone(), i).unwrap();
            let mut input = vec![0x80u8; 512 + 19];
            let buffer = Surface {
                base: input.as_ptr() as usize,
                len: input.len(),
                plane: Plane::Buffer {
                    width: 19,
                    height: 3,
                    pitch: 256,
                },
            };
            let upload =
                Transfer::new(buffer, tex(1), [2, 0, 0], Some([3, 1, 0, 8, 3, 1])).unwrap();
            let copy = Transfer::new(tex(1), tex(6), [3, 1, 0], Some([2, 0, 0, 7, 2, 1])).unwrap();
            assert!(Transfer::new(tex(1), tex(1), [0; 3], None).is_none());
            input[259..264].copy_from_slice(&[1, 2, 3, 4, 5]);
            input[515..520].copy_from_slice(&[6, 7, 8, 9, 10]);
            let expected = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
            assert_eq!(upload.run(), expected);
            assert_eq!(copy.run(), expected);
            let mut reference = vec![0x9a; bytes.len()];
            for (slice, x, y) in [(0, 2, 0), (1, 3, 1)] {
                let mut mip = [0x9a; 9 * 4];
                for row in 0..2 {
                    mip[(y + row) * 9 + x..][..5].copy_from_slice(&expected[row * 5..][..5]);
                }
                layout.tile(&mut reference, 1, slice, &mip, 9, 36).unwrap();
            }
            assert_eq!(bytes, reference);
        }
    }
}

#[cfg(windows)]
pub(crate) use commands::{involved, region, whole};

/// The guest memory must outlive deferred reads, including list replays.
#[cfg(windows)]
pub(crate) fn keep(list: &crate::host::HostList, resource: crate::object::This) {
    use windows::core::{IUnknown, Interface};
    crate::object::add_ref(resource);
    // SAFETY: guest resources implement IUnknown's first three slots. from_raw owns the new reference.
    let owner = unsafe { IUnknown::from_raw(resource.cast_mut().cast()) };
    list.keep(&owner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use victory_formats::texture::{Desc, Dimension, Swizzle};

    #[test]
    fn expansion_preserves_all_colors_and_rejects_every_off_grid_pair() {
        let mut bytes = [0; 512];
        for (i, byte) in bytes[..256].iter_mut().enumerate() {
            *byte = i as u8;
        }
        assert!(expand_rows(&mut bytes, 256, 1, 512));
        let mut inverse = [None; 65536];
        for (packed, rg) in bytes.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(usize::from(rg[0]) * 15, (packed % 16) * 255);
            assert_eq!(usize::from(rg[1]) * 15, (packed / 16) * 255);
            let code = u16::from_le_bytes([rg[0], rg[1]]) as usize;
            assert!(inverse[code].replace(packed).is_none());
        }
        for (code, guest) in inverse.iter().enumerate() {
            let [r, g] = (code as u16).to_le_bytes();
            assert_eq!(
                *guest,
                (r.is_multiple_of(17) && g.is_multiple_of(17))
                    .then_some(usize::from(g / 17) * 16 + usize::from(r / 17))
            );
        }
    }

    #[test]
    fn expansion_checks_both_row_ends_before_writing() {
        for width in [129usize, 257] {
            let pitch = (width * 2).next_multiple_of(256);
            let mut rows = vec![0xcd; 2 * pitch + width * 2];
            for y in 0..3 {
                rows[y * pitch..][..width].fill(0xa1 + y as u8);
            }
            let before = rows.clone();
            let n = rows.len() - 1;
            assert!(!expand_rows(&mut rows[..n], width, 3, pitch));
            assert!(!expand_rows(&mut rows, width, 3, width * 2 - 1));
            assert_eq!(rows, before);
            assert!(expand_rows(&mut rows, width, 3, pitch));
            for (y, row) in rows.chunks(pitch).enumerate() {
                assert!(
                    row[..width * 2]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .all(|p| *p == [17 * (1 + y as u8), 170])
                );
                assert!(row[width * 2..].iter().all(|&b| b == 0xcd));
            }
        }
    }

    #[test]
    fn cropped_copies_touch_only_the_selected_packed_mip_and_slice() {
        for mode in [Swizzle::LINEAR, Swizzle::SW_4KB_S] {
            let layout = std::sync::Arc::new(
                Layout::new(&Desc {
                    dimension: Dimension::Tex2d,
                    width: 19,
                    height: 9,
                    depth: 2,
                    mips: 5,
                    samples: 1,
                    element_bytes: 1,
                    block: 1,
                    swizzle: mode,
                })
                .unwrap(),
            );
            let dst = Plane::Texture {
                layout: layout.clone(),
                mip: 1,
                slice: 1,
            };
            let src = Plane::Buffer {
                width: 8,
                height: 3,
                pitch: 256,
            };
            let region = Region::new(&src, &dst, [2, 1, 0], Some([1, 0, 0, 7, 3, 1])).unwrap();
            let input: Vec<_> = (0..520).map(|i| (i % 251) as u8).collect();
            let mut rect = [0; 18];
            src.runs(region.from, region.size, |p, q, n| {
                rect[q..q + n].copy_from_slice(&input[p..p + n])
            });
            let mut actual = vec![0xcd; layout.size as usize];
            dst.runs(region.to, region.size, |p, q, n| {
                actual[p..p + n].copy_from_slice(&rect[q..q + n])
            });
            let mut expected = vec![0xcd; actual.len()];
            let mut mip = [0xcd; 9 * 4];
            for y in 0..3 {
                mip[(y + 1) * 9 + 2..][..6].copy_from_slice(&input[y * 256 + 1..][..6]);
            }
            layout.tile(&mut expected, 1, 1, &mip, 9, 36).unwrap();
            assert_eq!(actual, expected);
            assert!(Region::new(&src, &dst, [u32::MAX, 0, 0], None).is_none());
            assert!(Region::new(&src, &dst, [0, 0, 1], None).is_none());
            assert!(Region::new(&src, &dst, [0; 3], Some([3, 0, 0, 2, 1, 1])).is_none());
        }
    }
}
