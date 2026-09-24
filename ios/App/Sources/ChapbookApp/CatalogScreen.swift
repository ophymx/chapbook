import Chapbook
import ChapbookAppModel
import SwiftUI

struct CatalogScreen: View {
    @Environment(\.dismiss) private var dismiss
    private let saved: SavedCatalog?
    private let http: Http
    @StateObject private var vm: CatalogViewModel
    @State private var searching = false
    @State private var query = ""

    init(container: AppContainer, catalogID: String) {
        let saved = container.catalogs.get(catalogID)
        self.saved = saved
        http = container.http
        // A catalog that was removed while its screen was on the stack:
        // the model opens nothing and the body leaves.
        let placeholder = saved ?? SavedCatalog(id: catalogID, title: "", url: "")
        _vm = StateObject(
            wrappedValue: CatalogViewModel(
                saved: placeholder,
                session: CatalogSession(transport: container.http.transport, credentials: container.credentials),
                credentials: container.credentials, downloads: container.downloads))
    }

    private var title: String {
        if case .feed(let feed) = vm.ui, !feed.title.isEmpty { return feed.title }
        guard let saved else { return "" }
        return saved.title.isEmpty ? saved.url : saved.title
    }

    private var hasSearch: Bool {
        if case .feed(let feed) = vm.ui { return feed.hasSearch }
        return false
    }

    var body: some View {
        Group {
            switch vm.ui {
            case .opening:
                ProgressView()
            case .failed:
                ContentUnavailableView {
                    Label(L("catalog_failed"), systemImage: "wifi.exclamationmark")
                } actions: {
                    Button(L("back")) { dismiss() }
                }
            case .login(let title, let offersBasic, let retry):
                LoginForm(
                    title: title, offersBasic: offersBasic,
                    onSignIn: { username, password in vm.signIn(username: username, password: password, retry: retry) },
                    onCancel: { dismiss() })
            case .feed(let feed):
                if feed.loading {
                    ProgressView()
                } else {
                    FeedList(feed: feed, http: http, vm: vm)
                }
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        // Back walks the catalog's own crumb trail before it leaves the screen.
        .navigationBarBackButtonHidden(true)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button {
                    if !vm.back() { dismiss() }
                } label: {
                    Label(L("back"), systemImage: "chevron.backward")
                }
            }
            if hasSearch {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { searching = true } label: { Label(L("search"), systemImage: "magnifyingglass") }
                }
            }
        }
        .alert(L("search"), isPresented: $searching) {
            TextField(L("search"), text: $query)
            Button(L("search")) {
                let trimmed = query.trimmingCharacters(in: .whitespaces)
                if !trimmed.isEmpty { vm.search(trimmed) }
            }
            Button(L("cancel"), role: .cancel) {}
        }
        .onAppear { if saved == nil { dismiss() } }
    }
}

private struct FeedList: View {
    let feed: Browsing
    let http: Http
    @ObservedObject var vm: CatalogViewModel

    var body: some View {
        List {
            if !feed.facets.isEmpty {
                FacetRows(facets: feed.facets, onFacet: { vm.applyFacet($0) })
                    .listRowInsets(EdgeInsets())
                    .listRowSeparator(.hidden)
            }
            ForEach(feed.entries, id: \.index) { entry in
                EntryRow(entry: entry, http: http, onEntry: { vm.openEntry($0) }, onDownload: { vm.download($0) })
            }
            // Reaching the end asks for the next page — infinite scroll.
            if feed.nextPage != nil {
                HStack {
                    Spacer()
                    ProgressView()
                    Spacer()
                }
                .onAppear { vm.loadMore() }
            }
        }
        .listStyle(.plain)
    }
}

private struct FacetRows: View {
    let facets: [Catalog.Facet]
    let onFacet: (Catalog.Facet) -> Void

    var body: some View {
        // One control per group; the facets of a group are alternatives.
        let groups = Dictionary(grouping: facets, by: \.group).sorted { $0.key < $1.key }
        VStack(alignment: .leading, spacing: 4) {
            ForEach(groups, id: \.key) { _, group in
                Text(group.first?.groupName ?? "").font(.caption).foregroundStyle(.secondary).padding(.leading, 16)
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(group, id: \.self) { facet in
                            Button(facet.count.map { "\(facet.label) (\($0))" } ?? facet.label) { onFacet(facet) }
                                .buttonStyle(.bordered)
                                .tint(facet.isActive ? .accentColor : .secondary)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 4)
                }
            }
        }
        .padding(.vertical, 8)
    }
}

private struct EntryRow: View {
    let entry: Catalog.Entry
    let http: Http
    let onEntry: (Catalog.Entry) -> Void
    let onDownload: (Catalog.Entry) -> Void

    @State private var queued = false

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            if entry.kind == .publication {
                ZStack {
                    Color(.secondarySystemBackground)
                    if let url = entry.thumbnailURL {
                        RemoteImage(url: url, http: http)
                    }
                }
                .frame(width: 52, height: 78)
                .clipped()
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(entry.title).lineLimit(2)
                if !entry.authors.isEmpty {
                    Text(entry.authors.joined(separator: ", ")).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                if entry.kind == .publication, let summary = entry.summary, !summary.isEmpty {
                    Text(summary).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            switch entry.kind {
            case .navigation:
                Image(systemName: "chevron.forward").foregroundStyle(.secondary)
            case .publication:
                if entry.canDownload {
                    Button(L("get")) {
                        queued = true
                        onDownload(entry)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(queued)
                }
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { if entry.kind == .navigation { onEntry(entry) } }
    }
}

/// A cover or thumbnail through the app's own client, credentials
/// included — `AsyncImage` cannot send an `Authorization` header.
struct RemoteImage: View {
    let url: URL
    let http: Http
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Color.clear
            }
        }
        .task(id: url) {
            if let data = await http.bytes(url) { image = UIImage(data: data) }
        }
    }
}

private struct LoginForm: View {
    let title: String
    let offersBasic: Bool
    let onSignIn: (String, String) -> Void
    let onCancel: () -> Void

    @State private var username = ""
    @State private var password = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L("sign_in_to", title)).font(.headline)
            if offersBasic {
                TextField(L("username"), text: $username)
                    .textContentType(.username)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .textFieldStyle(.roundedBorder)
                SecureField(L("password"), text: $password)
                    .textContentType(.password)
                    .textFieldStyle(.roundedBorder)
                HStack {
                    Button(L("sign_in")) { onSignIn(username, password) }
                        .buttonStyle(.borderedProminent)
                        .disabled(username.trimmingCharacters(in: .whitespaces).isEmpty)
                    Button(L("cancel"), action: onCancel)
                }
            } else {
                Text(L("sign_in_unavailable")).foregroundStyle(.red)
                Button(L("back"), action: onCancel)
            }
            Spacer()
        }
        .padding(24)
    }
}
