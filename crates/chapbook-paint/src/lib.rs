//! The format-neutral page model and paint-neutral display list — the
//! contract every rasterization backend builds against (tiny-skia on the
//! CPU, vello on the GPU), and the seam where non-text formats join.
//!
//! `Page`/`Fragment` are produced by format-specific layout (chapbook-layout
//! for XHTML; image-per-page formats directly), and the `DisplayList`
//! flattens them into draw ops a backend consumes.

mod display;
mod images;
mod page;
mod panel;

pub use display::{build_display_list, DisplayList, DisplayOp, Frame, FrameIntent, Selection};
pub use images::{ImageFilter, ImageStore, StoredImage};
pub use page::{
    image_page, BoxDecoration, Decoration, Fragment, FragmentKind, Glyph, GlyphRun, LineFragment,
    Page,
};
pub use panel::{panel_rect, rotate};
