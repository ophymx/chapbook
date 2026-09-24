import Chapbook
import ChapbookAppModel
import SwiftUI

/// Which sheet is up, if any.
enum Sheet: String, Identifiable {
    case contents, search, marks, settings
    var id: String { rawValue }
}

/// A tap on a stored highlight: which, and where on the page.
struct HighlightTap: Identifiable {
    let id: Int64
    let point: CGPoint
}

/// The page's ground, so the safe-inset strips match the engine's theme.
private func paper(_ theme: Theme?) -> Color {
    switch theme {
    case .light: Color.white
    case .sepia: Color(red: 0xF6 / 255, green: 0xF0 / 255, blue: 0xE2 / 255)
    case .dark: Color(red: 0x12 / 255, green: 0x12 / 255, blue: 0x12 / 255)
    case nil: Color(.systemBackground)
    }
}

struct ReaderScreen: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @StateObject private var vm: ReaderViewModel

    @State private var chrome = false
    @State private var sheet: Sheet?
    @State private var noteDialog = false
    @State private var noteBody = ""
    @State private var highlightTap: HighlightTap?
    @State private var selectionBounds: SelectionBounds?
    @State private var toast: String?
    @State private var page: PageView?

    init(container: AppContainer, bookID: Int64) {
        _vm = StateObject(
            wrappedValue: ReaderViewModel(
                bookID: bookID, shelf: container.shelf, opener: container.opener,
                preferences: container.preferences))
    }

    var body: some View {
        ZStack {
            // The page never goes under the status bar, the notch or the
            // home indicator: the paper colour fills those, and the page
            // box is what is left. The chrome overlays the page, inside
            // the same bounds.
            paper(vm.settings?.theme).ignoresSafeArea()
            switch vm.state {
            case .opening:
                ProgressView()
            case .gone:
                ContentUnavailableView {
                    Label(L("open_gone"), systemImage: "book.closed")
                } actions: {
                    Button(L("back")) { dismiss() }
                }
            case .reading(let reading):
                reader(reading)
            }
            chromeBars
            if let toast {
                Text(toast)
                    .font(.footnote)
                    .padding(10)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
                    .frame(maxHeight: .infinity, alignment: .bottom)
                    .padding(.bottom, chrome ? 72 : 24)
                    .task { try? await Task.sleep(nanoseconds: 3_000_000_000); self.toast = nil }
            }
        }
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        .statusBarHidden(!chrome)
        // `didEnterBackground` is the last callback the platform
        // guarantees: save while there is a process to save from.
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didEnterBackgroundNotification)) { _ in
            vm.suspend()
        }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didReceiveMemoryWarningNotification)) { _ in
            vm.memoryWarning()
        }
        .onDisappear { vm.close() }
    }

    @ViewBuilder
    private func reader(_ reading: Reading) -> some View {
        PageViewRepresentable(
            session: reading.session, kind: reading.kind,
            onMoved: { vm.moved($0) },
            onMenu: { chrome.toggle() },
            onPageFailed: { spine, message in toast = L("page_failed", spine + 1, message) },
            onExternalLink: { href in
                if let url = URL(string: href) { openURL(url) } else { toast = L("link_failed", href) }
            },
            onHighlightTapped: { id, point in highlightTap = HighlightTap(id: id, point: point) },
            onSelection: { bounds in
                selectionBounds = bounds
                vm.selected(bounds?.locators)
            },
            install: { view in
                page = view
                vm.onNeedsRedraw = { [weak view] in view?.render() }
            }
        )
        .overlay(alignment: .topLeading) {
            if let bounds = selectionBounds {
                SelectionBar(
                    bounds: bounds,
                    onHighlight: { vm.highlightSelection() },
                    onNote: { noteDialog = true },
                    onCopy: {
                        UIPasteboard.general.string = vm.selection?.text ?? ""
                        vm.clearSelection()
                        toast = L("copied")
                    })
            }
        }
        .confirmationDialog(
            L("mark_highlight"), isPresented: Binding(get: { highlightTap != nil }, set: { if !$0 { highlightTap = nil } })
        ) {
            if let tap = highlightTap {
                Button(L("color_theme")) { vm.recolorHighlight(tap.id, color: nil) }
                ForEach(highlightColors, id: \.1) { label, color in
                    Button(L(label)) { vm.recolorHighlight(tap.id, color: color) }
                }
                Button(L("remove_highlight"), role: .destructive) { vm.removeMark(tap.id) }
            }
        }
        .alert(L("note_title"), isPresented: $noteDialog) {
            TextField(L("note_hint"), text: $noteBody)
            Button(L("save")) {
                let body = noteBody
                noteBody = ""
                if !body.trimmingCharacters(in: .whitespaces).isEmpty { vm.note(onSelection: body) }
            }
            Button(L("cancel"), role: .cancel) { noteBody = "" }
        }
        .sheet(item: $sheet) { which in
            Group {
                switch which {
                case .contents:
                    ContentsSheet(entries: reading.contents, currentSpine: vm.place.spine) { entry in
                        vm.go(to: entry)
                        sheet = nil
                        chrome = false
                    }
                case .search:
                    SearchSheet(vm: vm) { hit in
                        vm.go(toHit: hit)
                        sheet = nil
                        chrome = false
                    }
                case .marks:
                    MarksSheet(vm: vm) { mark in
                        vm.go(toMark: mark.id)
                        sheet = nil
                        chrome = false
                    }
                case .settings:
                    SettingsSheet(vm: vm, families: reading.fontFamilies)
                }
            }
            .presentationDetents([.medium, .large])
        }
    }

    private var chromeBars: some View {
        VStack {
            if chrome {
                HStack(spacing: 12) {
                    Button { dismiss() } label: { Image(systemName: "chevron.backward") }
                    VStack(alignment: .leading, spacing: 4) {
                        Text(vm.place.title).font(.headline).lineLimit(1)
                        ProgressReadout(place: vm.place, preferences: vm.preferences)
                    }
                    Spacer()
                    // The engine's Back: where the reader was before the
                    // last link, greyed out by absence rather than state.
                    if vm.place.canGoBack {
                        Button(L("return_back"), action: { vm.goBack() })
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .background(.regularMaterial)
                .transition(.move(edge: .top).combined(with: .opacity))
            }
            Spacer()
            if chrome {
                HStack {
                    Spacer()
                    Button { sheet = .contents } label: { Label(L("contents"), systemImage: "list.bullet") }
                    Spacer()
                    Button { sheet = .search } label: { Label(L("search"), systemImage: "magnifyingglass") }
                    Spacer()
                    Button { sheet = .marks } label: { Label(L("marks"), systemImage: "bookmark") }
                    Spacer()
                    Button { sheet = .settings } label: { Label(L("settings"), systemImage: "textformat.size") }
                    Spacer()
                }
                .labelStyle(.iconOnly)
                .font(.title3)
                .padding(.vertical, 14)
                .background(.regularMaterial)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.easeInOut(duration: 0.15), value: chrome)
    }
}

/// The whole-book bar, and a readout whose words are the reader's choice.
private struct ProgressReadout: View {
    let place: ChapbookAppModel.Place
    @ObservedObject var preferences: Preferences

    private var text: String {
        switch preferences.progressLabel {
        case .percent:
            L("progress_percent", Int((place.bookFraction * 100).rounded()))
        case .pagesLeft:
            {
                let left = max(0, place.pageCount - place.page - 1)
                return left == 0 ? L("progress_last_page") : L("progress_pages_left", left)
            }()
        case .chapterPage:
            L("reader_progress", place.spine + 1, place.spineLength, place.page + 1, place.pageCount)
        }
    }

    var body: some View {
        HStack(spacing: 10) {
            ProgressView(value: place.bookFraction)
                .progressViewStyle(.linear)
                .frame(maxWidth: 160)
            Text(text).font(.caption).foregroundStyle(.secondary).monospacedDigit()
        }
    }
}

let highlightColors: [(String, String)] = [
    ("color_yellow", "#ffe082"), ("color_green", "#a5d6a7"), ("color_blue", "#90caf9"), ("color_pink", "#f48fb1"),
]
