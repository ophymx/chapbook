using System.Runtime.InteropServices;

namespace Chapbook;

/// <summary>
/// The C ABI, transcribed. Nothing above this file names a raw pointer.
/// </summary>
/// <remarks>
/// <para>
/// Every declaration here corresponds to one in
/// <c>crates/chapbook-ffi/include/chapbook.h</c>, which is the contract —
/// a checked-in cbindgen golden, guarded by a test, and the artifact this
/// binding compiles against in the only sense a P/Invoke can. Nothing
/// generates this file, because a generated binding would still have to be
/// read by whoever debugged it, and because the shapes below are small
/// enough to state.
/// </para>
/// <para>
/// Three rules of that ABI decide everything in this file.
/// <b>Codes are the contract and strings are not</b>, so every fallible
/// call returns <see cref="Status"/> and the human-readable half is
/// fetched separately. <b>Nothing crosses owned</b>, so there is no
/// free-string entry point and every string is written into a buffer the
/// caller sized — which is why the string helpers below all run the same
/// ask-then-fill dance. And <b>nothing unwinds out</b>, so a Rust panic
/// arrives as <see cref="Status.Panic"/> rather than as a torn process.
/// </para>
/// <para>
/// <c>StringMarshalling.Utf8</c> on every string-taking call is not
/// decoration: the ABI documents its <c>const char*</c> as UTF-8, and the
/// default for a <c>string</c> parameter is ANSI, which silently mangles
/// any path holding a character outside the active code page. The ABI's
/// one-byte <c>bool</c> crosses as a <c>byte</c> for the same class of
/// reason — see <c>Native.cs</c>.
/// </para>
/// </remarks>
internal static partial class Interop
{
    /// <summary>
    /// The DLL name as it appears in every <c>LibraryImport</c> below.
    /// Windows resolves it as <c>chapbook_ffi.dll</c> beside the assembly
    /// or under <c>runtimes/win-x64/native/</c> in a package.
    /// </summary>
    internal const string Library = "chapbook_ffi";

    // ---- Engine ----

    [LibraryImport(Library)]
    internal static partial uint cb_abi_version();

    [LibraryImport(Library)]
    internal static partial uint cb_capabilities();

