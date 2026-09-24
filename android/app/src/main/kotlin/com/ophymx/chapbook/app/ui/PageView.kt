package com.ophymx.chapbook.app.ui

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import android.view.accessibility.AccessibilityNodeProvider
import com.ophymx.chapbook.BookKind
import com.ophymx.chapbook.PageAccessibility
import com.ophymx.chapbook.Position
import com.ophymx.chapbook.Session
import com.ophymx.chapbook.SessionEvent
import kotlin.math.abs
import kotlin.math.hypot

/** Where the selection sits on screen, for placing an action bar beside it. */
data class SelectionBounds(val start: Int, val end: Int, val boundsPx: RectF)

/**
 * The page: the one view that draws a book.
 *
 * The five steps of `docs/SHELLS.md`, as a `View`: metrics from its size,
 * a frame from the session into its own bitmap, input turned into the
 * engine's action names, and what the engine answers turned into a
 * redraw. Gestures are recognised here, natively, and only their
 * *meaning* crosses — a tap's band, a fling's direction, a long press
 * becoming a word — because the engine deliberately ships no recogniser.
 *
 * A press can land on three things and the order is the contract:
 * a link, then a stored highlight, then the tap band. Both hit tests
 * are exact, so a miss falls through to the turn.
 */
class PageView(context: Context, private val session: Session) : View(context) {

    private var bitmap: Bitmap? = null

    /** After every draw, where the reader is. A restore lands on the first frame, so read it then. */
    var onMoved: ((Position) -> Unit)? = null

    /** The middle band: the app's chrome, not the engine's. */
    var onMenu: (() -> Unit)? = null

    /** A page that will never arrive, for a message the reader can see. */
    var onPageFailed: ((spine: Int, message: String) -> Unit)? = null

    /** An `http(s)` link the engine will not follow; the app opens a browser. */
    var onExternalLink: ((href: String) -> Unit)? = null

    /** A tap on a stored highlight, with the view-pixel point, for a recolor menu. */
    var onHighlightTapped: ((id: Long, xPx: Float, yPx: Float) -> Unit)? = null

    /** The selection after each draw, or null once there is none. */
    var onSelection: ((SelectionBounds?) -> Unit)? = null

    private val a11y = PageAccessibility(this, session)
    private val density = resources.displayMetrics.density
    private val isImageBook = session.kind != BookKind.EPUB

    // ---- Selection state ----

    /** A long press anchored a selection and the finger is still down. */
    private var selecting = false

