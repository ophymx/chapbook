import ChapbookAppModel
import SwiftUI

struct CatalogsScreen: View {
    @ObservedObject var catalogs: Catalogs
    let onOpen: (String) -> Void

    @State private var adding = false
    @State private var url = ""

    init(container: AppContainer, onOpen: @escaping (String) -> Void) {
        catalogs = container.catalogs
        self.onOpen = onOpen
    }

    var body: some View {
        Group {
            if catalogs.all.isEmpty {
                ContentUnavailableView {
                    Label(L("catalogs_empty"), systemImage: "books.vertical")
                } description: {
                    Text(L("catalogs_empty_hint"))
                }
            } else {
                List {
                    ForEach(catalogs.all) { catalog in
                        Button { onOpen(catalog.id) } label: {
                            VStack(alignment: .leading) {
                                Text(catalog.title.isEmpty ? catalog.url : catalog.title).lineLimit(1)
                                Text(catalog.url).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                        .foregroundStyle(.primary)
                    }
                    .onDelete { offsets in
                        for index in offsets { catalogs.remove(catalogs.all[index].id) }
                    }
                }
            }
        }
        .navigationTitle(L("catalogs"))
        .toolbar {
            Button { adding = true } label: { Label(L("catalog_add"), systemImage: "plus") }
        }
        .alert(L("catalog_add"), isPresented: $adding) {
            TextField(L("catalog_url"), text: $url, prompt: Text(L("catalog_url_hint")))
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button(L("add")) {
                let trimmed = url.trimmingCharacters(in: .whitespaces)
                url = ""
                guard !trimmed.isEmpty else { return }
                onOpen(catalogs.add(url: trimmed).id)
            }
            Button(L("cancel"), role: .cancel) { url = "" }
        }
    }
}
