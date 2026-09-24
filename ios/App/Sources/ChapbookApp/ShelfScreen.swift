import Chapbook
import ChapbookAppModel
import SwiftUI
import UniformTypeIdentifiers

struct ShelfScreen: View {
    @ObservedObject var container: AppContainer
    let onOpen: (Int64) -> Void
    let onCatalogs: () -> Void

    @StateObject private var vm: ShelfViewModel
    @ObservedObject private var downloads: Downloads
    @State private var picking = false

    init(container: AppContainer, onOpen: @escaping (Int64) -> Void, onCatalogs: @escaping () -> Void) {
        self.container = container
        self.onOpen = onOpen
        self.onCatalogs = onCatalogs
        _vm = StateObject(
            wrappedValue: ShelfViewModel(shelf: container.shelf, opener: container.opener, downloads: container.downloads))
        downloads = container.downloads
    }

    private var filtered: Bool {
        !vm.state.search.trimmingCharacters(in: .whitespaces).isEmpty || vm.state.state != nil
    }

    var body: some View {
        VStack(spacing: 0) {
            StateChips(current: vm.state.state, onState: vm.setStateFilter)
            if vm.state.loading {
                Spacer()
                ProgressView()
                Spacer()
            } else if vm.state.books.isEmpty {
                Spacer()
                ContentUnavailableView {
                    Label(L(filtered ? "shelf_no_match" : "shelf_empty"), systemImage: "books.vertical")
                } description: {
                    if !filtered { Text(L("shelf_empty_hint")) }
                }
                Spacer()
            } else {
                ScrollView {
                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 120), spacing: 12)],
                        alignment: .leading, spacing: 16
                    ) {
                        ForEach(vm.state.books) { book in
                            BookTile(
                                book: book,
                                onOpen: { onOpen(book.id) },
                                onFinished: { vm.setFinished(book, finished: $0) },
                                onRemove: { vm.remove(book) })
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                    .padding(.bottom, 96)
                }
            }
        }
        .navigationTitle(L("app_name"))
        .searchable(
            text: Binding(get: { vm.state.search }, set: vm.setSearch),
            prompt: L("shelf_search"))
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                if downloads.active > 0 {
                    Text(L("downloads_active", downloads.active)).font(.caption)
                }
                Button(action: onCatalogs) { Label(L("catalogs_open"), systemImage: "list.bullet") }
                Menu {
                    Picker(L("sort"), selection: Binding(get: { vm.state.sort }, set: vm.setSort)) {
                        Text(L("sort_read")).tag(ShelfSort.read)
                        Text(L("sort_added")).tag(ShelfSort.added)
                        Text(L("sort_title")).tag(ShelfSort.title)
                        Text(L("sort_author")).tag(ShelfSort.author)
                        Text(L("sort_series")).tag(ShelfSort.series)
                    }
                } label: {
                    Label(L("sort"), systemImage: "arrow.up.arrow.down")
                }
                Button { picking = true } label: { Label(L("shelf_add"), systemImage: "plus") }
            }
        }
        // The picker. Its URL carries a security scope the app can
        // bookmark, which is what lets the book be *adopted* rather than
        // copied. Every type, deliberately: providers report octet-stream
        // for perfectly good books, and the bytes decide the format.
        .fileImporter(
            isPresented: $picking, allowedContentTypes: [.epub, .pdf, .zip, .data],
            allowsMultipleSelection: false
        ) { result in
            if case .success(let urls) = result, let url = urls.first {
                vm.add(url, onAdded: onOpen)
            }
        }
        // A file another app handed us. Taken here because the shelf is the
        // screen that can say what happened to it.
        .onReceive(container.$openRequest) { url in
            guard let url else { return }
            container.openRequest = nil
            vm.add(url, onAdded: onOpen)
        }
        .alert(
            L("open_failed"),
            isPresented: Binding(get: { vm.state.notice == .openFailed }, set: { if !$0 { vm.dismissNotice() } })
        ) {
            Button(L("ok"), role: .cancel) { vm.dismissNotice() }
        }
        .onAppear { vm.refresh() }
    }
}

private struct StateChips: View {
    let current: ReadingState?
    let onState: (ReadingState?) -> Void

    private let choices: [(ReadingState?, String)] = [
        (nil, "filter_all"), (.unread, "filter_unread"), (.reading, "filter_reading"), (.finished, "filter_finished"),
    ]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(choices, id: \.1) { state, label in
                    Button(L(label)) { onState(state) }
                        .buttonStyle(.bordered)
                        .tint(state == current ? .accentColor : .secondary)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
    }
}

private struct BookTile: View {
    let book: Library.Book
    let onOpen: () -> Void
    let onFinished: (Bool) -> Void
    let onRemove: () -> Void

    @State private var confirmRemove = false

    var body: some View {
        Button(action: onOpen) {
            VStack(alignment: .leading, spacing: 4) {
                ZStack {
                    Color(.secondarySystemBackground)
                    if let cover = book.coverURL.flatMap({ UIImage(contentsOfFile: $0.path) }) {
                        Image(uiImage: cover).resizable().scaledToFill()
                    } else {
                        Text(book.title)
                            .font(.subheadline)
                            .multilineTextAlignment(.center)
                            .lineLimit(4)
                            .padding(8)
                            .foregroundStyle(.primary)
                    }
                }
                .aspectRatio(2 / 3, contentMode: .fit)
                .clipped()
                if book.state == .reading, let progress = book.progress {
                    ProgressView(value: progress)
                }
                Text(book.title).font(.subheadline).lineLimit(2).foregroundStyle(.primary)
                Text(book.authors.joined(separator: ", ")).font(.caption).lineLimit(1).foregroundStyle(.secondary)
                Text(describe(book)).font(.caption2).foregroundStyle(.secondary)
            }
            .multilineTextAlignment(.leading)
        }
        .buttonStyle(.plain)
        .contextMenu {
            let finished = book.state == .finished
            Button(L(finished ? "mark_unread" : "mark_finished")) { onFinished(!finished) }
            Button(L("remove"), role: .destructive) { confirmRemove = true }
        }
        .confirmationDialog(L("remove_title"), isPresented: $confirmRemove, titleVisibility: .visible) {
            Button(L("remove"), role: .destructive, action: onRemove)
            Button(L("cancel"), role: .cancel) {}
        } message: {
            Text(L("remove_body"))
        }
    }

    /// The state line, worded the way the CLI and the desktop app word it.
    private func describe(_ book: Library.Book) -> String {
        switch book.state {
        case .unread: L("state_unread")
        case .reading: L("state_reading", Int((book.progress ?? 0) * 100))
        case .finished: L("state_finished")
        }
    }
}
