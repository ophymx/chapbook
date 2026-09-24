#!/usr/bin/env bash
# Build the Rust side straight into the library module's jniLibs, then
# check the two things that otherwise fail on a device rather than here.
#
# cargo-ndk's -o writes the `<abi>/lib*.so` layout Gradle expects, so
# nothing is copied by hand and the .so is never checked in.
#
# Portable shell on purpose: BSD `stat` has no `-c` and BSD `grep` no `-P`,
# and a Mac with Android Studio is a place this runs.
set -euo pipefail

: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to e.g. \$HOME/Android/Sdk/ndk/<version>}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here/.."

profile="${1:-debug}"
out=android/chapbook/src/main/jniLibs
build=(build)
[ "$profile" = "release" ] && build=(build --release)

cargo ndk -t arm64-v8a -t x86_64 -P 24 -o "$out" "${build[@]}" -p chapbook-jni

readelf="$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-readelf | head -1)"
native=android/chapbook/src/main/kotlin/com/ophymx/chapbook/Native.kt
status=0

for so in "$out"/*/libchapbook_jni.so; do
    abi="$(basename "$(dirname "$so")")"
    printf '%-10s %s  %s bytes\n' "$abi" "$so" "$(wc -c < "$so" | tr -d ' ')"

    # A missing `#[link(name = ...)]` leaves the AndroidBitmap symbols
    # undefined with nothing in DT_NEEDED to resolve them. The build stays
    # green and System.loadLibrary is where it goes wrong.
    if ! "$readelf" -d "$so" | grep -q 'libjnigraphics\.so'; then
        echo "  !! libjnigraphics is not in DT_NEEDED — this .so will fail to load" >&2
        status=1
    fi

    # Every `external fun` Kotlin declares must have a symbol to bind to,
    # or the first call throws UnsatisfiedLinkError. Names drift silently:
    # nothing on either side references the other at compile time.
    exported="$("$readelf" --dyn-syms "$so" | grep -o 'Java_com_ophymx_chapbook_Native_[A-Za-z0-9_]*' | sort -u)"
    while read -r fn; do
        [ -z "$fn" ] && continue
        if ! grep -qx "Java_com_ophymx_chapbook_Native_$fn" <<<"$exported"; then
            echo "  !! Native.$fn is declared in Kotlin but not exported by the .so" >&2
            status=1
        fi
    done < <(sed -n 's/.*external fun \([a-zA-Z0-9_]*\).*/\1/p' "$native")
done

# ---- The C ABI, checked against Android without Android consuming it ----
#
# `chapbook-jni` binds `chapbook-reader` directly and deliberately: Kotlin
# reaches Rust through JNI, which is already a C ABI, so routing it through
# `chapbook-ffi` as well would put two C-shaped boundaries back to back with
# Rust in the middle converting both ways. See docs/STABILITY.md.
#
# That leaves `chapbook-ffi`'s header unexercised on this platform, and the
# thing worth knowing is cheap to ask directly: does it compile under the
# NDK's clang, for both ABIs, and does it compose with `jni.h` — which is
# what an iOS-shaped host or a third party embedding the `.so` would do.
header=crates/chapbook-ffi/include/chapbook.h
if [ -f "$header" ]; then
    clang="$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/clang | head -1)"
    probe="$(mktemp -d)"
    trap 'rm -rf "$probe"' EXIT
    # Included twice, and beside jni.h, so the guard and the composition are
    # both covered rather than assumed.
    cat > "$probe/probe.c" <<'PROBE'
#include "chapbook.h"
#include "chapbook.h"
#include <jni.h>
JNIEXPORT jlong JNICALL Java_probe_Native_open(JNIEnv *e, jclass c, jstring p, jlong cfg) {
    const char *path = (*e)->GetStringUTFChars(e, p, NULL);
    cb_session *s = cb_session_open_path(path, (cb_config *)(intptr_t)cfg);
    (*e)->ReleaseStringUTFChars(e, p, path);
    (void)c;
    return (jlong)(intptr_t)s;
}
PROBE
    for target in aarch64-linux-android24 x86_64-linux-android24; do
        if "$clang" --target="$target" -std=c11 -Wall -Wextra -Werror \
             -fsyntax-only -I "$(dirname "$header")" "$probe/probe.c"; then
            printf '%-10s chapbook.h compiles and composes with jni.h\n' "${target%%-*}"
        else
            echo "  !! chapbook.h does not compile for $target" >&2
            status=1
        fi
    done
fi

[ $status -eq 0 ] && echo "jniLibs ok: linkage and symbols both check out"
exit $status
