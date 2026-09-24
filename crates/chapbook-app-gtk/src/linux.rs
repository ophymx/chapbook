//! The application proper, compiled on Linux only.
//!
//! Thin by design: which books the shelf shows, how a session is
//! configured, how sync runs and what its reports say all live in
//! `chapbook-app`. This file turns those answers into GTK widgets and GTK
//! events into those questions — a `Stack` with a shelf page and a reader
//! page, a header bar that swaps between them, and the reference viewer's
//! reader wiring (keys through `KeyMap`, taps through `TapZones`,
//! press-drag selection, the loader waker) copied over a session slot that
//! empties when the reader goes back to the shelf.
//!
//! The one deliberate leak: the `Shell` holds its widgets strongly and the
//! widgets' callbacks hold the `Shell`, a cycle that lives exactly as long
//! as the application. Granular weak references would buy nothing here but
//! plumbing — there is one window, and closing it exits.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk::cairo;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use chapbook_app::chapbook_library::{BookId, BookRecord, ReadingState, Sort};
use chapbook_app::chapbook_reader::chapbook_core::{
    self, Action, ActionOutcome, BookKind, EdgeSizes, Key, KeyMap, PageMetrics, Rotation, Size,
    TapZones,
};
use chapbook_app::chapbook_reader::{SessionEvent, SettingsScope};
use chapbook_app::{App, Opened, ShelfFilter, SyncStatus};

use crate::page_area::{PageArea, SessionSlot};

pub fn run() -> glib::ExitCode {
    // The engine reports through `log`; this shell is the app and may print.
    chapbook_core::log_to_stderr();
    let model = match App::desktop(None) {
        Ok(model) => model,
        Err(e) => {
            eprintln!("chapbook-app-gtk: {e}");
            return glib::ExitCode::from(1);
        }
    };
    let model = Rc::new(RefCell::new(model));

    let app = gtk::Application::builder()
        .application_id("com.ophymx.chapbook.app")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| build_ui(app, model.clone()));
    app.run_with_args::<&str>(&[])
}

/// Everything the callbacks share. Model state on the left, widgets on
/// the right; the methods below are the only places both meet.
struct Shell {
    model: Rc<RefCell<App>>,
    filter: RefCell<ShelfFilter>,
    session: SessionSlot,
    /// Shelf row index → book id, rebuilt with the list.
    rows: RefCell<Vec<BookId>>,
    /// Rebuilt per book: the zones read the book's direction.
    zones: RefCell<TapZones>,
    /// Nudges the wake loop from the loader and sync threads.
    waker: Arc<dyn Fn() + Send + Sync>,
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    list: gtk::ListBox,
    status: gtk::Label,
    back: gtk::Button,
    area: PageArea,
    /// The reader's own chrome, shown only while a book is open.
    nav_menu: gtk::MenuButton,
    settings_menu: gtk::MenuButton,
    marks_menu: gtk::MenuButton,
    /// The open book's contents, flattened, row index aligned.
    toc: RefCell<Vec<chapbook_app::chapbook_reader::chapbook_core::TocEntry>>,
    toc_list: gtk::ListBox,
    marks_box: gtk::Box,
    theme_drop: gtk::DropDown,
    family_drop: gtk::DropDown,
    /// True while code sets the dropdowns, so their notify handlers know
    /// a change came from the session rather than the reader's hand.
    syncing: Cell<bool>,
}

