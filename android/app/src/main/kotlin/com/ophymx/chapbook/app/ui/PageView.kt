package com.ophymx.chapbook.app.ui

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.View
import android.view.accessibility.AccessibilityNodeProvider
import com.ophymx.chapbook.PageAccessibility
import com.ophymx.chapbook.Position
import com.ophymx.chapbook.Session
import com.ophymx.chapbook.SessionEvent
import kotlin.math.abs

/**
 * The page: the one view that draws a book.
 *
 * The five steps of `docs/SHELLS.md`, as a `View`: metrics from its size,
 * a frame from the session into its own bitmap, input turned into the
 * engine's action names, and what the engine answers turned into a
 * redraw. Gestures are recognised here, natively, and only their
 * *meaning* crosses — a tap's band, a fling's direction — because the
 * engine deliberately ships no recogniser.
 */
class PageView(context: Context, private val session: Session) : View(context) {

    private var bitmap: Bitmap? = null

    /** After every draw, where the reader is. A restore lands on the first frame, so read it then. */
    var onMoved: ((Position) -> Unit)? = null

    /** The middle band: the app's chrome, not the engine's. */
    var onMenu: (() -> Unit)? = null

    /** A page that will never arrive, for a message the reader can see. */
    var onPageFailed: ((spine: Int, message: String) -> Unit)? = null

    private val a11y = PageAccessibility(this, session)
    private val density = resources.displayMetrics.density

    private val gestures = GestureDetector(
        context,
        object : GestureDetector.SimpleOnGestureListener() {
            override fun onDown(e: MotionEvent) = true

            override fun onSingleTapUp(e: MotionEvent): Boolean {
                // Logical units: the engine was given the page box in the
                // same space, and device pixels would put every tap in
                // the last band on a 3x screen.
                val action = session.tapAction(e.x / density, e.y / density) ?: return true
                act(action)
                return true
            }

            override fun onFling(e1: MotionEvent?, e2: MotionEvent, vx: Float, vy: Float): Boolean {
                val start = e1 ?: return false
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
        // Every content change funnels through a draw, including the
        // first page and a restored position resolving — so this is the
        // one place the chrome and accessibility need telling.
        post {
            a11y.pageChanged()
            onMoved?.invoke(session.position)
        }
    }

    override fun onTouchEvent(event: MotionEvent): Boolean = gestures.onTouchEvent(event) || true

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
        val outcome = session.apply(action)
        if (outcome.needsRedraw) invalidate()
        return outcome.consumed
    }
}
