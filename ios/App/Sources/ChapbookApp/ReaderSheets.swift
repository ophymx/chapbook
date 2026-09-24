import Chapbook
import ChapbookAppModel
import SwiftUI

// MARK: Contents

struct ContentsSheet: View {
    let entries: [Session.TOCEntry]
    let currentSpine: Int
    let onPick: (Session.TOCEntry) -> Void

    var body: some View {
        NavigationStack {
            Group {
                if entries.isEmpty {
                    ContentUnavailableView(L("contents_empty"), systemImage: "list.bullet")
                } else {
                    List(entries) { entry in
                        // A heading that links nowhere is kept for its
                        // children's sake and drawn as one.
                        let linked = entry.spine != nil
                        Button { onPick(entry) } label: {
                            Text(entry.label)
                                .fontWeight(entry.spine == currentSpine ? .bold : .regular)
                                .foregroundStyle(linked ? .primary : .secondary)
                                .lineLimit(2)
                                .padding(.leading, CGFloat(entry.depth) * 16)
                        }
                        .disabled(!linked)
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle(L("contents"))
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}

// MARK: Search

struct SearchSheet: View {
    @ObservedObject var vm: ReaderViewModel
    let onPick: (Session.SearchHit) -> Void
    @State private var query = ""
    @FocusState private var focused: Bool

    private var summary: String {
        if vm.search.running { return L("search_running") }
        if vm.search.query.isEmpty { return "" }
        if vm.search.hits.isEmpty { return L("search_none") }
        return L("search_count", vm.search.hits.count)
    }

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 8) {
                TextField(L("search_hint"), text: $query)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused)
                    .submitLabel(.search)
                    .padding(.horizontal, 16)
                Text(summary).font(.caption).padding(.horizontal, 16)
                List(vm.search.hits, id: \.self) { hit in
                    Button { onPick(hit) } label: {
                        Text(emboldened(hit)).lineLimit(3).foregroundStyle(.primary)
                    }
                }
                .listStyle(.plain)
            }
            .padding(.top, 12)
            .navigationTitle(L("search"))
            .navigationBarTitleDisplayMode(.inline)
        }
        // Opening search means typing: the field takes focus and the
        // keyboard comes up. A keystroke does not search; a pause does.
        .onAppear {
            query = vm.search.query
            focused = true
        }
        .task(id: query) {
            try? await Task.sleep(nanoseconds: 300_000_000)
            if !Task.isCancelled, query.trimmingCharacters(in: .whitespaces) != vm.search.query { vm.search(query) }
        }
    }

    /// The matched words bold rather than the whole line — in scalar
    /// offsets, which is what the engine counts in.
    private func emboldened(_ hit: Session.SearchHit) -> AttributedString {
        let scalars = Array(hit.context.unicodeScalars)
        let end = min(Int(hit.matchInContext.upperBound), scalars.count)
        let start = min(Int(hit.matchInContext.lowerBound), end)
        guard start < end else { return AttributedString(hit.context) }
        var match = AttributedString(String(String.UnicodeScalarView(scalars[start..<end])))
        match.font = .body.bold()
        return AttributedString(String(String.UnicodeScalarView(scalars[..<start]))) + match
            + AttributedString(String(String.UnicodeScalarView(scalars[end...])))
    }
}

// MARK: Marks

struct MarksSheet: View {
    @ObservedObject var vm: ReaderViewModel
    let onPick: (Session.Annotation) -> Void

    private func kind(_ mark: Session.Annotation) -> String {
        switch mark.kind {
        case .bookmark: L("mark_bookmark")
        case .highlight: L("mark_highlight")
        case .note: L("mark_note")
        }
    }