fn build_ui(app: &gtk::Application, model: Rc<RefCell<App>>) {
    follow_system_color_scheme();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Chapbook")
        .default_width(900)
        .default_height(700)
        .build();

    let header = gtk::HeaderBar::new();
    let back = gtk::Button::from_icon_name("go-previous-symbolic");
    back.set_tooltip_text(Some("Back to the shelf"));
    back.set_visible(false);
    let import = gtk::Button::with_label("Import…");
    let sync = gtk::Button::with_label("Sync");
    header.pack_start(&back);
    header.pack_end(&sync);
    header.pack_end(&import);

    // ---- Reader chrome: contents, settings, marks ----
    //
    // Widgets only; every decision they surface — what a theme is, what
    // the contents are, the wording of a mark — comes from the model or
    // the session. Hidden until a book is open.
    let nav_menu = gtk::MenuButton::new();
    nav_menu.set_icon_name("view-list-symbolic");
    nav_menu.set_tooltip_text(Some("Contents"));
    nav_menu.set_visible(false);
    let toc_list = gtk::ListBox::new();
    toc_list.set_selection_mode(gtk::SelectionMode::None);
    {
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&toc_list));
        scroll.set_propagate_natural_width(true);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(500);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&scroll));
        nav_menu.set_popover(Some(&popover));
    }

    let settings_menu = gtk::MenuButton::new();
    settings_menu.set_label("Aa");
    settings_menu.set_tooltip_text(Some("Reading settings"));
    settings_menu.set_visible(false);
    let theme_drop =
        gtk::DropDown::from_strings(&chapbook_app::theme_names().map(|(name, _)| name));
    let family_drop = gtk::DropDown::from_strings(&["Publisher\u{2019}s default"]);
    let (smaller, larger) = (
        gtk::Button::with_label("A\u{2212}"),
        gtk::Button::with_label("A+"),
    );
    {
        let size = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        size.set_homogeneous(true);
        size.append(&smaller);
        size.append(&larger);
        let grid = gtk::Box::new(gtk::Orientation::Vertical, 8);
        grid.set_margin_start(10);
        grid.set_margin_end(10);
        grid.set_margin_top(10);
        grid.set_margin_bottom(10);
        grid.append(&size);
        let theme_label = gtk::Label::new(Some("Theme"));
        theme_label.set_xalign(0.0);
        theme_label.add_css_class("dim-label");
        grid.append(&theme_label);
        grid.append(&theme_drop);
        let family_label = gtk::Label::new(Some("Typeface"));
        family_label.set_xalign(0.0);
        family_label.add_css_class("dim-label");
        grid.append(&family_label);
        grid.append(&family_drop);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&grid));
        settings_menu.set_popover(Some(&popover));
    }

    let marks_menu = gtk::MenuButton::new();
    marks_menu.set_icon_name("user-bookmarks-symbolic");
    marks_menu.set_tooltip_text(Some("Bookmarks and marks"));
    marks_menu.set_visible(false);
    let marks_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    marks_box.set_margin_start(8);
    marks_box.set_margin_end(8);
    marks_box.set_margin_top(8);
    marks_box.set_margin_bottom(8);
    {
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&marks_box));
        scroll.set_propagate_natural_width(true);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(500);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&scroll));
        marks_menu.set_popover(Some(&popover));
    }

    header.pack_end(&marks_menu);
    header.pack_end(&settings_menu);
    header.pack_end(&nav_menu);
    window.set_titlebar(Some(&header));

    // ---- Shelf page ----
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search the shelf"));
    search.set_hexpand(true);
    let sort = gtk::DropDown::from_strings(&[
        "Recently read",
        "Recently added",
        "Title",
        "Author",
        "Series",
    ]);
    let state = gtk::DropDown::from_strings(&["All", "Reading", "Unread", "Finished"]);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(8);
    bar.set_margin_end(8);
    bar.set_margin_top(8);
    bar.append(&search);
    bar.append(&sort);
    bar.append(&state);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_margin_start(8);
    status.set_margin_end(8);
    status.set_margin_bottom(4);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.add_css_class("dim-label");

    let shelf_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    shelf_box.append(&bar);
    shelf_box.append(&scroll);
    shelf_box.append(&status);

    // ---- Reader page ----
    let session: SessionSlot = Rc::new(RefCell::new(None));
    let area = PageArea::new(session.clone());
    area.set_hexpand(true);
    area.set_vexpand(true);

    let stack = gtk::Stack::new();
    stack.add_named(&shelf_box, Some("shelf"));
    stack.add_named(&area, Some("reader"));
    window.set_child(Some(&stack));

    // One waker serves the loader and the sync driver: a wake means "come
    // drain everything", and the wake loop below does.
    let (wake_tx, mut wake_rx) = futures_channel::mpsc::unbounded::<()>();
    let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        // Failure means the receiver is gone, which means the window is
        // closing. Nothing to do about it and nothing to say.
        let _ = wake_tx.unbounded_send(());
    });

    let shell = Rc::new(Shell {
        model,
        filter: RefCell::new(ShelfFilter::default()),
        session,
        rows: RefCell::new(Vec::new()),
        zones: RefCell::new(TapZones::default()),
        waker,
        window: window.clone(),
        stack,
        list: list.clone(),
        status,
        back: back.clone(),
        area: area.clone(),
        nav_menu: nav_menu.clone(),
        settings_menu: settings_menu.clone(),
        marks_menu: marks_menu.clone(),
        toc: RefCell::new(Vec::new()),
        toc_list: toc_list.clone(),
        marks_box: marks_box.clone(),
        theme_drop: theme_drop.clone(),
        family_drop: family_drop.clone(),
        syncing: Cell::new(false),
    });

    // ---- Reader chrome wiring ----
    {
        // Contents: a row per flattened entry; a click jumps and closes.
        let shell = shell.clone();
        toc_list.connect_row_activated(move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            let entry = shell.toc.borrow().get(index as usize).cloned();
            let Some(entry) = entry else { return };
            let moved = shell
                .session
                .borrow_mut()
                .as_mut()
                .is_some_and(|s| s.goto_toc(&entry));
            if moved {
                shell.area.queue_draw();
            }
            if let Some(popover) = shell.nav_menu.popover() {
                popover.popdown();
            }
        });
    }
    {
        let shell = shell.clone();
        smaller.connect_clicked(move |_| shell.apply_reader_action(Action::FontDown));
    }
    {
        let shell = shell.clone();
        larger.connect_clicked(move |_| shell.apply_reader_action(Action::FontUp));
    }
    {
        // The dropdowns speak both ways: the session sets them when the
        // popover opens, the reader sets the session when they change.
        // `syncing` is what keeps those two from echoing.
        let shell = shell.clone();
        if let Some(popover) = settings_menu.popover() {
            popover.connect_show(move |_| shell.sync_settings_widgets());
        }
    }
    {
        let shell = shell.clone();
        theme_drop.connect_selected_notify(move |dropdown| {
            if shell.syncing.get() {
                return;
            }
            let Some((_, theme)) = chapbook_app::theme_names()
                .into_iter()
                .nth(dropdown.selected() as usize)
            else {
                return;
            };
            {
                let mut slot = shell.session.borrow_mut();
                if let Some(s) = slot.as_mut() {
                    let settings = chapbook_app::chapbook_reader::chapbook_core::ReadingSettings {
                        theme,
                        ..s.settings().clone()
                    };
                    s.set_settings(settings, SettingsScope::Global);
                }
            }
            shell.area.queue_draw();
        });
    }
    {
        let shell = shell.clone();
        family_drop.connect_selected_notify(move |dropdown| {
            if shell.syncing.get() {
                return;
            }
            let selected = dropdown.selected();
            let family = if selected == 0 {
                None
            } else {
                dropdown
                    .model()
                    .and_then(|m| m.item(selected))
                    .and_then(|item| item.downcast::<gtk::StringObject>().ok())
                    .map(|s| s.string().to_string())
            };
            {
                let mut slot = shell.session.borrow_mut();
                if let Some(s) = slot.as_mut() {
                    s.set_font_family(family, SettingsScope::Global);
                }
            }
            shell.area.queue_draw();
        });
    }
    {
        let shell = shell.clone();
        if let Some(popover) = marks_menu.popover() {
            popover.connect_show(move |_| shell.rebuild_marks());
        }
    }

    // Whatever the menus were, the page is what a reader is looking at:
    // hand the focus back when one closes, which is also what puts a
    // screen reader's locus back on the text.
    for menu in [&nav_menu, &settings_menu, &marks_menu] {
        if let Some(popover) = menu.popover() {
            let shell = shell.clone();
            popover.connect_closed(move |_| {
                if shell.stack.visible_child_name().as_deref() == Some("reader") {
                    shell.area.grab_focus();
                }
            });
        }
    }

    // ---- Shelf interactions ----
    {
        let shell = shell.clone();
        search.connect_search_changed(move |entry| {
            shell.filter.borrow_mut().search = entry.text().to_string();
            shell.refresh_shelf();
        });
    }
    {
        let shell = shell.clone();
        const SORTS: [Sort; 5] = [
            Sort::Read,
            Sort::Added,
            Sort::Title,
            Sort::Author,
            Sort::Series,
        ];
        sort.connect_selected_notify(move |sort| {
            let chosen = SORTS[(sort.selected() as usize).min(SORTS.len() - 1)];
            shell.filter.borrow_mut().sort = chosen;
            shell.refresh_shelf();
        });
    }
    {
        let shell = shell.clone();
        const STATES: [Option<ReadingState>; 4] = [
            None,
            Some(ReadingState::Reading),
            Some(ReadingState::Unread),
            Some(ReadingState::Finished),
        ];
        state.connect_selected_notify(move |state| {
            let chosen = STATES[(state.selected() as usize).min(STATES.len() - 1)];
            shell.filter.borrow_mut().state = chosen;
            shell.refresh_shelf();
        });
    }
    {
        let shell = shell.clone();
        list.connect_row_activated(move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            let Some(id) = shell.rows.borrow().get(index as usize).copied() else {
                return;
            };
            shell.open_book(id);
        });
    }
    {
        let shell = shell.clone();
        back.connect_clicked(move |_| shell.close_book());
    }
    {
        let shell = shell.clone();
        import.connect_clicked(move |_| shell.import_dialog());
    }
    {
        let shell = shell.clone();
        sync.connect_clicked(move |_| shell.start_sync());
    }

    // ---- Paint ----
    {
        let shell = shell.clone();
        area.set_draw_func(move |area, ctx, width, height| {
            if width <= 0 || height <= 0 {
                return;
            }
            let scale = area.scale_factor() as f32;
            let mut slot = shell.session.borrow_mut();
            let Some(s) = slot.as_mut() else { return };
            s.set_metrics(PageMetrics {
                size: Size::new(width as f32, height as f32),
                margins: EdgeSizes::uniform(40.0),
                dpi_scale: scale,
                rotation: Rotation::None,
            });
            let Some(pixmap) = s.render() else { return };
            let count = s.page_count().max(1);
            let title = format!(
                "{} — {} {}/{} p {}/{}",
                s.title(),
                match s.kind() {
                    BookKind::Epub => "ch",
                    BookKind::Comic | BookKind::Pdf => "pg",
                },
                s.spine() + 1,
                s.spine_len(),
                s.page() + 1,
                count,
            );
            drop(slot);

            // Premultiplied RGBA → cairo ARGB32 (BGRA in little-endian).
            let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
            let mut data = pixmap.take();
            for px in data.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            let stride = pw * 4;
            let Ok(surface) =
                cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, pw, ph, stride)
            else {
                return;
            };
            // The pixmap is device pixels; paint at 1/scale so it maps to
            // the widget's logical size.
            ctx.scale(1.0 / scale as f64, 1.0 / scale as f64);
            let _ = ctx.set_source_surface(&surface, 0.0, 0.0);
            let _ = ctx.paint();

            // Every content change funnels through a draw, so this is the
            // one place assistive technology and the title need to be
            // told — from an idle rather than inside the draw vfunc.
            // The title especially: setting it here would relayout the
            // header bar mid-snapshot, which GTK answers with a stream of
            // "snapshot without a current allocation" warnings.
            // page_changed itself no-ops unless the page's text moved.
            let page = shell.area.downgrade();
            let window = shell.window.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(window) = window.upgrade() {
                    if window.title().is_none_or(|now| now.as_str() != title) {
                        window.set_title(Some(&title));
                    }
                }
                if let Some(page) = page.upgrade() {
                    page.page_changed();
                }
            });
        });
    }

    // ---- Reader keyboard ----
    //
    // On the window in the capture phase, not on the page, and the
    // reason is what a focusable header bar does to arrow keys: a menu
    // button keeps the focus after its popover closes, and GTK then
    // spends Left and Right moving focus *between the buttons* instead
    // of turning pages. A reader whose page-turn keys stop working
    // because a menu was opened once is broken, and no amount of
    // refocusing the page fixes the case where somebody tabbed to a
    // button on purpose.
    //
    // Capturing at the window means these keys reach the engine
    // wherever the focus sits, so two guards keep it honest: the shelf
    // page is left alone entirely (its search entry owns every key it
    // gets), and an open popover is left alone too, because arrows in
    // the contents list are that list's to navigate.
    {
        let shell = shell.clone();
        let mut keys = KeyMap::default();
        // This shell's chrome is the header bar, which is always there;
        // nothing for `ToggleMenu` to do.
        keys.unbind(Key::Char('m'));
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if shell.stack.visible_child_name().as_deref() != Some("reader")
                || shell.a_popover_is_open()
            {
                return glib::Propagation::Proceed;
            }
            let name = keyval.name();
            let mut slot = shell.session.borrow_mut();
            let Some(s) = slot.as_mut() else {
                return glib::Propagation::Proceed;
            };
            // Shell-owned keys first — the clipboard, the annotation
            // store and leaving the reader belong to the app. Everything
            // the engine can do for itself falls through to the key map.
            let outcome = match name.as_deref() {
                Some("h") => {
                    // The stored highlight replaces the selection that
                    // made it.
                    s.add_highlight();
                    s.selection_clear();
                    ActionOutcome::Changed
                }
                Some("Escape") if s.selected_range().is_some() => {
                    s.selection_clear();
                    ActionOutcome::Changed
                }
                Some("c") => {
                    if let Some(text) = s.selected_text() {
                        shell.window.clipboard().set_text(&text);
                    }
                    return glib::Propagation::Stop;
                }
                Some("q") | Some("Escape") => {
                    drop(slot);
                    shell.close_book();
                    return glib::Propagation::Stop;
                }
                name => match name.and_then(engine_key).and_then(|k| keys.action(k)) {
                    Some(action) => s.apply(action),
                    None => return glib::Propagation::Proceed,
                },
            };
            drop(slot);
            if outcome.needs_redraw() {
                shell.area.queue_draw();
            }
            if outcome.consumed() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key);
    }

    // ---- Links, selection and tap zones (press-drag) ----
    // ---- Double-click selects a word ----
    //
    // The desktop's spelling of the long press: `select_word_at` is what
    // a touch shell calls, and a mouse gets the same range so the mark
    // buttons in the chrome have something to act on without a drag.
    {
        let shell = shell.clone();
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |gesture, presses, x, y| {
            if presses != 2 {
                return;
            }
            let selected = shell
                .session
                .borrow_mut()
                .as_mut()
                .is_some_and(|s| s.select_word_at(x as f32, y as f32));
            if selected {
                // Claim it, or the drag gesture below sees the second
                // press as the start of a new empty selection.
                gesture.set_state(gtk::EventSequenceState::Claimed);
                shell.area.queue_draw();
            }
        });
        area.add_controller(click);
    }

    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        // Set when a press starts a selection rather than following a
        // link. A press that then never moves is a tap, and `drag_end`
        // asks the engine what a tap at that point means.
        let tap = Rc::new(Cell::new(false));
        {
            let shell = shell.clone();
            let tap = tap.clone();
            drag.connect_drag_begin(move |_, x, y| {
                let mut slot = shell.session.borrow_mut();
                let Some(s) = slot.as_mut() else { return };
                tap.set(false);
                // A press on a link follows it rather than starting a
                // selection there.
                if let Some(href) = s.link_at(x as f32, y as f32) {
                    if s.follow_link(&href) {
                        drop(slot);
                        shell.area.queue_draw();
                        return;
                    }
                }
                s.selection_begin(x as f32, y as f32);
                tap.set(true);
                drop(slot);
                shell.area.queue_draw();
            });
        }
        {
            let shell = shell.clone();
            drag.connect_drag_update(move |gesture, dx, dy| {
                if let Some((sx, sy)) = gesture.start_point() {
                    if let Some(s) = shell.session.borrow_mut().as_mut() {
                        s.selection_drag((sx + dx) as f32, (sy + dy) as f32);
                    }
                    shell.area.queue_draw();
                }
            });
        }
        {
            let shell = shell.clone();
            drag.connect_drag_end(move |gesture, dx, dy| {
                // A press that wandered was a selection and the drag
                // handlers already have it. The slop is a mouse's.
                const SLOP: f64 = 4.0;
                if !tap.replace(false) || dx.abs() > SLOP || dy.abs() > SLOP {
                    return;
                }
                let Some((x, y)) = gesture.start_point() else {
                    return;
                };
                let mut slot = shell.session.borrow_mut();
                let Some(s) = slot.as_mut() else { return };
                if s.selected_range().is_some() {
                    return;
                }
                // The press anchored an empty selection; drop it before
                // turning, so the anchor does not outlive the page.
                s.selection_clear();
                let Some(metrics) = s.metrics() else { return };
                let Some(action) = shell.zones.borrow().action_at(x as f32, y as f32, &metrics)
                else {
                    return;
                };
                let outcome = s.apply(action);
                drop(slot);
                if outcome.needs_redraw() {
                    shell.area.queue_draw();
                }
            });
        }
        area.add_controller(drag);
    }

    // ---- The wake loop: loader landings and sync reports ----
    //
    // Same shape as the reference viewer's: senders cross threads, the
    // receiving half runs on the main context where it may touch the
    // session, the model and the widgets.
    {
        let shell = shell.clone();
        glib::spawn_future_local(async move {
            use futures_util::StreamExt;
            while wake_rx.next().await.is_some() {
                let mut redraw = false;
                let mut lines = Vec::new();
                if let Some(s) = shell.session.borrow_mut().as_mut() {
                    redraw = s.poll_loaded();
                    for event in s.drain_events() {
                        if let SessionEvent::UnitFailed { spine, message } = event {
                            lines.push(format!("page {} will not load: {message}", spine + 1));
                        }
                    }
                }
                let mut synced = false;
                {
                    let model = shell.model.borrow();
                    for report in model.sync_events() {
                        if matches!(report, SyncStatus::Finished { .. }) {
                            synced = true;
                        }
                        lines.push(model.describe_sync(&report));
                    }
                }
                if let Some(line) = lines.last() {
                    shell.status.set_text(line);
                }
                if synced {
                    // Positions and marks may have moved under the shelf.
                    shell.refresh_shelf();
                }
                if redraw {
                    shell.area.queue_draw();
                }
            }
        });
    }

    // ---- Persistence on close ----
    {
        let shell = shell.clone();
        window.connect_close_request(move |_| {
            if let Some(s) = shell.session.borrow_mut().as_mut() {
                s.save_position();
            }
            glib::Propagation::Proceed
        });
    }

    shell.refresh_shelf();
    window.present();
    search.grab_focus();
}

