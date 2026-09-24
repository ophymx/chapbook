//! Decoded raster images, keyed by opaque producer ids (chapbook-layout
//! keys by DOM node tag; a comic producer would key by page index). Shared
//! between layout (intrinsic dimensions) and renderers (pixels).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use chapbook_core::Rgba;

/// Store identity for renderer-side caches (see [`ImageStore::id`]).
static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(1);

/// One CSS filter function, as a producer hands it to [`ImageStore::derive`].
///
/// The colour-matrix and per-channel functions of CSS Filter Effects — the
/// ones a pixel can answer for itself. `blur()`, `drop-shadow()` and `url()`
/// need neighbours or a document and are not here. Amounts are thousandths
/// (`Invert(1000)` is `invert(100%)`) and the angle is whole degrees, so the
/// enum is `Hash`/`Eq` and can key the derived-image cache without a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFilter {
    Brightness(u16),
    Contrast(u16),
    Grayscale(u16),
    /// Degrees, any sign; reduced modulo 360 when applied.
    HueRotate(i16),
    Invert(u16),
    Opacity(u16),
    Saturate(u16),
    Sepia(u16),
}

#[derive(PartialEq, Eq, Hash)]
struct DerivedKey {
    source: u64,
    background: Option<Rgba>,
    filters: Vec<ImageFilter>,
}

pub struct ImageStore {
    id: u64,
    images: HashMap<u64, StoredImage>,
    /// Filtered variants already computed, so a page that shows the same
    /// image twice, or a relayout at the same theme, pays once.
    derived: HashMap<DerivedKey, u64>,
    /// Ids for derived images count down from the top of the space, where
    /// no producer's tag lives (a DOM node tag is a slotmap key: a small
    /// version in the high half, an index in the low).
    next_derived: u64,
}

impl Default for ImageStore {
    fn default() -> Self {
        ImageStore {
            id: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
            images: HashMap::new(),
            derived: HashMap::new(),
            next_derived: u64::MAX,
        }
    }
}

/// Premultiplied RGBA8, converted once at [`ImageStore::insert`] — a frame
/// composites images without touching the pixels again (tiny-skia reads
/// them in place; vello marks the upload premultiplied).
pub struct StoredImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ImageStore {
    /// Store straight (non-premultiplied) RGBA8 — what decoders produce.
    /// Premultiplication happens here, once, instead of per frame in every
    /// backend. Opaque images (comics, PDF pages) pass through unchanged.
    pub fn insert(&mut self, id: u64, width: u32, height: u32, mut rgba: Vec<u8>) {
        debug_assert_eq!(rgba.len(), (width * height * 4) as usize);
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = u16::from(px[3]);
            if a < 255 {
                px[0] = (u16::from(px[0]) * a / 255) as u8;
                px[1] = (u16::from(px[1]) * a / 255) as u8;
                px[2] = (u16::from(px[2]) * a / 255) as u8;
            }
        }
        self.images.insert(
            id,
            StoredImage {
                width,
                height,
                rgba,
            },
        );
    }

    /// A process-unique identity, changing with every new store. What a
    /// renderer-side cache (e.g. vello's image blobs) keys on to know its
    /// entries describe *this* store — resource ids alone repeat across
    /// chapters, since they come from per-document arenas.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn get(&self, id: u64) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    /// The image `source` would paint as with `background` under it and
    /// `filters` applied in order, as a stored image of its own: the id to
    /// put in an image fragment instead of `source`. Computed once and
    /// cached by its inputs; `None` when `source` is not here.
    ///
    /// CSS filters the element's whole rendering, background included —
    /// which is what a book that paints its black-on-transparent line art
    /// on white and inverts it for a dark scheme relies on: the white goes
    /// black with the lines going white, rather than white lines vanishing
    /// on a white box. So the background composites *before* the filters,
    /// and a producer that has a filter hands the background here rather
    /// than painting it as a fill. With no background and no filters the
    /// answer is `source` itself and nothing is copied.
    ///
    /// The maths is CSS Filter Effects Level 1, in sRGB on straight alpha,
    /// clamped after every function; the store's pixels are premultiplied,
    /// so they are unpremultiplied on the way in and re-premultiplied on
    /// the way out.
    pub fn derive(
        &mut self,
        source: u64,
        background: Option<Rgba>,
        filters: &[ImageFilter],
    ) -> Option<u64> {
        let background = background.filter(|c| c.a > 0);
        if background.is_none() && filters.is_empty() {
            return self.images.contains_key(&source).then_some(source);
        }
        let key = DerivedKey {
            source,
            background,
            filters: filters.to_vec(),
        };
        if let Some(id) = self.derived.get(&key) {
            return Some(*id);
        }
        let src = self.images.get(&source)?;
        let (width, height) = (src.width, src.height);
        let mut rgba = src.rgba.clone();
        for px in rgba.as_chunks_mut::<4>().0 {
            let mut c = unpremultiply(*px);
            if let Some(bg) = background {
                c = over(c, bg);
            }
            for f in filters {
                c = apply(*f, c);
            }
            *px = premultiply(c);
        }
        let mut id = self.next_derived;
        while self.images.contains_key(&id) {
            id -= 1;
        }
        self.next_derived = id - 1;
        self.images.insert(
            id,
            StoredImage {
                width,
                height,
                rgba,
            },
        );
        self.derived.insert(key, id);
        Some(id)
    }

    /// Intrinsic size in CSS px (1 image px = 1 CSS px).
    pub fn dims(&self, id: u64) -> Option<(u32, u32)> {
        self.images.get(&id).map(|i| (i.width, i.height))
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Bytes of decoded pixels held here.
    ///
    /// Exact, and the term that matters: a 1600x2400 comic page is 15.4 MB
    /// of RGBA whatever the display can show, so this is what a session's
    /// cache budget is mostly spending.
    pub fn bytes(&self) -> usize {
        self.images.values().map(|i| i.rgba.len()).sum()
    }
}

