//! Cross-backend parity: the same display list, rasterized by tiny-skia on
//! the CPU and by vello on the GPU.
//!
//! These will never agree byte-for-byte — different rasterizers, different
//! antialiasing, different glyph hinting — so the comparison is
//! structural: the page is divided into cells and the two backends must
//! agree about how much ink lands in each. That is enough to catch what
//! actually goes wrong across a seam (a run positioned at the wrong
//! origin, a fill in the wrong place, a scale applied twice) while
//! tolerating what legitimately differs.
//!
//! Skipped, not failed, where no wgpu adapter exists: CI machines without
//! a GPU and without a software Vulkan implementation can't run these, and
//! that is not a regression in chapbook. Skipped on WARP too — see
//! [`on_warp`] — because a rasterizer that takes the process down cannot
//! rasterize anything worth comparing.

use std::path::PathBuf;

use chapbook_core::{EdgeSizes, PageMetrics, Rotation, Size};
use chapbook_reader::{Session, SessionConfig};
use chapbook_render_vello::{RenderedPage, VelloRenderer};
use vello::wgpu;

/// Whether the adapter the renderer would pick is WARP — D3D12's software
/// rasterizer, and the only adapter a hosted Windows CI runner has.
///
/// vello on WARP took the test process down with `STATUS_ACCESS_VIOLATION`
/// on five of thirteen Windows CI runs, on the same runner image the
/// other eight passed on, a few seconds into the first test and with
/// nothing of chapbook's on the stack. Serializing the three tests did not
/// change it, so it is not a race between them; it is WARP. Mesa's
/// software Vulkan runs the same three tests on every Linux CI run and
/// passes, which is the software-adapter coverage this suite was written
/// for. A crash inside a driver is not a failure this suite can report, so
/// on WARP it reports nothing.
///
/// The probe requests an adapter the way `VelloRenderer::new` does and
/// stops there: enumeration never crashed, only what came after it.
fn on_warp() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .or_else(|_| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: true,
            compatible_surface: None,
        }))
    });
    let Ok(adapter) = adapter else {
        return false;
    };
    let info = adapter.get_info();
    if info.backend == wgpu::Backend::Dx12 && info.device_type == wgpu::DeviceType::Cpu {
        eprintln!("skipping: the only adapter is {} (WARP)", info.name);
        return true;
    }
    false
}