impl Shell {
    fn set_status(&self, line: &str) {
        self.status.set_text(line);
    }

    /// Rebuild the shelf list from the model, under the current filter.
    fn refresh_shelf(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        // Bound, not matched on directly — see import_dialog.
        let shelved = self.model.borrow().shelf(&self.filter.borrow());
        let records = match shelved {
            Ok(records) => records,
            Err(e) => {
                self.set_status(&format!("shelf: {e}"));
                return;
            }
        };
        let mut rows = self.rows.borrow_mut();
        rows.clear();
        if records.is_empty() {
            // Distinguish "nothing here" from "nothing matched": the
            // second is a filter to relax.
            let filter = self.filter.borrow();
            let narrowed = !filter.search.trim().is_empty() || filter.state.is_some();
            let label = gtk::Label::new(Some(if narrowed {
                "nothing on the shelf matches"
            } else {
                "the library is empty — Import… adds a book"
            }));
            label.add_css_class("dim-label");
            label.set_margin_top(24);
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);
            row.set_child(Some(&label));
            self.list.append(&row);
            return;
        }
        for record in &records {
            rows.push(record.id);
            self.list.append(&self.shelf_row(record));
        }
    }

    /// Put a shelf book on the reader page.
    fn open_book(&self, id: BookId) {
        // Bound, not matched on directly — see import_dialog.
        let opened = self.model.borrow().open_book(id);
        let mut session = match opened {
            Ok(Opened::Session(session)) => *session,
            // This desktop only imports; a row the platform owns arrived
            // some other way, and there is no grant here to resolve.
            Ok(Opened::Adopted { .. }) => {
                self.set_status(
                    "open: this book is adopted, and the desktop has no way to reach it",
                );
                return;
            }
            Ok(Opened::Missing) => {
                self.set_status("open: the book's file is gone from the library");
                return;
            }
            Err(e) => {
                self.set_status(&format!("open: {e}"));
                return;
            }
        };
        let waker = self.waker.clone();
        session.set_waker(move || waker());
        // Thirds with an inert middle — the header bar is always there,
        // so the band that would toggle a menu is bound to nothing. The
        // direction comes from the book.
        *self.zones.borrow_mut() = TapZones {
            middle: None,
            ..TapZones::new(session.reading_direction())
        };
        self.window.set_title(Some(session.title()));
        // The contents are fixed per book: flatten once, rows aligned to
        // the stored entries by index.
        let toc = chapbook_app::flatten_toc(session.toc());
        while let Some(child) = self.toc_list.first_child() {
            self.toc_list.remove(&child);
        }
        for (depth, entry) in &toc {
            let label = gtk::Label::new(Some(&entry.label));
            label.set_xalign(0.0);
            label.set_margin_start(8 + (*depth as i32) * 14);
            label.set_margin_end(8);
            label.set_margin_top(4);
            label.set_margin_bottom(4);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label));
            self.toc_list.append(&row);
        }
        *self.toc.borrow_mut() = toc.into_iter().map(|(_, entry)| entry).collect();
        *self.session.borrow_mut() = Some(session);
        self.back.set_visible(true);
        self.nav_menu.set_visible(true);
        self.settings_menu.set_visible(true);
        self.marks_menu.set_visible(true);
        self.stack.set_visible_child_name("reader");
        self.area.grab_focus();
        self.area.queue_draw();
    }

    /// Back to the shelf. Dropping the session joins its loader, so by
    /// the time the shelf redraws nothing is still fetching behind it.
    fn close_book(self: &Rc<Self>) {
        if let Some(mut session) = self.session.borrow_mut().take() {
            session.save_position();
        }
        self.window.set_title(Some("Chapbook"));
        self.back.set_visible(false);
        self.nav_menu.set_visible(false);
        self.settings_menu.set_visible(false);
        self.marks_menu.set_visible(false);
        self.stack.set_visible_child_name("shelf");
        // The position (and maybe a finished flag) just moved under the
        // shelf; and with the slot empty, retract the announced page.
        self.refresh_shelf();
        self.area.page_changed();
    }

    fn import_dialog(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Books (EPUB, CBZ, PDF)"));
        for suffix in ["epub", "cbz", "pdf"] {
            filter.add_suffix(suffix);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import a book")
            .filters(&filters)
            .build();
        let shell = self.clone();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return }; // cancelled
            let Some(path) = file.path() else { return };
            // Bound before the match, never matched on directly: a borrow
            // in a match scrutinee lives through every arm, and the Ok arm
            // re-borrows the model inside refresh_shelf.
            let imported = shell.model.borrow_mut().import(&path);
            match imported {
                Ok(record) => {
                    shell.set_status(&format!("imported \u{201c}{}\u{201d}", record.title));
                    shell.refresh_shelf();
                }
                Err(e) => shell.set_status(&format!("import: {e}")),
            }
        });
    }

    /// Whether any chrome popover is on screen — the keys are its own
    /// while it is, so a contents list can be arrowed through.
    fn a_popover_is_open(&self) -> bool {
        [&self.nav_menu, &self.settings_menu, &self.marks_menu]
            .into_iter()
            .filter_map(|menu| menu.popover())
            .any(|popover| popover.is_visible())
    }

    /// One engine action from a chrome button — the same door the keys
    /// use, so a button and a keystroke can never disagree.
    fn apply_reader_action(&self, action: Action) {
        let outcome = {
            let mut slot = self.session.borrow_mut();
            let Some(s) = slot.as_mut() else { return };
            s.apply(action)
        };
        if outcome.needs_redraw() {
            self.area.queue_draw();
        }
    }

    /// Point the settings popover's widgets at what the session holds —
    /// called as the popover opens, under the `syncing` flag so the
    /// notify handlers know nobody's hand is on the dial.
    fn sync_settings_widgets(&self) {
        let slot = self.session.borrow();
        let Some(s) = slot.as_ref() else { return };
        self.syncing.set(true);
        let theme = s.settings().theme;
        if let Some(index) = chapbook_app::theme_names()
            .iter()
            .position(|(_, t)| *t == theme)
        {
            self.theme_drop.set_selected(index as u32);
        }
        // The family list is the session's own font database, headed by
        // the publisher's default.
        let mut names = vec!["Publisher\u{2019}s default".to_string()];
        names.extend(s.font_families());
        let current = s.settings().font_family.clone();
        let selected = current
            .as_deref()
            .and_then(|family| names.iter().position(|n| n == family))
            .unwrap_or(0);
        let strs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.family_drop
            .set_model(Some(&gtk::StringList::new(&strs)));
        self.family_drop.set_selected(selected as u32);
        self.syncing.set(false);
    }

    /// Rebuild the marks popover: the actions at the top, then every
    /// mark the book carries, in reading order.
    fn rebuild_marks(self: &Rc<Self>) {
        while let Some(child) = self.marks_box.first_child() {
            self.marks_box.remove(&child);
        }
        let bookmark = gtk::Button::with_label("Bookmark this page");
        bookmark.set_has_frame(false);
        {
            let shell = self.clone();
            bookmark.connect_clicked(move |_| {
                let added = shell
                    .session
                    .borrow_mut()
                    .as_mut()
                    .and_then(|s| s.add_bookmark());
                shell.set_status(if added.is_some() {
                    "bookmarked"
                } else {
                    "nothing to bookmark yet"
                });
                if let Some(popover) = shell.marks_menu.popover() {
                    popover.popdown();
                }
            });
        }
        self.marks_box.append(&bookmark);

        let has_selection = self
            .session
            .borrow()
            .as_ref()
            .and_then(|s| s.selected_range())
            .is_some();
        let highlight = gtk::Button::with_label("Highlight the selection");
        highlight.set_has_frame(false);
        highlight.set_sensitive(has_selection);
        {
            let shell = self.clone();
            highlight.connect_clicked(move |_| {
                {
                    let mut slot = shell.session.borrow_mut();
                    if let Some(s) = slot.as_mut() {
                        if s.add_highlight().is_some() {
                            s.selection_clear();
                        }
                    }
                }
                shell.area.queue_draw();
                if let Some(popover) = shell.marks_menu.popover() {
                    popover.popdown();
                }
            });
        }
        self.marks_box.append(&highlight);

        let note = gtk::Button::with_label("Note on the selection\u{2026}");
        note.set_has_frame(false);
        note.set_sensitive(has_selection);
        {
            let shell = self.clone();
            note.connect_clicked(move |_| {
                if let Some(popover) = shell.marks_menu.popover() {
                    popover.popdown();
                }
                shell.open_note_dialog();
            });
        }
        self.marks_box.append(&note);

        let marks = self
            .session
            .borrow()
            .as_ref()
            .map(|s| s.annotations())
            .unwrap_or_default();
        if marks.is_empty() {
            let empty = gtk::Label::new(Some("no marks in this book yet"));
            empty.add_css_class("dim-label");
            empty.set_margin_top(6);
            self.marks_box.append(&empty);
            return;
        }
        self.marks_box
            .append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        for mark in marks {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let jump = gtk::Button::with_label(&chapbook_app::describe_annotation(&mark));
            jump.set_has_frame(false);
            jump.set_hexpand(true);
            if let Some(child) = jump.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                child.set_xalign(0.0);
                child.set_ellipsize(gtk::pango::EllipsizeMode::End);
                child.set_max_width_chars(48);
            }
            {
                let shell = self.clone();
                let id = mark.id;
                jump.connect_clicked(move |_| {
                    let moved = shell
                        .session
                        .borrow_mut()
                        .as_mut()
                        .is_some_and(|s| s.goto_annotation(id));
                    if moved {
                        shell.area.queue_draw();
                    }
                    if let Some(popover) = shell.marks_menu.popover() {
                        popover.popdown();
                    }
                });
            }
            line.append(&jump);
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.set_has_frame(false);
            remove.set_tooltip_text(Some("Remove this mark"));
            {
                let shell = self.clone();
                let id = mark.id;
                remove.connect_clicked(move |_| {
                    if let Some(s) = shell.session.borrow_mut().as_mut() {
                        s.remove_annotation(id);
                    }
                    shell.area.queue_draw();
                    shell.rebuild_marks();
                });
            }
            line.append(&remove);
            self.marks_box.append(&line);
        }
    }

    /// A small modal asking for the note's words; the selection it
    /// annotates is still live underneath.
    fn open_note_dialog(self: &Rc<Self>) {
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("The note\u{2026}"));
        entry.set_activates_default(true);
        let add = gtk::Button::with_label("Add note");
        add.add_css_class("suggested-action");
        let cancel = gtk::Button::with_label("Cancel");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&cancel);
        buttons.append(&add);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.append(&entry);
        content.append(&buttons);
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Note on the selection")
            .default_width(360)
            .build();
        dialog.set_child(Some(&content));
        dialog.set_default_widget(Some(&add));
        {
            let dialog = dialog.clone();
            cancel.connect_clicked(move |_| dialog.close());
        }
        {
            let shell = self.clone();
            let dialog = dialog.clone();
            let entry = entry.clone();
            add.connect_clicked(move |_| {
                let body = entry.text();
                let body = body.trim();
                if !body.is_empty() {
                    let added = {
                        let mut slot = shell.session.borrow_mut();
                        slot.as_mut().and_then(|s| {
                            let id = s.add_note(body);
                            if id.is_some() {
                                s.selection_clear();
                            }
                            id
                        })
                    };
                    shell.set_status(if added.is_some() {
                        "noted"
                    } else {
                        "select some text first"
                    });
                    shell.area.queue_draw();
                }
                dialog.close();
            });
        }
        dialog.present();
        entry.grab_focus();
    }

    fn start_sync(&self) {
        // Bound, not matched on directly — see import_dialog.
        let started = self.model.borrow_mut().sync_all(self.waker.clone());
        match started {
            Ok(true) => self.set_status("syncing…"),
            Ok(false) => self.set_status("no book on the shelf has a service to sync with"),
            Err(e) => self.set_status(&format!("sync: {e}")),
        }
    }

    /// One shelf row: cover if the book has one, title, a quiet line of
    /// everything else, and a menu for what a row can have done to it.
    fn shelf_row(self: &Rc<Self>, record: &BookRecord) -> gtk::ListBoxRow {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.set_margin_start(8);
        line.set_margin_end(8);
        line.set_margin_top(6);
        line.set_margin_bottom(6);
        if let Some(cover) = &record.cover_path {
            // An `Image` with a pixel size, not a `Picture`: a picture
            // asks for the file's natural size and a cover would swallow
            // the row.
            let thumb = gtk::Image::from_file(cover);
            thumb.set_pixel_size(56);
            line.append(&thumb);
        }
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(Some(&record.title));
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("heading");
        text.append(&title);
        let mut parts = Vec::new();
        if !record.authors.is_empty() {
            parts.push(record.authors.join(", "));
        }
        if let Some(series) = &record.series {
            parts.push(match record.series_index {
                Some(index) => format!("{series} #{}", trim_index(index)),
                None => series.clone(),
            });
        }
        parts.push(chapbook_app::describe_state(record));
        let subtitle = gtk::Label::new(Some(&parts.join(" · ")));
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.add_css_class("dim-label");
        text.append(&subtitle);
        line.append(&text);
        line.append(&self.row_menu(record));
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&line));
        row
    }

    /// The row's "⋮" menu. A button inside the row consumes its own
    /// clicks, so opening it never also opens the book.
    fn row_menu(self: &Rc<Self>, record: &BookRecord) -> gtk::MenuButton {
        let id = record.id;
        let finished = record.state() == ReadingState::Finished;

        let popover = gtk::Popover::new();
        let items = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // "Read" here is the finished flag, the same state `lib finish`
        // sets. Un-marking a book with a position shows it as "reading"
        // again, not "unread" — the shelf reports the position it still
        // holds, and the platform has no "forget my place" verb yet.
        let toggle = gtk::Button::with_label(if finished {
            "Mark as unread"
        } else {
            "Mark as read"
        });
        toggle.set_has_frame(false);
        {
            let shell = self.clone();
            let popover = popover.downgrade();
            toggle.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
                // Bound, not matched on directly — see import_dialog.
                let done = shell.model.borrow_mut().set_finished(id, !finished);
                if let Err(e) = done {
                    shell.set_status(&format!("shelf: {e}"));
                }
                shell.refresh_shelf();
            });
        }
        items.append(&toggle);

        let remove = gtk::Button::with_label("Remove from library");
        remove.set_has_frame(false);
        remove.add_css_class("destructive-action");
        {
            let shell = self.clone();
            let title = record.title.clone();
            let popover = popover.downgrade();
            remove.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
                shell.confirm_remove(id, &title);
            });
        }
        items.append(&remove);

        popover.set_child(Some(&items));
        let menu = gtk::MenuButton::new();
        menu.set_icon_name("view-more-symbolic");
        menu.set_has_frame(false);
        menu.set_valign(gtk::Align::Center);
        menu.set_tooltip_text(Some("Book actions"));
        menu.set_popover(Some(&popover));
        menu
    }

    /// Ask before a remove. The delete is soft — importing the same file
    /// again restores the row with its position and marks — and the
    /// dialog says as much rather than threatening more than happens.
    fn confirm_remove(self: &Rc<Self>, id: BookId, title: &str) {
        let dialog = gtk::AlertDialog::builder()
            .message(format!("Remove \u{201c}{title}\u{201d}?"))
            .detail(
                "It leaves the shelf. Importing the same file again brings \
                 back its reading position and marks.",
            )
            .buttons(["Cancel", "Remove"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let shell = self.clone();
        dialog.choose(Some(&self.window), gio::Cancellable::NONE, move |choice| {
            if choice != Ok(1) {
                return;
            }
            // Bound, not matched on directly — see import_dialog.
            let removed = shell.model.borrow_mut().remove(id);
            match removed {
                Ok(()) => shell.set_status("removed"),
                Err(e) => shell.set_status(&format!("remove: {e}")),
            }
            shell.refresh_shelf();
        });
    }
}

/// Follow the desktop's light/dark preference, now and when it changes.
///
/// Plain GTK4 never reads the portal's color-scheme on its own — that is
/// libadwaita's habit — so a stock-GTK app sits light on a dark desktop.
/// `org.freedesktop.appearance/color-scheme` is the cross-desktop answer
/// (1 means prefer dark); no portal, no change. Only the chrome follows:
/// the page keeps its own engine theme, because paper is a reading choice
/// and `t` cycles it.
fn follow_system_color_scheme() {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };

    // The portal wraps the value in a variant inside the reply's variant;
    // unwrap however deep it nests rather than memorizing which call
    // double-wraps.
    fn scheme_of(value: &glib::Variant) -> Option<u32> {
        let mut value = value.clone();
        while let Some(inner) = value.as_variant() {
            value = inner;
        }
        value.get::<u32>()
    }
    fn apply(dark: bool) {
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(dark);
        }
    }

    let reply = bus.call_sync(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
        "Read",
        Some(&("org.freedesktop.appearance", "color-scheme").into()),
        None,
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    );
    match reply {
        Ok(value) => apply(scheme_of(&value.child_value(0)) == Some(1)),
        // No settings portal is a desktop without a stated preference,
        // not an error worth a status line.
        Err(_) => return,
    }

    // The guard unsubscribes on drop, and this subscription is meant for
    // the life of the app — forgetting it is the deliberate leak. The
    // callback captures nothing GTK; it asks for the Settings object
    // fresh on its own (main) thread.
    let subscription = bus.subscribe_to_signal(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Settings"),
        Some("SettingChanged"),
        Some("/org/freedesktop/portal/desktop"),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            let params = signal.parameters;
            let (namespace, key) = (params.child_value(0), params.child_value(1));
            if namespace.get::<String>().as_deref() == Some("org.freedesktop.appearance")
                && key.get::<String>().as_deref() == Some("color-scheme")
            {
                apply(scheme_of(&params.child_value(2)) == Some(1));
            }
        },
    );
    std::mem::forget(subscription);
}

