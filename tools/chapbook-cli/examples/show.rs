//! Read a book on a Linux framebuffer.
//!
//!     cargo run -p chapbook-cli --features fbdev --example show -- <book> [/dev/fb0] [turns]
//!
//! Deliberately written the way a device shell would be, rather than the
//! short way. It goes through `Session::frame` and rasterizes for itself,
//! so the whole seam is exercised: display list, panel policy, damage,
//! intent, and `PanelDriver`'s rules — not just `Session::render`, which
//! would hide all of it behind one call.
//!
//! The panel half of that seam is [mezzotint]; this is the chapbook half,
//! and it lives here because it is the only part of the pairing that knows
//! what a book is. mezzotint's own `bringup` example covers the panel
//! without one.
//!
//! Built with `--features hwtcon`, a framebuffer whose driver is `hwtcon`
//! (Kobo's MediaTek colour readers) is driven through mezzotint's e-ink
//! backend, so every page turn is a real controller update with a
//! waveform chosen from the frame's intent. Any other framebuffer is
//! driven bare, as before. Nothing above the panel changes between the
//! two: that is what the seam is for.
//!
//! A device rootfs rarely has `/usr/share/fonts`, so the host font source
//! finds nothing there and every glyph is tofu. `SHOW_FONT_DIR` and
//! `SHOW_FONT_FAMILY` together name a directory to scan and the one
//! family every CSS generic resolves to — a Kobo's Qt font directory and
//! "Vollkorn", say. A shell proper would build its `FontSource` field by
//! field; an example names the two things it needs.
//!
//! Needs write access to the framebuffer (`video` group, or root), and a
//! console that is not being redrawn underneath it — `chvt` to a free one,
//! or run it in a VM. mezzotint's `scripts/fbdev-vm.sh` boots one.

use std::path::PathBuf;
use std::time::Duration;

use chapbook_core::{EdgeSizes, FontSource, PageMetrics, Rotation, Size};
use chapbook_reader::{chapbook_paint, chapbook_render_tinyskia, tiny_skia, Session};
use mezzotint::fbdev::FbdevPanel;
use mezzotint::{Panel, PanelDriver, Source, Update};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(book) = args.next() else {
        eprintln!("usage: show <book.epub|comic.cbz|doc.pdf> [/dev/fb0] [turns]");
        std::process::exit(2);
    };
    let device = args.next().unwrap_or_else(|| "/dev/fb0".to_string());
    let turns: usize = match args.next() {
        Some(n) => n.parse()?,
        None => 3,
    };
    let fonts = fonts();

    #[cfg(feature = "hwtcon")]
    match mezzotint::hwtcon::HwtconPanel::open_path(&device) {
        Ok(panel) => {
            eprintln!("show: {device} — {}", panel.describe());
            return read(panel, &book, fonts, turns);
        }
        // Not an error: this build knows one controller, and this is not
        // it. The bare framebuffer still shows the book, just without
        // telling anything to refresh.
        Err(mezzotint::Error::UnexpectedDriver { .. }) => {
            eprintln!("show: {device} is not hwtcon, driving it as a bare framebuffer");
        }
        Err(err) => return Err(err.into()),
    }

    let panel = FbdevPanel::open_path(&device)?;
    // Say what was found, for the same reason the GPU backend names its
    // adapter: "it drew something" is not the same as "it drew it right".
    eprintln!("show: {device} — {}", panel.describe());
    read(panel, &book, fonts, turns)
}

/// The font source this run names, from the environment or the host.
fn fonts() -> FontSource {
    match (
        std::env::var_os("SHOW_FONT_DIR"),
        std::env::var("SHOW_FONT_FAMILY"),
    ) {
        (Some(dir), Ok(family)) => FontSource::embedded(PathBuf::from(dir), family),
        (None, Err(_)) => FontSource::host(),
        _ => {
            eprintln!("show: SHOW_FONT_DIR and SHOW_FONT_FAMILY are set together or not at all");
            std::process::exit(2);
        }
    }
}