    var body: some View {
        NavigationStack {
            List {
                Button(L("bookmark_page"), action: { vm.addBookmark() })
                if vm.marks.isEmpty {
                    Text(L("marks_empty")).font(.subheadline).foregroundStyle(.secondary)
                } else {
                    ForEach(vm.marks) { mark in
                        Button { onPick(mark) } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text((mark.text?.isEmpty == false ? mark.text : nil) ?? kind(mark))
                                    .lineLimit(2).foregroundStyle(.primary)
                                Text("\(kind(mark)) · \(L("mark_at", Int((mark.progression * 100).rounded())))")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    .onDelete { offsets in
                        for index in offsets { vm.removeMark(vm.marks[index].id) }
                    }
                }
            }
            .navigationTitle(L("marks"))
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}

// MARK: Settings

struct SettingsSheet: View {
    @ObservedObject var vm: ReaderViewModel
    let families: [String]
    @State private var thisBook = false
    @State private var lineHeight: Double = 1.4

    var body: some View {
        NavigationStack {
            Form {
                if let settings = vm.settings {
                    Section {
                        HStack {
                            Text(L("font_size"))
                            Spacer()
                            Button(L("smaller")) { step(settings, by: -2) }.buttonStyle(.bordered)
                            Text("\(Int(settings.baseFontSize.rounded()))").monospacedDigit().frame(minWidth: 32)
                            Button(L("larger")) { step(settings, by: 2) }.buttonStyle(.bordered)
                        }
                        // The slider reflows on release, not on every
                        // pixel of the drag.
                        HStack {
                            Text(L("line_height"))
                            Slider(value: $lineHeight, in: 1...2, step: 0.1) { editing in
                                if !editing {
                                    var next = settings
                                    next.lineHeight = lineHeight
                                    vm.apply(next, thisBook: thisBook)
                                }
                            }
                            Text(String(format: "%.1f", lineHeight)).monospacedDigit().frame(width: 36)
                        }
                        Toggle(L("justify"), isOn: Binding(get: { settings.justify }, set: { on in
                            var next = settings
                            next.justify = on
                            vm.apply(next, thisBook: thisBook)
                        }))
                        Toggle(L("publisher_styles"), isOn: Binding(get: { settings.publisherStyles }, set: { on in
                            var next = settings
                            next.publisherStyles = on
                            vm.apply(next, thisBook: thisBook)
                        }))
                    }
                    Section(L("theme")) {
                        Picker(L("theme"), selection: Binding(get: { settings.theme }, set: { theme in
                            var next = settings
                            next.theme = theme
                            vm.apply(next, thisBook: thisBook)
                        })) {
                            Text(L("theme_light")).tag(Theme.light)
                            Text(L("theme_sepia")).tag(Theme.sepia)
                            Text(L("theme_dark")).tag(Theme.dark)
                        }
                        .pickerStyle(.segmented)
                    }
                    Section(L("typeface")) {
                        Picker(L("typeface"), selection: Binding(get: { vm.fontFamily ?? "" }, set: { family in
                            vm.setFontFamily(family.isEmpty ? nil : family, thisBook: thisBook)
                        })) {
                            Text(L("typeface_publisher")).tag("")
                            ForEach(families, id: \.self) { family in Text(family).tag(family) }
                        }
                    }
                    Section {
                        Toggle(L("scope_this_book"), isOn: $thisBook)
                        Text(L("scope_hint")).font(.caption).foregroundStyle(.secondary)
                        Button(L("reset_book_settings"), action: { vm.resetBookSettings() })
                    }
                }
            }
            .navigationTitle(L("settings"))
            .navigationBarTitleDisplayMode(.inline)
        }
        .onAppear { lineHeight = Double(vm.settings?.lineHeight ?? 1.4) }
        .onChange(of: vm.settings?.lineHeight) { _, value in
            if let value { lineHeight = Double(value) }
        }
    }

    private func step(_ settings: ReadingSettings, by delta: CGFloat) {
        var next = settings
        next.baseFontSize = min(40, max(10, settings.baseFontSize + delta))
        vm.apply(next, thisBook: thisBook)
    }
}

// MARK: The selection's action bar

/// Floats above the selection, or below it when there is no room above.
struct SelectionBar: View {
    let bounds: SelectionBounds
    let onHighlight: () -> Void
    let onNote: () -> Void
    let onCopy: () -> Void

    var body: some View {
        let barHeight: CGFloat = 44
        let gap: CGFloat = 12
        let handles: CGFloat = 24
        let above = bounds.bounds.minY - barHeight - gap
        let y = above >= 0 ? above : bounds.bounds.maxY + handles + gap
        HStack(spacing: 0) {
            Button(L("sel_highlight"), action: onHighlight)
            Divider().frame(height: 20)
            Button(L("sel_note"), action: onNote)
            Divider().frame(height: 20)
            Button(L("sel_copy"), action: onCopy)
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 8)
        .frame(height: barHeight)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
        .shadow(radius: 4)
        .frame(maxWidth: .infinity)
        .offset(y: y)
    }
}
