import Chapbook
import SwiftUI
import UIKit

/// Where the selection sits on screen, for placing an action bar beside
/// it.
struct SelectionBounds: Equatable {
    let locators: Range<UInt32>
    let bounds: CGRect
}

/// The page: the one view that draws a book.
///
/// The five steps of `docs/SHELLS.md`, as a `UIView`: metrics from its
/// bounds, a frame from the session onto its own layer, input turned into
/// the engine's actions, and what the engine answers turned into a
/// redraw. Gestures are recognised here, natively, and only their
/// *meaning* crosses — a tap's band, a swipe's direction, a long press
/// becoming a word — because the engine deliberately ships no
/// recogniser.
///
/// A press can land on three things and the order is the contract: a
/// link, then a stored highlight, then the tap band. Both hit tests are
/// exact, so a miss falls through to the turn.
@MainActor
final class PageView: UIView, UIGestureRecognizerDelegate {
    let session: Session
    private let isImageBook: Bool

    /// After every draw, where the reader is. A restore lands on the first
    /// frame, so read it then.
    var onMoved: ((Session.Position) -> Void)?
    /// The middle band: the app's chrome, not the engine's.
    var onMenu: (() -> Void)?
    /// A page that will never arrive, for a message the reader can see.
    var onPageFailed: ((Int, String) -> Void)?
    /// An `http(s)` link the engine will not follow; the app opens a
    /// browser.
    var onExternalLink: ((String) -> Void)?
    /// A tap on a stored highlight, with the point, for a recolor menu.
    var onHighlightTapped: ((Int64, CGPoint) -> Void)?
    /// The selection after each draw, or `nil` once there is none.
    var onSelection: ((SelectionBounds?) -> Void)?

    private let pageLayer = CALayer()
    private let handlesLayer = CAShapeLayer()
    private var a11y: PageAccessibility?
    private var laidOut = CGSize.zero

    // ---- Selection state ----

    /// Which handle the finger holds: none, the start, the end.
    private enum Handle { case none, start, end }
    private var handle = Handle.none
    private var startHandle: CGRect?
    private var endHandle: CGRect?
    private let handleRadius: CGFloat = 9
    private let grabRadius: CGFloat = 28

    /// On prose, the pinch's accumulated factor; one font step per
    /// gesture.
    private var pinch: CGFloat = 1
    private let swipe: CGFloat = 48