/// Open the book on the panel and turn a few pages.
///
/// Generic over the panel because nothing here knows or cares which
/// controller is underneath: the frame's intent becomes an `UpdateClass`,
/// and what that means on the glass is the backend's business.
fn read<P: Panel>(
    panel: P,
    book: &str,
    fonts: FontSource,
    turns: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let info = panel.info();
    let mut driver = PanelDriver::new(panel);

    let mut session = Session::open(book, fonts)?;
    // The page is laid out in reading orientation; rotation happens on the
    // way to the panel, so an unrotated shell hands over its own size.
    let page = Size::new(info.width as f32, info.height as f32);
    let rotation = Rotation::None;
    session.set_metrics(PageMetrics {
        size: page,
        margins: EdgeSizes::uniform(32.0),
        dpi_scale: 1.0,
        rotation,
    });
    session.set_pixel_format(info.format);

    let mut renderer = chapbook_render_tinyskia::Renderer::new();
    let mut show = |session: &mut Session, driver: &mut PanelDriver<P>| {
        let Some(frame) = session.frame() else {
            return Ok(());
        };
        // Nothing changed, so there is nothing for the panel to do. On
        // e-ink that is a saving worth having: the cheapest update is the
        // one not submitted.
        let Some(class) = frame.intent.update_class() else {
            return Ok(());
        };
        let (w, h) = (page.w as u32, page.h as u32);
        let Some(mut pixmap) = tiny_skia::Pixmap::new(w, h) else {
            return Ok(());
        };
        let (fonts, images) = session.paint_resources();
        renderer.render(&frame.list, fonts, images, 1.0, &mut pixmap.as_mut());

        // Where dithering belongs — over images, not over text — is the
        // one part of this a shell cannot work out from the pixels it was
        // handed, and the display list is what knows.
        let dithered = frame.list.dither_regions(1.0);
        // Both halves of the reduction question, and exactly one of them
        // acts. A panel asking for `Grey` wants the reduction done above
        // it, which is this call; a panel asking for `Rgba` reduces at
        // submit, where it knows the class, and this is a no-op — so the
        // regions travel on the `Update` as well, for the backend that
        // will use them there.
        let whole = mezzotint::PanelRect::full(w, h);
        mezzotint::encode::quantize_for(pixmap.data_mut(), w, h, whole, info.format, &dithered);
        let rgba = chapbook_paint::rotate(pixmap.data(), w, h, rotation);

        let damage = frame
            .damage
            .map(|rect| chapbook_paint::panel_rect(rect, page, 1.0, rotation));
        let src = Source::panel(&rgba, info.width, info.height)?;
        let started = std::time::Instant::now();
        driver.present(src, damage, Update::new(class).dithering_within(&dithered))?;
        // Submission, not completion: the glass settles on its own clock.
        eprintln!(
            "show: {:?} at {:?}, {class:?}, submitted in {:?}",
            session.position(),
            session.locator(),
            started.elapsed()
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    show(&mut session, &mut driver)?;
    // A few page turns, so the damage and cadence paths get walked rather
    // than just the first full paint.
    for _ in 0..turns {
        std::thread::sleep(Duration::from_secs(2));
        // `next_page` answers "did it move" itself, which is the whole
        // reason it returns anything. Deriving it here is what broke this
        // loop the first time: it compared the page alone, and a turn off
        // the end of a unit crosses into the next one by resetting the
        // page to 0, so a successful move read as "did not move". Most
        // books open on a single-page cover, so it stopped on turn one.
        // `chapbook_reader::conformance` now asserts this for any shell.
        if !session.next_page() {
            break;
        }
        show(&mut session, &mut driver)?;
    }
    // A controller may still be writing the last page to the glass; a
    // shell owes this before it tears the panel down, and on a bare
    // framebuffer, where nothing is ever in flight, the habit should be
    // visible all the same.
    driver.flush()?;
    Ok(())
}