// ---- Filter maths: straight-alpha sRGB in [0, 1] ----

/// Straight (non-premultiplied) colour with alpha, each in `0..=1`.
#[derive(Clone, Copy)]
struct Straight {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

fn unpremultiply(px: [u8; 4]) -> Straight {
    let a = f32::from(px[3]) / 255.0;
    let un = |v: u8| {
        if a > 0.0 {
            (f32::from(v) / 255.0 / a).min(1.0)
        } else {
            0.0
        }
    };
    Straight {
        r: un(px[0]),
        g: un(px[1]),
        b: un(px[2]),
        a,
    }
}

fn premultiply(c: Straight) -> [u8; 4] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * c.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    [
        q(c.r),
        q(c.g),
        q(c.b),
        (c.a.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// `c` composited over `bg` (source-over, straight alpha).
fn over(c: Straight, bg: Rgba) -> Straight {
    let ba = f32::from(bg.a) / 255.0;
    let a = c.a + ba * (1.0 - c.a);
    if a <= 0.0 {
        return Straight {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }
    let mix = |fg: f32, bgc: u8| (fg * c.a + f32::from(bgc) / 255.0 * ba * (1.0 - c.a)) / a;
    Straight {
        r: mix(c.r, bg.r),
        g: mix(c.g, bg.g),
        b: mix(c.b, bg.b),
        a,
    }
}

/// One filter function on one straight-alpha pixel, per the spec's
/// matrices (which are written against sRGB, not linear light).
fn apply(f: ImageFilter, c: Straight) -> Straight {
    let amount = |k: u16| f32::from(k) / 1000.0;
    let matrix = |m: [f32; 9]| Straight {
        r: m[0] * c.r + m[1] * c.g + m[2] * c.b,
        g: m[3] * c.r + m[4] * c.g + m[5] * c.b,
        b: m[6] * c.r + m[7] * c.g + m[8] * c.b,
        a: c.a,
    };
    let out = match f {
        ImageFilter::Brightness(k) => {
            let k = amount(k);
            Straight {
                r: c.r * k,
                g: c.g * k,
                b: c.b * k,
                a: c.a,
            }
        }
        ImageFilter::Contrast(k) => {
            let k = amount(k);
            let i = 0.5 - 0.5 * k;
            Straight {
                r: c.r * k + i,
                g: c.g * k + i,
                b: c.b * k + i,
                a: c.a,
            }
        }
        ImageFilter::Grayscale(k) => {
            let t = 1.0 - amount(k).min(1.0);
            matrix([
                0.2126 + 0.7874 * t,
                0.7152 - 0.7152 * t,
                0.0722 - 0.0722 * t,
                0.2126 - 0.2126 * t,
                0.7152 + 0.2848 * t,
                0.0722 - 0.0722 * t,
                0.2126 - 0.2126 * t,
                0.7152 - 0.7152 * t,
                0.0722 + 0.9278 * t,
            ])
        }
        ImageFilter::Sepia(k) => {
            let t = 1.0 - amount(k).min(1.0);
            matrix([
                0.393 + 0.607 * t,
                0.769 - 0.769 * t,
                0.189 - 0.189 * t,
                0.349 - 0.349 * t,
                0.686 + 0.314 * t,
                0.168 - 0.168 * t,
                0.272 - 0.272 * t,
                0.534 - 0.534 * t,
                0.131 + 0.869 * t,
            ])
        }
        ImageFilter::Saturate(k) => {
            let s = amount(k);
            matrix([
                0.213 + 0.787 * s,
                0.715 - 0.715 * s,
                0.072 - 0.072 * s,
                0.213 - 0.213 * s,
                0.715 + 0.285 * s,
                0.072 - 0.072 * s,
                0.213 - 0.213 * s,
                0.715 - 0.715 * s,
                0.072 + 0.928 * s,
            ])
        }
        ImageFilter::HueRotate(deg) => {
            let (sn, cs) = (f32::from(deg).rem_euclid(360.0)).to_radians().sin_cos();
            matrix([
                0.213 + 0.787 * cs - 0.213 * sn,
                0.715 - 0.715 * cs - 0.715 * sn,
                0.072 - 0.072 * cs + 0.928 * sn,
                0.213 - 0.213 * cs + 0.143 * sn,
                0.715 + 0.285 * cs + 0.140 * sn,
                0.072 - 0.072 * cs - 0.283 * sn,
                0.213 - 0.213 * cs - 0.787 * sn,
                0.715 - 0.715 * cs + 0.715 * sn,
                0.072 + 0.928 * cs + 0.072 * sn,
            ])
        }
        ImageFilter::Invert(k) => {
            let k = amount(k).min(1.0);
            let inv = |v: f32| v + k * (1.0 - 2.0 * v);
            Straight {
                r: inv(c.r),
                g: inv(c.g),
                b: inv(c.b),
                a: c.a,
            }
        }
        ImageFilter::Opacity(k) => Straight {
            a: c.a * amount(k).min(1.0),
            ..c
        },
    };
    Straight {
        r: out.r.clamp(0.0, 1.0),
        g: out.g.clamp(0.0, 1.0),
        b: out.b.clamp(0.0, 1.0),
        a: out.a.clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(id: u64, px: [u8; 4]) -> ImageStore {
        let mut store = ImageStore::default();
        store.insert(id, 1, 1, px.to_vec());
        store
    }

    fn pixel(store: &ImageStore, id: u64) -> [u8; 4] {
        let img = store.get(id).unwrap();
        [img.rgba[0], img.rgba[1], img.rgba[2], img.rgba[3]]
    }

    #[test]
    fn nothing_asked_answers_the_source_itself() {
        let mut store = store_with(7, [200, 10, 10, 255]);
        assert_eq!(store.derive(7, None, &[]), Some(7));
        assert_eq!(store.derive(7, Some(Rgba::TRANSPARENT), &[]), Some(7));
        assert_eq!(store.derive(8, None, &[ImageFilter::Invert(1000)]), None);
    }

    #[test]
    fn invert_flips_a_colour_and_is_cached() {
        let mut store = store_with(1, [255, 0, 0, 255]);
        let a = store.derive(1, None, &[ImageFilter::Invert(1000)]).unwrap();
        assert_ne!(a, 1);
        assert_eq!(pixel(&store, a), [0, 255, 255, 255]);
        let b = store.derive(1, None, &[ImageFilter::Invert(1000)]).unwrap();
        assert_eq!(a, b, "the same request is the same variant");
        assert_eq!(store.bytes(), 8, "one source, one variant");
    }

    #[test]
    fn a_background_composites_before_the_filter() {
        // Black line art on transparent, painted on white, then inverted:
        // the line goes white and the ground goes black — the dark-scheme
        // rendering a book that does this is asking for.
        let mut store = ImageStore::default();
        store.insert(1, 2, 1, vec![0, 0, 0, 255, 0, 0, 0, 0]);
        let id = store
            .derive(1, Some(Rgba::WHITE), &[ImageFilter::Invert(1000)])
            .unwrap();
        let img = store.get(id).unwrap();
        assert_eq!(&img.rgba[0..4], &[255, 255, 255, 255], "the line");
        assert_eq!(&img.rgba[4..8], &[0, 0, 0, 255], "the ground");
    }

    #[test]
    fn grayscale_sepia_and_opacity_do_what_the_spec_says() {
        let mut store = store_with(1, [255, 0, 0, 255]);
        let g = store
            .derive(1, None, &[ImageFilter::Grayscale(1000)])
            .unwrap();
        assert_eq!(pixel(&store, g), [54, 54, 54, 255]);
        let s = store.derive(1, None, &[ImageFilter::Sepia(1000)]).unwrap();
        assert_eq!(pixel(&store, s), [100, 89, 69, 255]);
        // Opacity halves alpha, and the stored pixel is premultiplied.
        let o = store.derive(1, None, &[ImageFilter::Opacity(500)]).unwrap();
        assert_eq!(pixel(&store, o), [128, 0, 0, 128]);
    }

    #[test]
    fn filters_apply_in_order_and_clamp_between_them() {
        // Contrast then brightness: 0.8 → 1.1, clamped to 1, halved to 0.5.
        // Brightness then contrast: 0.8 → 0.4 → 0.3. Different answers,
        // because each function sees the previous one's clamped output.
        let mut store = store_with(1, [204, 204, 204, 255]);
        let a = store
            .derive(
                1,
                None,
                &[ImageFilter::Contrast(2000), ImageFilter::Brightness(500)],
            )
            .unwrap();
        let b = store
            .derive(
                1,
                None,
                &[ImageFilter::Brightness(500), ImageFilter::Contrast(2000)],
            )
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(pixel(&store, a), [128, 128, 128, 255]);
        assert_eq!(pixel(&store, b), [77, 77, 77, 255]);
    }
}