    [LibraryImport(Library)]
    internal static partial Status cb_last_error_message(byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_set_log_callback(
        nint callback, nint user, LogLevel maxLevel);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_log(LogLevel level, string target, string message);

    [LibraryImport(Library)]
    internal static partial byte cb_log_enabled();

    // ---- Font sources ----

    [LibraryImport(Library)]
    internal static partial nint cb_font_source_host();

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint cb_font_source_embedded(string dir, string family);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_font_source_add_dir(nint fonts, string dir);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_font_source_set_generics(
        nint fonts, string serif, string sansSerif, string monospace, string cursive, string fantasy);

    [LibraryImport(Library)]
    internal static partial Status cb_font_source_use_platform_generics(nint fonts);

    [LibraryImport(Library)]
    internal static partial void cb_font_source_free(nint fonts);

    // ---- Configuration ----

    [LibraryImport(Library)]
    internal static partial nint cb_config_new(nint fonts);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_config_set_library_dir(nint config, string dir);

    [LibraryImport(Library)]
    internal static partial Status cb_config_set_cache_budget(nint config, nuint bytes);

    [LibraryImport(Library)]
    internal static partial void cb_config_free(nint config);

    [LibraryImport(Library)]
    internal static partial Status cb_config_set_http_transport(
        nint config, nint get, nint download, nint finalize, nint user);

    // ---- The response a transport builds ----

    [LibraryImport(Library)]
    internal static partial Status cb_http_response_set_status(nint response, ushort status);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_http_response_set_content_type(
        nint response, string contentType);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_http_response_add_header(
        nint response, string name, string value);

    [LibraryImport(Library)]
    internal static partial Status cb_http_response_append_body(
        nint response, nint bytes, nuint len);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_http_response_fail(nint response, string message);

    // ---- Opening and closing ----

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint cb_session_open_path(string path, nint config);

    [LibraryImport(Library)]
    internal static partial nint cb_session_open_bytes(
        byte[] bytes, nuint len, BookFormat format, nint config);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint cb_session_open_url(string url, nint config);

    [LibraryImport(Library)]
    internal static partial void cb_session_close(nint session);

    // ---- Metrics, navigation, position ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_set_metrics(nint session, NativeMetrics metrics);

    [LibraryImport(Library)]
    internal static partial Status cb_session_next_page(
        nint session, out byte moved);

    [LibraryImport(Library)]
    internal static partial Status cb_session_prev_page(
        nint session, out byte moved);

    [LibraryImport(Library)]
    internal static partial Status cb_session_next_unit(
        nint session, out byte moved);

    [LibraryImport(Library)]
    internal static partial Status cb_session_prev_unit(
        nint session, out byte moved);

    [LibraryImport(Library)]
    internal static partial Status cb_session_position(nint session, out NativePosition position);

    [LibraryImport(Library)]
    internal static partial Status cb_session_spine_len(nint session, out nuint len);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_count(nint session, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_title(
        nint session, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_book_kind(nint session, out BookKind kind);

    [LibraryImport(Library)]
    internal static partial Status cb_session_reading_direction(
        nint session, out ReadingDirection direction);

    // ---- Input ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_set_tap_zones(
        nint session, float prevFraction, float nextFraction, ReaderAction middle);

    [LibraryImport(Library)]
    internal static partial Status cb_session_tap_action(
        nint session, float x, float y, out ReaderAction action);

    [LibraryImport(Library)]
    internal static partial ReaderAction cb_key_default_action(Key key);

    [LibraryImport(Library)]
    internal static partial ReaderAction cb_char_default_action(uint codepoint);

    [LibraryImport(Library)]
    internal static partial Status cb_session_apply(
        nint session, ReaderAction action, out ActionOutcome outcome);

    // ---- Settings and fonts ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_settings(nint session, out NativeSettings settings);

    [LibraryImport(Library)]
    internal static partial Status cb_session_set_settings(
        nint session, NativeSettings settings, SettingsScope scope);

    [LibraryImport(Library)]
    internal static partial Status cb_session_font_family_count(nint session, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_font_family_at(
        nint session, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_font_family(
        nint session, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_session_set_font_family(
        nint session, string? family, SettingsScope scope);

    [LibraryImport(Library)]
    internal static partial Status cb_session_font_face_count(nint session, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_font_unresolved(
        nint session, byte[]? buf, nuint cap, out nuint needed);

    // ---- Rendering ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_render_size(
        nint session, out uint width, out uint height);

    [LibraryImport(Library)]
    internal static partial Status cb_session_render_into(
        nint session, nint pixels, nuint len, uint width, uint height, nuint stride);

    // ---- The text surface ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_text_run_count(nint session, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_text_run(
        nint session, nuint index, out NativeTextRun run);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_text_run_text(
        nint session, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_range_rects(
        nint session, uint start, uint end, NativeRect[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_speakable_text(
        nint session, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_word_count(nint session, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_word(
        nint session, nuint index, out NativeWordSpan span);

    [LibraryImport(Library)]
    internal static partial Status cb_session_word_at(
        nint session, float x, float y, out uint start, out uint end);

    // ---- Lifecycle ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_suspend(nint session);

    [LibraryImport(Library)]
    internal static partial Status cb_session_release_caches(nint session);

    [LibraryImport(Library)]
    internal static partial Status cb_session_cache_bytes(nint session, out nuint bytes);

    [LibraryImport(Library)]
    internal static partial Status cb_session_cache_budget(nint session, out nuint bytes);

    [LibraryImport(Library)]
    internal static partial Status cb_session_set_waker(nint session, nint wake, nint user);

    [LibraryImport(Library)]
    internal static partial Status cb_session_poll_loaded(
        nint session, out byte changed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_next_event(
        nint session, out NativeSessionEvent evt);

    [LibraryImport(Library)]
    internal static partial Status cb_session_has_pending_loads(
        nint session, out byte pending);

    // ---- The library ----

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_library_open(string dir, out nint library);

    [LibraryImport(Library)]
    internal static partial void cb_library_close(nint library);

    [LibraryImport(Library)]
    internal static partial Status cb_library_default_dir(
        byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_library_query(
        nint library, NativeBookQuery query, out nint shelf);

    [LibraryImport(Library)]
    internal static partial void cb_shelf_free(nint shelf);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_len(nint shelf, out nuint len);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_book(nint shelf, nuint index, out NativeBook book);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_title(
        nint shelf, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_author(
        nint shelf, nuint index, nuint author, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_series(
        nint shelf, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_language(
        nint shelf, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_identifier(
        nint shelf, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_fingerprint(
        nint shelf, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_file_path(
        nint shelf, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_cover_path(
        nint shelf, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_collection_id(
        nint shelf, nuint index, nuint slot, out long id);

    [LibraryImport(Library)]
    internal static partial Status cb_shelf_collection_name(
        nint shelf, nuint index, nuint slot, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_library_delete_book(nint library, long book);

    [LibraryImport(Library)]
    internal static partial Status cb_library_set_finished(
        nint library, long book, byte finished);

    [LibraryImport(Library)]
    internal static partial Status cb_session_book_id(nint session, out long book);

    [LibraryImport(Library)]
    internal static partial Status cb_library_collections(
        nint library, NativeCollection[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_library_collection_name(
        nint library, long collection, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_library_create_collection(
        nint library, string name, out long id);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_library_rename_collection(
        nint library, long collection, string name);

    [LibraryImport(Library)]
    internal static partial Status cb_library_delete_collection(nint library, long collection);

    [LibraryImport(Library)]
    internal static partial Status cb_library_add_to_collection(
        nint library, long book, long collection);

    [LibraryImport(Library)]
    internal static partial Status cb_library_remove_from_collection(
        nint library, long book, long collection);

    // ---- Sync ----

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_library_set_sync_targets(
        nint library, long book, string? progressionUrl, string? annotationContainer);

    [LibraryImport(Library)]
    internal static partial Status cb_library_sync_progression_url(
        nint library, long book, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_library_sync_annotation_container(
        nint library, long book, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_sync_open(
        string libraryDir, string deviceId, string deviceName,
        nint get, nint send, nint finalize, nint transportUser,
        nint wake, nint wakeUser, out nint sync);

    [LibraryImport(Library)]
    internal static partial Status cb_sync_request_all(nint sync);

    [LibraryImport(Library)]
    internal static partial Status cb_sync_request_book(nint sync, long book);

    [LibraryImport(Library)]
    internal static partial Status cb_sync_next(nint sync, out NativeSyncReport report);

    [LibraryImport(Library)]
    internal static partial void cb_sync_close(nint sync);

    // ---- Contents, locators, links ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_toc_count(nint session, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_toc_entry(
        nint session, nuint index, out NativeTocEntry entry);

    [LibraryImport(Library)]
    internal static partial Status cb_session_toc_label(
        nint session, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_goto_toc(nint session, nuint index, out byte moved);

    [LibraryImport(Library)]
    internal static partial Status cb_session_locator(
        nint session, out nuint spine, out uint offset);

    [LibraryImport(Library)]
    internal static partial Status cb_session_goto(
        nint session, nuint spine, uint offset, out byte moved);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_session_goto_anchor(
        nint session, nuint spine, string fragment, out byte moved);

    [LibraryImport(Library)]
    internal static partial Status cb_session_can_go_back(nint session, out byte can);

    [LibraryImport(Library)]
    internal static partial Status cb_session_link_at(
        nint session, float x, float y, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_session_follow_link(
        nint session, string href, out byte moved);

    // ---- Search ----

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_session_search(
        nint session, string query, nuint limit, out nuint count);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_session_search_unit(
        nint session, nuint spine, string query, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_search_hit(
        nint session, nuint index, out NativeSearchHit hit);

    [LibraryImport(Library)]
    internal static partial Status cb_session_search_context(
        nint session, nuint index, byte[]? buf, nuint cap, out nuint needed);

    // ---- The selection ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_selection_begin(
        nint session, float x, float y, out byte started);

    [LibraryImport(Library)]
    internal static partial Status cb_session_selection_drag(nint session, float x, float y);

    [LibraryImport(Library)]
    internal static partial Status cb_session_select_word_at(
        nint session, float x, float y, out byte selected);

    [LibraryImport(Library)]
    internal static partial Status cb_session_select_range(nint session, uint start, uint end);

    [LibraryImport(Library)]
    internal static partial Status cb_session_selection_clear(nint session);

    [LibraryImport(Library)]
    internal static partial Status cb_session_selected_range(
        nint session, out uint start, out uint end);

    [LibraryImport(Library)]
    internal static partial Status cb_session_selected_text(
        nint session, byte[]? buf, nuint cap, out nuint needed);

    // ---- Marks ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_add_highlight(nint session, out long id);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_session_add_note(nint session, string body, out long id);

    [LibraryImport(Library)]
    internal static partial Status cb_session_add_bookmark(nint session, out long id);

    [LibraryImport(Library)]
    internal static partial Status cb_session_highlight_at(
        nint session, float x, float y, out long id);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_session_set_highlight_color(
        nint session, long id, string? color);

    [LibraryImport(Library)]
    internal static partial Status cb_session_remove_annotation(nint session, long id);

    [LibraryImport(Library)]
    internal static partial Status cb_session_goto_annotation(
        nint session, long id, out byte moved);

    [LibraryImport(Library)]
    internal static partial Status cb_session_annotation_count(nint session, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_session_annotation(
        nint session, nuint index, out NativeAnnotation annotation);

    [LibraryImport(Library)]
    internal static partial Status cb_session_annotation_text(
        nint session, nuint index, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_annotation_color(
        nint session, nuint index, byte[]? buf, nuint cap, out nuint needed);

    // ---- Zoom ----

    [LibraryImport(Library)]
    internal static partial Status cb_session_set_page_zoom(
        nint session, float zoom, float focusX, float focusY, out byte changed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_pan_page(
        nint session, float dx, float dy, out byte changed);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_zoom(nint session, out float zoom);

    [LibraryImport(Library)]
    internal static partial Status cb_session_page_pan(nint session, out float x, out float y);

    // ---- The catalogue ----

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_open(
        nint get, nint download, nint finalize, nint user, out nint catalog);

    [LibraryImport(Library)]
    internal static partial void cb_catalog_close(nint catalog);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_catalog_set_authorization(nint catalog, string? value);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_catalog_set_basic_auth(
        nint catalog, string username, string password);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_catalog_fetch(nint catalog, string url);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_catalog_search(nint catalog, string query);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_feed_title(
        nint catalog, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_entry_count(nint catalog, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_entry(nint catalog, nuint index, out NativeEntry entry);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_entry_text(
        nint catalog, nuint index, EntryField field, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_entry_author(
        nint catalog, nuint index, nuint author, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_facet_count(nint catalog, out nuint count);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_facet(nint catalog, nuint index, out NativeFacet facet);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_facet_text(
        nint catalog, nuint index, FacetField field, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_page_href(
        nint catalog, CatalogPage direction, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_has_search(nint catalog, out byte has);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial Status cb_catalog_download(
        nint catalog, nuint index, string libraryDir, out long book);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_auth_title(
        nint catalog, byte[]? buf, nuint cap, out nuint needed);

    [LibraryImport(Library)]
    internal static partial Status cb_catalog_auth_offers_basic(nint catalog, out byte offers);
}