/// A series position without a trailing `.0` — "#3", not "#3.0", while a
/// genuine "#1.5" keeps its half.
fn trim_index(index: f64) -> String {
    if index.fract() == 0.0 {
        format!("{}", index as i64)
    } else {
        format!("{index}")
    }
}

/// A GDK keyval name in the engine's key vocabulary, or `None` for a key
/// no reader binds. What each key *does* is `KeyMap`'s answer, not this
/// function's — the split that lets a binding be written once instead of
/// once per shell.
fn engine_key(name: &str) -> Option<Key> {
    Some(match name {
        "Right" => Key::ArrowRight,
        "Left" => Key::ArrowLeft,
        "Up" => Key::ArrowUp,
        "Down" => Key::ArrowDown,
        "Page_Down" => Key::PageDown,
        "Page_Up" => Key::PageUp,
        "space" => Key::Space,
        "BackSpace" => Key::Backspace,
        // GDK names the punctuation; the map binds the character.
        "plus" => Key::Char('+'),
        "equal" => Key::Char('='),
        "minus" => Key::Char('-'),
        // Anything else is a key map's `Char`, if it is one character at
        // all — every other GDK name ("Shift_L", "F11") is several.
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c.to_ascii_lowercase()),
                _ => return None,
            }
        }
    })
}
