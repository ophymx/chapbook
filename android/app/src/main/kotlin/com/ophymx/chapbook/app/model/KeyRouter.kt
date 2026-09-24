package com.ophymx.chapbook.app.model

/**
 * Where the activity's key events go.
 *
 * Volume keys arrive at the activity, and the page is a view three
 * layers down that may or may not be on screen. The reader installs
 * itself here while it is showing and removes itself when it goes, so
 * the activity never has to know what is in front of the reader.
 */
class KeyRouter {
    /** Act on a key-down; answer whether it was ours. */
    var onKeyDown: ((keyCode: Int) -> Boolean)? = null

    /** Claim a key-up without acting, or the system acts on the volume press. */
    var bindsKey: ((keyCode: Int) -> Boolean)? = null
}