    /** Which handle the finger holds: 0 none, 1 start, 2 end. */
    private var handle = 0
    private var startHandle: RectF? = null
    private var endHandle: RectF? = null
    private val handlePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0xFF1E88E5.toInt() }
    private val handleRadius = 9 * density
    private val grabRadius = 28 * density

    // ---- Pinch state ----

    /** On prose, the pinch's accumulated factor; one font step per gesture. */
    private var pinch = 1f

    private val scaler = ScaleGestureDetector(
        context,
        object : ScaleGestureDetector.SimpleOnScaleGestureListener() {
            override fun onScale(detector: ScaleGestureDetector): Boolean {
                if (isImageBook) {
                    // Straight into the engine, in logical units around the fingers.
                    val zoom = (session.pageZoom * detector.scaleFactor).coerceIn(1f, 8f)
                    if (session.setPageZoom(zoom, detector.focusX / density, detector.focusY / density)) invalidate()
                } else {
                    pinch *= detector.scaleFactor
                }
                return true
            }

            override fun onScaleEnd(detector: ScaleGestureDetector) {
                if (!isImageBook) {
                    // The same gesture on prose is a font-size change, one
                    // step per pinch, and the threshold keeps a wobbling
                    // two-finger tap from reflowing the book.
                    if (pinch > 1.15f) act("font-up") else if (pinch < 0.87f) act("font-down")
                }
                pinch = 1f
            }
        },
    )

    private val gestures = GestureDetector(
        context,
        object : GestureDetector.SimpleOnGestureListener() {
            override fun onDown(e: MotionEvent) = true

            override fun onSingleTapUp(e: MotionEvent): Boolean {
                tap(e.x, e.y)
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                // The long press is the touch spelling of "select this
                // word"; the drag that may follow extends it.
                if (session.selectWordAt(e.x / density, e.y / density)) {
                    selecting = true
                    parent?.requestDisallowInterceptTouchEvent(true)
                    invalidate()
                }
            }

            override fun onScroll(e1: MotionEvent?, e2: MotionEvent, dx: Float, dy: Float): Boolean {
                // A zoomed image pans under the finger; at fit the drag
                // falls through to a fling.
                if (isImageBook && session.pageZoom > 1f) {
                    if (session.panPage(-dx / density, -dy / density)) invalidate()
                    return true
                }
                return false
            }

            override fun onFling(e1: MotionEvent?, e2: MotionEvent, vx: Float, vy: Float): Boolean {
                val start = e1 ?: return false
                if (isImageBook && session.pageZoom > 1f) return false
                val dx = e2.x - start.x
                val dy = e2.y - start.y
                if (abs(dx) < abs(dy) || abs(dx) < swipe) return false
                // A swipe toward the leading edge turns forward. Which edge
                // is leading is the book's, not the screen's.
                val forward = if (session.readingDirection == "rtl") dx > 0 else dx < 0
                act(if (forward) "next-page" else "prev-page")
                return true
            }
        },
    )

    private val swipe = 48 * density

    init {
        keepScreenOn = true
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_YES
        // Thirds, with the middle asking for the app's menu. The engine
        // answers "toggle-menu" for that band and this view acts on the
        // name before the engine sees it, since the engine has no menu.
        session.setTapZones(1f / 3f, 1f / 3f, "toggle-menu")
        // Image books decode off the UI thread and the waker is how their
        // pages reach the screen. It fires on the loader thread, so it
        // only posts; the post polls, and a visible change repaints.
        session.setWaker(
            Runnable {
                post {
                    if (session.pollLoaded()) invalidate()
                    for (event in session.drainEvents()) {
                        when (event) {
                            is SessionEvent.UnitFailed -> onPageFailed?.invoke(event.spine, event.message)
                            is SessionEvent.PositionChanged -> invalidate()
                            else -> {}
                        }
                    }
                }
            },
        )
    }

    override fun getAccessibilityNodeProvider(): AccessibilityNodeProvider = a11y

    override fun dispatchHoverEvent(event: MotionEvent): Boolean =
        a11y.dispatchHoverEvent(event) || super.dispatchHoverEvent(event)

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        if (w <= 0 || h <= 0) return
        session.setMetrics(w / density, h / density, 20f, density)
        bitmap?.recycle()
        // The session says how big the surface is; the round trip through
        // logical units does not always land back on the view's pixel.
        val size = session.renderSize ?: return
        bitmap = Bitmap.createBitmap(size.width, size.height, Bitmap.Config.ARGB_8888)
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        val target = bitmap ?: return
        // Straight into the bitmap's own pixels: premultiplied RGBA from
        // the engine is what ARGB_8888 holds, so nothing converts.
        if (session.renderInto(target) == 0) canvas.drawBitmap(target, 0f, 0f, null)
        drawHandles(canvas)
        // Every content change funnels through a draw, including the
        // first page and a restored position resolving — so this is the
        // one place the chrome and accessibility need telling.
        post {
            a11y.pageChanged()
            onMoved?.invoke(session.position)
            onSelection?.invoke(selectionBounds())
        }
    }

    /** The engine paints the selection itself; the handles are the view's. */
    private fun drawHandles(canvas: Canvas) {
        val range = session.selectedRange
        if (range == null) {
            startHandle = null
            endHandle = null
            return
        }
        val rects = session.rangeRects(range.first, range.last + 1)
        if (rects.isEmpty()) return
        val first = toView(rects.first())
        val last = toView(rects.last())
        startHandle = first
        endHandle = last
        canvas.drawCircle(first.left, first.bottom + handleRadius, handleRadius, handlePaint)
        canvas.drawCircle(last.right, last.bottom + handleRadius, handleRadius, handlePaint)
    }

    /** Page-space logical rect to view pixels: `view = fit * zoom + pan`, then density. */
    private fun toView(r: RectF): RectF {
        val zoom = session.pageZoom
        val (px, py) = session.pagePan
        return RectF(
            (r.left * zoom + px) * density,
            (r.top * zoom + py) * density,
            (r.right * zoom + px) * density,
            (r.bottom * zoom + py) * density,
        )
    }

    private fun selectionBounds(): SelectionBounds? {
        val range = session.selectedRange ?: return null
        val rects = session.rangeRects(range.first, range.last + 1)
        if (rects.isEmpty()) return null
        val bounds = RectF(toView(rects.first()))
        for (r in rects) bounds.union(toView(r))
        return SelectionBounds(range.first, range.last + 1, bounds)
    }

    // ---- Input ----

    override fun onTouchEvent(event: MotionEvent): Boolean {
        scaler.onTouchEvent(event)
        if (scaler.isInProgress) return true
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                handle = grabbedHandle(event.x, event.y)
                if (handle != 0) {
                    parent?.requestDisallowInterceptTouchEvent(true)
                    if (handle == 1) reanchorAtEnd()
                    return true
                }
            }
            MotionEvent.ACTION_MOVE -> if (selecting || handle != 0) {
                session.selectionDrag(event.x / density, event.y / density)
                invalidate()
                return true
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> if (selecting || handle != 0) {
                selecting = false
                handle = 0
                invalidate()
                return true
            }
        }
        return gestures.onTouchEvent(event) || true
    }

    private fun grabbedHandle(x: Float, y: Float): Int {
        val s = startHandle ?: return 0
        val e = endHandle ?: return 0
        if (hypot(x - s.left, y - (s.bottom + handleRadius)) < grabRadius) return 1
        if (hypot(x - e.right, y - (e.bottom + handleRadius)) < grabRadius) return 2
        return 0
    }

    /**
     * Dragging the start handle means the *end* is the anchor. The
     * engine's selection is anchor-plus-drag, so re-anchor at the last
     * selected glyph and let the drag move the other end.
     */
    private fun reanchorAtEnd() {
        val range = session.selectedRange ?: return
        val rects = session.rangeRects(range.first, range.last + 1)
        val last = rects.lastOrNull() ?: return
        session.selectionBegin(last.right - 0.5f, last.centerY())
    }

    /** Links, then highlights, then the band — the order is the contract. */
    private fun tap(xPx: Float, yPx: Float) {
        // A tap outside a selection dismisses it and does nothing else.
        if (session.selectedRange != null) {
            session.selectionClear()
            invalidate()
            return
        }
        val x = xPx / density
        val y = yPx / density
        session.linkAt(x, y)?.let { href ->
            if (session.followLink(href)) invalidate() else onExternalLink?.invoke(href)
            return
        }
        session.highlightAt(x, y)?.let { id ->
            onHighlightTapped?.invoke(id, xPx, yPx)
            return
        }
        val action = session.tapAction(x, y) ?: return
        act(action)
    }

    /** Forwarded from the activity: volume keys arrive there. Answers whether the key was ours. */
    fun handleKey(keyCode: Int): Boolean {
        val action = session.actionForKeyCode(keyCode) ?: return false
        return act(action)
    }

    /** Whether the key is bound, without acting — for key-up. */
    fun bindsKey(keyCode: Int): Boolean = session.actionForKeyCode(keyCode) != null

    private fun act(action: String): Boolean {
        if (action == "toggle-menu") {
            onMenu?.invoke()
            return true
        }
        // A zoomed comic page is view state; a turn starts the next page at fit.
        if (isImageBook && (action == "next-page" || action == "prev-page")) session.setPageZoom(1f, 0f, 0f)
        val outcome = session.apply(action)
        if (outcome.needsRedraw) invalidate()
        return outcome.consumed
    }
}