    init(session: Session, kind: BookKind) {
        self.session = session
        isImageBook = kind != .epub
        super.init(frame: .zero)
        backgroundColor = .clear
        pageLayer.contentsGravity = .topLeft
        pageLayer.anchorPoint = .zero
        layer.addSublayer(pageLayer)
        handlesLayer.fillColor = UIColor.systemBlue.cgColor
        layer.addSublayer(handlesLayer)

        // Thirds, with the middle asking for the app's menu. The engine
        // answers `toggleMenu` for that band and this view acts on the
        // name before the engine sees it, since the engine has no menu.
        try? session.setTapZones(prevFraction: 1 / 3, nextFraction: 1 / 3, middle: .toggleMenu)
        // Image books decode off the main actor and the waker is how
        // their pages reach the screen. It fires on the loader thread,
        // so it only hops; the hop polls, and a visible change repaints.
        try? session.onWake { [weak self] in
            Task { @MainActor in self?.woke() }
        }
        a11y = PageAccessibility(host: self, session: session)

        let tap = UITapGestureRecognizer(target: self, action: #selector(tapped))
        let press = UILongPressGestureRecognizer(target: self, action: #selector(pressed))
        let pan = UIPanGestureRecognizer(target: self, action: #selector(panned))
        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(pinched))
        pan.maximumNumberOfTouches = 1
        tap.require(toFail: press)
        pan.delegate = self
        for recognizer in [tap, press, pan, pinch] { addGestureRecognizer(recognizer) }
    }

    required init?(coder: NSCoder) { nil }

    // MARK: Shape and drawing

    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = window?.screen.scale ?? UIScreen.main.scale
        pageLayer.contentsScale = scale
        guard bounds.width > 0, bounds.height > 0, bounds.size != laidOut else { return }
        laidOut = bounds.size
        try? session.setMetrics(
            PageMetrics(
                width: bounds.width, height: bounds.height,
                marginTop: 20, marginRight: 20, marginBottom: 20, marginLeft: 20,
                dpiScale: scale))
        render()
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil { becomeFirstResponder() }
    }

    /// Rasterize and show the current page. Every content change funnels
    /// through here, including the first page and a restored position
    /// resolving — so this is the one place the chrome and accessibility
    /// need telling.
    func render() {
        guard laidOut != .zero else { return }
        do {
            let image = try session.renderImage()
            pageLayer.frame = CGRect(x: 0, y: 0, width: bounds.width, height: bounds.height)
            pageLayer.contents = image
        } catch {
            EngineLog.write("render failed: \(error)", level: .error, target: "page")
        }
        drawHandles()
        a11y?.pageChanged()
        if let position = try? session.position() { onMoved?(position) }
        onSelection?(selectionBounds())
    }

    private func woke() {
        if (try? session.pollLoaded()) == true { render() }
        for event in (try? session.drainEvents()) ?? [] {
            switch event {
            case .unitFailed(let spine, let message): onPageFailed?(spine, message)
            case .positionChanged: render()
            default: break
            }
        }
    }

    /// The engine paints the selection itself; the handles are the view's.
    private func drawHandles() {
        guard let range = try? session.selectedRange(), let rects = try? session.rects(for: range),
            let first = rects.first, let last = rects.last
        else {
            startHandle = nil
            endHandle = nil
            handlesLayer.path = nil
            return
        }
        let start = toView(first)
        let end = toView(last)
        startHandle = start
        endHandle = end
        let path = UIBezierPath()
        path.append(
            UIBezierPath(
                arcCenter: CGPoint(x: start.minX, y: start.maxY + handleRadius), radius: handleRadius,
                startAngle: 0, endAngle: .pi * 2, clockwise: true))
        path.append(
            UIBezierPath(
                arcCenter: CGPoint(x: end.maxX, y: end.maxY + handleRadius), radius: handleRadius,
                startAngle: 0, endAngle: .pi * 2, clockwise: true))
        handlesLayer.path = path.cgPath
    }

    /// Page-space rect to view points: `view = fit * zoom + pan`.
    private func toView(_ rect: CGRect) -> CGRect {
        let zoom = (try? session.pageZoom()) ?? 1
        let pan = (try? session.pagePan()) ?? .zero
        return CGRect(
            x: rect.minX * zoom + pan.x, y: rect.minY * zoom + pan.y,
            width: rect.width * zoom, height: rect.height * zoom)
    }

    private func selectionBounds() -> SelectionBounds? {
        guard let range = try? session.selectedRange(), let rects = try? session.rects(for: range),
            let first = rects.first
        else { return nil }
        let bounds = rects.dropFirst().reduce(toView(first)) { $0.union(toView($1)) }
        return SelectionBounds(locators: range, bounds: bounds)
    }

    // MARK: Input

    /// Links, then highlights, then the band — the order is the contract.
    @objc private func tapped(_ gesture: UITapGestureRecognizer) {
        let point = gesture.location(in: self)
        // A tap outside a selection dismisses it and does nothing else.
        if ((try? session.selectedRange()) ?? nil) != nil {
            try? session.clearSelection()
            render()
            return
        }
        if let href = ((try? session.link(at: point)) ?? nil) {
            if (try? session.follow(link: href)) == true { render() } else { onExternalLink?(href) }
            return
        }
        if let id = ((try? session.highlight(at: point)) ?? nil) {
            onHighlightTapped?(id, point)
            return
        }
        guard let action = ((try? session.tapAction(at: point)) ?? nil) else { return }
        act(action)
    }

    /// The long press is the touch spelling of "select this word"; the
    /// drag that may follow extends it.
    @objc private func pressed(_ gesture: UILongPressGestureRecognizer) {
        let point = gesture.location(in: self)
        switch gesture.state {
        case .began:
            if (try? session.selectWord(at: point)) == true { render() }
        case .changed:
            try? session.dragSelection(to: point)
            render()
        default:
            render()
        }
    }

    @objc private func panned(_ gesture: UIPanGestureRecognizer) {
        let point = gesture.location(in: self)
        switch gesture.state {
        case .began:
            handle = grabbedHandle(at: point)
            if handle == .start { reanchorAtEnd() }
        case .changed:
            if handle != .none {
                try? session.dragSelection(to: point)
                render()
            } else if isImageBook, ((try? session.pageZoom()) ?? 1) > 1 {
                // A zoomed image pans under the finger.
                let delta = gesture.translation(in: self)
                gesture.setTranslation(.zero, in: self)
                if (try? session.panPage(by: CGSize(width: delta.x, height: delta.y))) == true { render() }
            }
        case .ended, .cancelled:
            if handle != .none {
                handle = .none
                render()
                return
            }
            if isImageBook, ((try? session.pageZoom()) ?? 1) > 1 { return }
            // A swipe toward the leading edge turns forward. Which edge is
            // leading is the book's, not the screen's.
            let travel = gesture.translation(in: self)
            guard abs(travel.x) >= abs(travel.y), abs(travel.x) >= swipe else { return }
            let rightToLeft = (try? session.readingDirection()) == .rightToLeft
            let forward = rightToLeft ? travel.x > 0 : travel.x < 0
            act(forward ? .nextPage : .prevPage)
        default:
            break
        }
    }

    @objc private func pinched(_ gesture: UIPinchGestureRecognizer) {
        if isImageBook {
            // Straight into the engine, around the fingers.
            let zoom = min(8, max(1, ((try? session.pageZoom()) ?? 1) * gesture.scale))
            gesture.scale = 1
            if (try? session.setPageZoom(zoom, focus: gesture.location(in: self))) == true { render() }
            return
        }
        switch gesture.state {
        case .changed:
            pinch *= gesture.scale
            gesture.scale = 1
        case .ended:
            // The same gesture on prose is a font-size change, one step
            // per pinch, and the threshold keeps a wobbling two-finger
            // tap from reflowing the book.
            if pinch > 1.15 { act(.fontUp) } else if pinch < 0.87 { act(.fontDown) }
            pinch = 1
        case .cancelled:
            pinch = 1
        default:
            break
        }
    }

    /// A pan that starts on a handle, or over a zoomed picture, is ours;
    /// anything else waits for the long press to fail first.
    func gestureRecognizer(
        _ gestureRecognizer: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
    ) -> Bool {
        other is UIPinchGestureRecognizer
    }

    private func grabbedHandle(at point: CGPoint) -> Handle {
        guard let start = startHandle, let end = endHandle else { return .none }
        if hypot(point.x - start.minX, point.y - (start.maxY + handleRadius)) < grabRadius { return .start }
        if hypot(point.x - end.maxX, point.y - (end.maxY + handleRadius)) < grabRadius { return .end }
        return .none
    }

    /// Dragging the start handle means the *end* is the anchor. The
    /// engine's selection is anchor-plus-drag, so re-anchor at the last
    /// selected glyph and let the drag move the other end.
    private func reanchorAtEnd() {
        guard let range = try? session.selectedRange(), let last = try? session.rects(for: range).last else {
            return
        }
        _ = try? session.beginSelection(at: CGPoint(x: last.maxX - 0.5, y: last.midY))
    }

    @discardableResult
    func act(_ action: Action) -> Bool {
        if action == .toggleMenu {
            onMenu?()
            return true
        }
        // A zoomed comic page is view state; a turn starts the next page
        // at fit.
        if isImageBook, action == .nextPage || action == .prevPage {
            _ = try? session.setPageZoom(1, focus: .zero)
        }
        guard let outcome = try? session.apply(action) else { return false }
        if outcome == .changed { render() }
        return outcome != .unhandled
    }

    // MARK: A hardware keyboard

    override var canBecomeFirstResponder: Bool { true }

    /// The engine's default table, which already knows what arrows,
    /// space and the page keys mean — and answers `nil` for the rest, so
    /// nothing here swallows a key it does not bind.
    override var keyCommands: [UIKeyCommand]? {
        let keys: [(String, Key)] = [
            (UIKeyCommand.inputLeftArrow, .arrowLeft), (UIKeyCommand.inputRightArrow, .arrowRight),
            (UIKeyCommand.inputUpArrow, .arrowUp), (UIKeyCommand.inputDownArrow, .arrowDown),
            (UIKeyCommand.inputPageUp, .pageUp), (UIKeyCommand.inputPageDown, .pageDown),
            (" ", .space),
        ]
        return keys.compactMap { input, key in
            guard Action.default(for: key) != nil else { return nil }
            let command = UIKeyCommand(input: input, modifierFlags: [], action: #selector(keyed))
            command.wantsPriorityOverSystemBehavior = true
            return command
        }
    }

    @objc private func keyed(_ command: UIKeyCommand) {
        let key: Key? =
            switch command.input {
            case UIKeyCommand.inputLeftArrow: .arrowLeft
            case UIKeyCommand.inputRightArrow: .arrowRight
            case UIKeyCommand.inputUpArrow: .arrowUp
            case UIKeyCommand.inputDownArrow: .arrowDown
            case UIKeyCommand.inputPageUp: .pageUp
            case UIKeyCommand.inputPageDown: .pageDown
            case " ": .space
            default: nil
            }
        if let key, let action = Action.default(for: key) { act(action) }
    }
}

/// The page inside SwiftUI. The session belongs to the model, so a
/// rotation re-lays this view out and never reopens the book.
struct PageViewRepresentable: UIViewRepresentable {
    let session: Session
    let kind: BookKind
    let onMoved: (Session.Position) -> Void
    let onMenu: () -> Void
    let onPageFailed: (Int, String) -> Void
    let onExternalLink: (String) -> Void
    let onHighlightTapped: (Int64, CGPoint) -> Void
    let onSelection: (SelectionBounds?) -> Void
    /// Installed while the page is showing: the model's changes want a
    /// repaint, and this is the one view that can give one.
    let install: (PageView?) -> Void

    func makeUIView(context: Context) -> PageView {
        let view = PageView(session: session, kind: kind)
        wire(view)
        install(view)
        return view
    }

    func updateUIView(_ view: PageView, context: Context) {
        wire(view)
    }

    static func dismantleUIView(_ view: PageView, coordinator: ()) {
        view.onMoved = nil
        view.onSelection = nil
    }

    private func wire(_ view: PageView) {
        view.onMoved = onMoved
        view.onMenu = onMenu
        view.onPageFailed = onPageFailed
        view.onExternalLink = onExternalLink
        view.onHighlightTapped = onHighlightTapped
        view.onSelection = onSelection
    }
}