fn fixture(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn metrics() -> PageMetrics {
    PageMetrics {
        size: Size::new(600.0, 800.0),
        margins: EdgeSizes::uniform(40.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    }
}

/// The vendored fixture faces, all three axes pinned, so this suite means
/// the same thing on Linux, on a Mac and on a device. Taking the host's
/// fonts is what pinned two of these assertions to one machine's
/// collection.
fn fixture_fonts() -> chapbook_core::FontSource {
    chapbook_core::FontSource::embedded(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fonts"),
        "Crimson Text",
    )
}

/// A session over a per-test library dir, so tests don't share state.
fn session(name: &str, source: &str) -> Session {
    let dir =
        std::env::temp_dir().join(format!("chapbook-vello-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Session::open_with(
        source,
        SessionConfig::new(fixture_fonts()).with_library_dir(&dir),
    )
    .unwrap()
}

/// `None` when the machine has no adapter at all, or only WARP — the test
/// then skips.
fn renderer() -> Option<VelloRenderer> {
    if on_warp() {
        return None;
    }
    match VelloRenderer::new() {
        Ok(renderer) => Some(renderer),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

/// Mean luminance per cell of a `DIVISIONS`-square grid, 0.0–1.0.
const DIVISIONS: usize = 24;

fn coverage(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<f32> {
    let mut cells = vec![0.0f32; DIVISIONS * DIVISIONS];
    let mut counts = vec![0u32; DIVISIONS * DIVISIONS];
    for y in 0..height {
        let row = (y as usize * DIVISIONS / height.max(1) as usize).min(DIVISIONS - 1);
        for x in 0..width {
            let col = (x as usize * DIVISIONS / width.max(1) as usize).min(DIVISIONS - 1);
            let px = pixel(x, y);
            let lum =
                0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]);
            cells[row * DIVISIONS + col] += lum / 255.0;
            counts[row * DIVISIONS + col] += 1;
        }
    }
    for (cell, count) in cells.iter_mut().zip(&counts) {
        *cell /= (*count).max(1) as f32;
    }
    cells
}

/// Darkness summed along one axis: a profile whose shape says where ink
/// sits, independent of how much antialiasing each rasterizer spreads
/// around an edge.
fn profile(width: u32, height: u32, by_row: bool, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<f32> {
    let mut out = vec![0.0f32; if by_row { height } else { width } as usize];
    for y in 0..height {
        for x in 0..width {
            let px = pixel(x, y);
            let lum =
                (0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]))
                    / 255.0;
            out[if by_row { y } else { x } as usize] += 1.0 - lum;
        }
    }
    let total: f32 = out.iter().sum();
    if total > 0.0 {
        for v in &mut out {
            *v /= total;
        }
    }
    out
}

/// The shift that best aligns two profiles, and how much better it is than
/// no shift at all. Total ink is normalized away first, so this measures
/// displacement and nothing else — which is exactly what can go wrong when
/// one backend hands another a coordinate.
fn best_shift(a: &[f32], b: &[f32]) -> (i32, f32) {
    const SEARCH: i32 = 6;
    let distance = |shift: i32| -> f32 {
        a.iter()
            .enumerate()
            .map(|(i, av)| {
                let j = i as i32 + shift;
                let bv = if j >= 0 && (j as usize) < b.len() {
                    b[j as usize]
                } else {
                    0.0
                };
                (av - bv).abs()
            })
            .sum()
    };
    let mut best = (0, distance(0));
    for shift in -SEARCH..=SEARCH {
        let d = distance(shift);
        if d < best.1 {
            best = (shift, d);
        }
    }
    best
}

/// Assert two renders agree: the same ink, in the same cells, and — the
/// sharp part — not displaced relative to each other.
fn assert_parity(cpu: &tiny_skia::Pixmap, gpu: &RenderedPage, what: &str) {
    let cpu_px = |x: u32, y: u32| {
        let px = cpu.pixel(x, y).unwrap();
        [px.red(), px.green(), px.blue(), px.alpha()]
    };
    let gpu_px = |x: u32, y: u32| gpu.pixel(x, y).unwrap();

    for (axis, by_row) in [("vertically", true), ("horizontally", false)] {
        let a = profile(cpu.width(), cpu.height(), by_row, cpu_px);
        let b = profile(gpu.width, gpu.height, by_row, gpu_px);
        assert!(
            a.iter().sum::<f32>() > 0.5,
            "{what}: the cpu page is blank, so anything would agree"
        );
        let (shift, _) = best_shift(&a, &b);
        assert!(
            shift.abs() <= 1,
            "{what}: the gpu page is displaced {shift} px {axis} \u{2014} a coordinate \
             is being handled differently, not just antialiased differently"
        );
    }

    // Antialiasing changes how much ink there is, a little. It should not
    // change it much.
    let ink = |w: u32, h: u32, px: &dyn Fn(u32, u32) -> [u8; 4]| -> f32 {
        let mut total = 0.0;
        for y in 0..h {
            for x in 0..w {
                let p = px(x, y);
                let lum = (0.2126 * f32::from(p[0])
                    + 0.7152 * f32::from(p[1])
                    + 0.0722 * f32::from(p[2]))
                    / 255.0;
                total += 1.0 - lum;
            }
        }
        total / (w * h) as f32
    };
    let cpu_ink = ink(cpu.width(), cpu.height(), &cpu_px);
    let gpu_ink = ink(gpu.width, gpu.height, &gpu_px);
    let ratio = gpu_ink / cpu_ink.max(1e-6);
    assert!(
        (0.85..=1.15).contains(&ratio),
        "{what}: ink differs by {:.1}% (cpu {cpu_ink:.4}, gpu {gpu_ink:.4})",
        (ratio - 1.0) * 100.0
    );

    let (mean, worst, cell) = compare(cpu, gpu);
    assert!(
        mean < 0.02,
        "{what}: backends disagree about where ink lands: mean {mean:.4}, worst {worst:.4} at cell {cell}"
    );
    assert!(
        worst < 0.16,
        "{what}: one region diverges: worst {worst:.4} at cell {cell} (mean {mean:.4})"
    );
}

fn compare(cpu: &tiny_skia::Pixmap, gpu: &RenderedPage) -> (f32, f32, usize) {
    let cpu_cells = coverage(cpu.width(), cpu.height(), |x, y| {
        let px = cpu.pixel(x, y).unwrap();
        [px.red(), px.green(), px.blue(), px.alpha()]
    });
    let gpu_cells = coverage(gpu.width, gpu.height, |x, y| gpu.pixel(x, y).unwrap());

    let mut worst = 0.0f32;
    let mut total = 0.0f32;
    let mut worst_at = 0;
    for (i, (a, b)) in cpu_cells.iter().zip(&gpu_cells).enumerate() {
        let d = (a - b).abs();
        total += d;
        if d > worst {
            worst = d;
            worst_at = i;
        }
    }
    (total / cpu_cells.len() as f32, worst, worst_at)
}

use chapbook_reader::tiny_skia;

#[test]
fn a_text_page_lands_in_the_same_places_on_both_backends() {
    let Some(mut vello) = renderer() else { return };

    let mut s = session("text", &fixture("epub/illustrated.epub"));
    s.set_metrics(metrics());
    let cpu = s.render().expect("cpu render");

    let frame = s.frame().expect("frame");
    let (fonts, images) = s.paint_resources();
    let gpu = vello
        .render(&frame.list, fonts, images, 1.0)
        .expect("gpu render");

    assert_eq!((gpu.width, gpu.height), (cpu.width(), cpu.height()));

    // The page ground: both backends must agree exactly on a flat fill.
    let corner_cpu = cpu.pixel(2, 2).unwrap();
    let corner_gpu = gpu.pixel(2, 2).unwrap();
    assert_eq!(
        [corner_cpu.red(), corner_cpu.green(), corner_cpu.blue()],
        [corner_gpu[0], corner_gpu[1], corner_gpu[2]],
        "the page ground is a flat fill and should match exactly"
    );

    assert_parity(&cpu, &gpu, "text page");
}

#[test]
fn an_image_page_lands_in_the_same_places_on_both_backends() {
    let Some(mut vello) = renderer() else { return };

    let mut s = session("comic", &fixture("cbz/minimal.cbz"));
    s.set_metrics(metrics());
    // Drive the loader: an image unit is a placeholder until it arrives.
    for _ in 0..200 {
        s.render();
        if !s.has_pending_loads() {
            break;
        }
        s.poll_loaded();
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    s.poll_loaded();
    let cpu = s.render().expect("cpu render");

    let frame = s.frame().expect("frame");
    let (fonts, images) = s.paint_resources();
    let gpu = vello
        .render(&frame.list, fonts, images, 1.0)
        .expect("gpu render");

    assert_parity(&cpu, &gpu, "image page");
}

#[test]
fn hidpi_scales_the_scene_rather_than_the_pixels() {
    let Some(mut vello) = renderer() else { return };

    let mut s = session("hidpi", &fixture("epub/illustrated.epub"));
    s.set_metrics(PageMetrics {
        dpi_scale: 2.0,
        ..metrics()
    });
    let cpu = s.render().expect("cpu render");
    assert_eq!((cpu.width(), cpu.height()), (1200, 1600));

    let frame = s.frame().expect("frame");
    let (fonts, images) = s.paint_resources();
    let gpu = vello
        .render(&frame.list, fonts, images, 2.0)
        .expect("gpu render");
    assert_eq!((gpu.width, gpu.height), (1200, 1600));

    assert_parity(&cpu, &gpu, "hidpi page");
}
