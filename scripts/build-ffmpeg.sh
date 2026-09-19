#!/usr/bin/env bash
# Builds the ffmpeg/ffprobe pair Pandora runs, from source, tuned to the CPU it is built on.
#
# The portable builds `ensure_startup_binaries` downloads are compiled for a generic x86-64
# baseline so they run anywhere. This one is compiled with `-march=native` (Linux, Intel/AMD and
# arm64) or `-mcpu=native` (macOS on Apple silicon), so the compiler is free to use every
# instruction the build host has — AVX2/FMA/BMI2 on an i9-9900K, NEON and the Apple extensions on
# an M-series Mac — in ffmpeg's own C code: swscale, the filters, the AAC encoder, the muxers and
# the demuxers. libx264, libx265 and libass carry hand-written assembly with runtime CPU dispatch,
# so their inner loops were already tuned; what a native build changes for them is the C that
# surrounds those loops. The result is a binary that must not be copied to another machine, which
# is why it lives in `DB/bin` and never in the repository.
#
# Usage, from the repository root (or wherever `DB/` lives):
#
#     scripts/build-ffmpeg.sh              # build into DB/bin, keep the work tree for rebuilds
#     scripts/build-ffmpeg.sh --clean      # throw the work tree away first
#     pndc --build-ffmpeg                  # the same script, run by the binary it is embedded in
#
# Prerequisites are checked before anything is downloaded, and the missing ones are named with
# the package-manager line that installs them. The build needs a C/C++ compiler, make, cmake,
# pkg-config, curl, tar, xz, git, nasm (x86 only) and the development packages for freetype,
# fontconfig, harfbuzz and fribidi. Those four are linked from the system because fontconfig's
# configuration and cache belong to the machine; x264, x265 and libass are built here and linked
# statically, so the ffmpeg that comes out depends on nothing it did not build itself apart from
# those and libc.
#
# Everything is overridable through the environment:
#
#     PANDORA_FFMPEG_OUT      where ffmpeg/ffprobe land            (default: DB/bin)
#     PANDORA_FFMPEG_WORK     sources, prefix and build trees      (default: $OUT/build)
#     PANDORA_FFMPEG_JOBS     parallel make jobs                   (default: every core)
#     PANDORA_FFMPEG_CFLAGS   the tuning flags                     (default: -march=native / -mcpu=native)
#     PANDORA_FFMPEG_LTO      link-time optimisation for ffmpeg    (default: 0; see below)
#     PANDORA_FFMPEG_NVENC    build the NVENC/NVDEC hooks on Linux (default: 1; header-only, no driver needed)
#     PANDORA_FFMPEG_CLEAN    same as --clean                      (default: 0)
#     FFMPEG_VERSION, X264_REF, X265_VERSION, LIBASS_VERSION, NV_CODEC_HEADERS_VERSION
#
# LTO is off by default. Apple's clang 21 produced an ffmpeg that passed every encode and then
# segfaulted on exit whenever it had opened a demuxer (the `.ass` probe, an MP4 write); the same
# tree without `--enable-lto` was clean. The few percent LTO can add are not worth an unattended
# startup build that installs a binary which crashes after its work is done; `-march=native` is
# where the speed is. Set `PANDORA_FFMPEG_LTO=1` to try it on a toolchain you have verified.
#
# The versions below are pinned so two machines that run this script get the same ffmpeg, which
# is what a Pandora Mini node needs to produce the same frames as its coordinator. Bump them
# together, and bump them here rather than in the environment when the change is meant to last.
#
# The finished pair is smoke-tested before it is installed: a libx264 encode through the `ass`
# filter and a 10-bit libx265 encode, which between them cover what every preset asks of it.
# `DB/bin/ffmpeg.build` records what was built, with which flags, on which CPU; `pndc` prints it
# at startup and uses its presence to tell a native build from a downloaded one.

set -euo pipefail

FFMPEG_VERSION="${FFMPEG_VERSION:-8.1.2}"
X264_REF="${X264_REF:-stable}"
X265_VERSION="${X265_VERSION:-4.1}"
LIBASS_VERSION="${LIBASS_VERSION:-0.17.5}"
NV_CODEC_HEADERS_VERSION="${NV_CODEC_HEADERS_VERSION:-n12.2.72.0}"

OUT="${PANDORA_FFMPEG_OUT:-DB/bin}"
WORK="${PANDORA_FFMPEG_WORK:-$OUT/build}"
LTO="${PANDORA_FFMPEG_LTO:-0}"
NVENC="${PANDORA_FFMPEG_NVENC:-1}"
CLEAN="${PANDORA_FFMPEG_CLEAN:-0}"

for arg in "$@"; do
    case "$arg" in
        --clean) CLEAN=1 ;;
        -h|--help) sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build-ffmpeg: unknown argument '$arg' (only --clean is accepted)" >&2; exit 64 ;;
    esac
done

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Linux|Darwin) ;;
    *) echo "build-ffmpeg: $OS is not supported; Windows keeps the portable download" >&2; exit 78 ;;
esac

log()  { printf '\033[1;34m[build-ffmpeg]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m[build-ffmpeg]\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------------------------------------
# Prerequisites. Everything is checked up front so a machine that is missing three packages is
# told about all three, once, before a single byte is downloaded.
# ---------------------------------------------------------------------------------------------

missing_tools=()
for tool in cc c++ make cmake pkg-config curl tar xz git; do
    have "$tool" || missing_tools+=("$tool")
done
case "$ARCH" in
    x86_64|amd64|i686) have nasm || missing_tools+=("nasm") ;;
esac

missing_pkgs=()
for pkg in freetype2 fontconfig harfbuzz fribidi zlib; do
    pkg-config --exists "$pkg" 2>/dev/null || missing_pkgs+=("$pkg")
done

if [ "${#missing_tools[@]}" -gt 0 ] || [ "${#missing_pkgs[@]}" -gt 0 ]; then
    [ "${#missing_tools[@]}" -gt 0 ] && log "missing tools: ${missing_tools[*]}"
    [ "${#missing_pkgs[@]}" -gt 0 ] && log "missing development packages (pkg-config names): ${missing_pkgs[*]}"
    log "install them with one of:"
    log "  apt:    sudo apt-get install -y build-essential cmake pkg-config curl xz-utils git nasm libfreetype-dev libfontconfig-dev libharfbuzz-dev libfribidi-dev zlib1g-dev"
    log "  dnf:    sudo dnf install -y gcc gcc-c++ make cmake pkgconf-pkg-config curl xz git nasm freetype-devel fontconfig-devel harfbuzz-devel fribidi-devel zlib-devel"
    log "  pacman: sudo pacman -S --needed base-devel cmake pkgconf curl xz git nasm freetype2 fontconfig harfbuzz fribidi zlib"
    log "  brew:   brew install cmake pkg-config xz git nasm freetype fontconfig harfbuzz fribidi"
    die "prerequisites are missing; nothing was downloaded or built"
fi

JOBS="${PANDORA_FFMPEG_JOBS:-}"
if [ -z "$JOBS" ]; then
    if have nproc; then JOBS="$(nproc)"; else JOBS="$(sysctl -n hw.ncpu 2>/dev/null || echo 4)"; fi
fi

# ---------------------------------------------------------------------------------------------
# Tuning flags. `-march=native` is what GCC and Clang understand on x86 and on Linux arm64; Apple
# Clang on arm64 spells the same thing `-mcpu=native`, and older Apple toolchains that predate
# that spelling get the M1 target, which every Apple silicon Mac is a superset of.
# ---------------------------------------------------------------------------------------------

probe_cflag() {
    printf 'int main(void){return 0;}\n' > "$WORK/cflag-probe.c"
    cc "$1" -c "$WORK/cflag-probe.c" -o "$WORK/cflag-probe.o" >/dev/null 2>&1
}

mkdir -p "$OUT" "$WORK"
OUT="$(cd "$OUT" && pwd)"
WORK="$(cd "$WORK" && pwd)"
PREFIX="$WORK/prefix"
SRC="$WORK/src"
STAMPS="$WORK/stamps"

if [ "$CLEAN" = "1" ]; then
    log "cleaning $WORK"
    rm -rf "$WORK"
    mkdir -p "$WORK"
fi
mkdir -p "$PREFIX" "$SRC" "$STAMPS"

if [ -n "${PANDORA_FFMPEG_CFLAGS:-}" ]; then
    TUNE="$PANDORA_FFMPEG_CFLAGS"
elif [ "$OS" = "Darwin" ] && [ "$ARCH" = "arm64" ]; then
    if probe_cflag -mcpu=native; then TUNE="-mcpu=native"; else TUNE="-mcpu=apple-m1"; fi
else
    probe_cflag -march=native || die "the compiler rejects -march=native; set PANDORA_FFMPEG_CFLAGS explicitly"
    TUNE="-march=native"
fi

CPU_MODEL="unknown"
if [ "$OS" = "Linux" ] && [ -r /proc/cpuinfo ]; then
    CPU_MODEL="$(grep -m1 -E '^(model name|Model|Hardware)' /proc/cpuinfo | sed 's/^[^:]*:[[:space:]]*//')"
elif [ "$OS" = "Darwin" ]; then
    CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
fi

# Optional pieces, taken when the machine has them and skipped silently when it does not. None of
# these change what Pandora's CPU presets produce; they let the same ffmpeg serve a GPU node.
OPTIONAL_FLAGS=()
if [ "$OS" = "Linux" ]; then
    pkg-config --exists libva 2>/dev/null       && OPTIONAL_FLAGS+=(--enable-vaapi)
    pkg-config --exists vpl 2>/dev/null         && OPTIONAL_FLAGS+=(--enable-libvpl)
    pkg-config --exists dav1d 2>/dev/null       && OPTIONAL_FLAGS+=(--enable-libdav1d)
    pkg-config --exists SvtAv1Enc 2>/dev/null   && OPTIONAL_FLAGS+=(--enable-libsvtav1)
else
    OPTIONAL_FLAGS+=(--enable-videotoolbox)
    pkg-config --exists dav1d 2>/dev/null       && OPTIONAL_FLAGS+=(--enable-libdav1d)
    pkg-config --exists SvtAv1Enc 2>/dev/null   && OPTIONAL_FLAGS+=(--enable-libsvtav1)
fi

log "host: $OS $ARCH, $CPU_MODEL"
log "tuning: $TUNE, jobs: $JOBS, lto: $LTO"
log "versions: ffmpeg $FFMPEG_VERSION, x264 $X264_REF, x265 $X265_VERSION, libass $LIBASS_VERSION"
log "output: $OUT, work tree: $WORK"

export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export CFLAGS="$TUNE -O3 -fPIC -I$PREFIX/include"
export CXXFLAGS="$CFLAGS"
export LDFLAGS="-L$PREFIX/lib"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-$( [ "$OS" = "Darwin" ] && sw_vers -productVersion | cut -d. -f1 || echo "")}"
[ -z "$MACOSX_DEPLOYMENT_TARGET" ] && unset MACOSX_DEPLOYMENT_TARGET

fetch_tar() {
    # fetch_tar <name> <strip-components> <url> [<url>...]: downloads once, unpacks into
    # $SRC/<name>. Each URL is tried in turn; the script dies, naming them all, if none serves.
    local name="$1" strip="$2" url archive
    archive="$SRC/$name.archive"
    shift 2
    if [ -d "$SRC/$name" ]; then return 0; fi
    for url in "$@"; do
        log "downloading $name from $url"
        if curl --fail --location --retry 5 --retry-all-errors --connect-timeout 30 \
                --silent --show-error "$url" -o "$archive"; then
            mkdir -p "$SRC/$name"
            if tar -xf "$archive" -C "$SRC/$name" --strip-components="$strip"; then
                rm -f "$archive"
                return 0
            fi
            log "  ...the archive from $url did not unpack"
            rm -rf "$SRC/$name"
        else
            log "  ...that download failed"
        fi
        rm -f "$archive"
    done
    die "could not fetch $name from any of: $*"
}

fetch_git() {
    # fetch_git <name> <ref> <url> [<url>...]: a shallow clone of one ref, first URL that answers.
    local name="$1" ref="$2" url
    shift 2
    if [ -d "$SRC/$name" ]; then return 0; fi
    for url in "$@"; do
        log "cloning $name ($ref) from $url"
        if git clone --quiet --depth 1 --branch "$ref" "$url" "$SRC/$name" 2>"$WORK/git-clone.log"; then
            return 0
        fi
        log "  ...that clone failed: $(tail -1 "$WORK/git-clone.log" 2>/dev/null)"
        rm -rf "$SRC/$name"
    done
    die "could not clone $name ($ref) from any of: $*"
}

stamped() {
    # stamped <component> <version>: true when this exact version is already built into $PREFIX.
    [ -f "$STAMPS/$1" ] && [ "$(cat "$STAMPS/$1")" = "$2" ]
}

stamp() { printf '%s' "$2" > "$STAMPS/$1"; }

# The three libraries built here are installed as static archives only, so anything that links
# them also needs their dependencies on the link line — which pkg-config only reveals for
# `--static`, and `--static` then chases the *system* libraries' private dependencies too
# (gettext behind fontconfig on macOS, expat and glib on Linux), which are not always installed
# where the linker can see them. So instead each of our own .pc files has its private
# dependencies promoted to public ones: our archives get everything they need, and freetype,
# fontconfig, harfbuzz and fribidi are linked as the ordinary shared libraries they are.
promote_private() {
    local pc="$PREFIX/lib/pkgconfig/$1.pc" tmp
    [ -f "$pc" ] || return 0
    tmp="$pc.tmp"
    awk '
        /^Libs\.private:/     { sub(/^Libs\.private:[ \t]*/, ""); lp = lp " " $0; next }
        /^Requires\.private:/ { sub(/^Requires\.private:[ \t]*/, ""); rp = rp (rp == "" ? "" : ", ") $0; next }
        { lines[++n] = $0 }
        END {
            for (i = 1; i <= n; i++) {
                if (lines[i] ~ /^Libs:/ && lp != "") { print lines[i] lp; libs_done = 1; continue }
                if (lines[i] ~ /^Requires:/ && rp != "") {
                    print lines[i] (lines[i] ~ /^Requires:[ \t]*$/ ? " " : ", ") rp; req_done = 1; continue
                }
                print lines[i]
            }
            if (!libs_done && lp != "") print "Libs:" lp
            if (!req_done && rp != "") print "Requires: " rp
        }' "$pc" > "$tmp" && mv "$tmp" "$pc"
}

# ---------------------------------------------------------------------------------------------
# libx264. `--bit-depth=all` gives one library that does 8- and 10-bit, which is what the
# distribution builds ship and what a preset naming `high10` would need.
# ---------------------------------------------------------------------------------------------

if stamped x264 "$X264_REF"; then
    log "x264 $X264_REF already built"
else
    fetch_git x264 "$X264_REF" https://code.videolan.org/videolan/x264.git https://github.com/mirror/x264.git
    log "building x264"
    (
        cd "$SRC/x264"
        ./configure --prefix="$PREFIX" --enable-static --disable-shared --disable-cli --enable-pic \
            --bit-depth=all --extra-cflags="$TUNE -O3" >"$WORK/x264-configure.log" 2>&1 \
            || { cat "$WORK/x264-configure.log"; die "x264 configure failed"; }
        make -j"$JOBS" >"$WORK/x264-make.log" 2>&1 || { tail -50 "$WORK/x264-make.log"; die "x264 build failed"; }
        make install >>"$WORK/x264-make.log" 2>&1
    )
    stamp x264 "$X264_REF"
fi

# ---------------------------------------------------------------------------------------------
# libx265, as the three-depth library ffmpeg expects: the 10- and 12-bit encoders are built
# without the C API and folded into the 8-bit one, so `-pix_fmt yuv420p10le` selects the right
# encoder at runtime. This is x265's own multilib recipe, minus its shell-specific parts.
# ---------------------------------------------------------------------------------------------

if stamped x265 "$X265_VERSION"; then
    log "x265 $X265_VERSION already built"
else
    # The repository's own tag archive first: the release tarball under /downloads redirects to
    # S3 with a signed URL, which is one more thing to time out on a slow link.
    fetch_tar x265 1 \
        "https://bitbucket.org/multicoreware/x265_git/get/${X265_VERSION}.tar.gz" \
        "https://bitbucket.org/multicoreware/x265_git/downloads/x265_${X265_VERSION}.tar.gz"
    # x265 4.1 pins two CMake policies to their pre-3.0 behaviour, which CMake 4 refuses to
    # honour at all. Upstream has since dropped the pins; dropping them here is the same change.
    # It also recognises the compiler family by `STREQUAL "Clang"`, which Apple's toolchain
    # ("AppleClang") fails, so on a Mac it neither detects NEON nor assembles its aarch64
    # sources and the link then wants symbols nothing provided. `MATCHES` is upstream's fix.
    sed -i.orig \
        -e '/cmake_policy(SET CMP0025 OLD)/d' -e '/cmake_policy(SET CMP0054 OLD)/d' \
        -e 's/if(${CMAKE_CXX_COMPILER_ID} STREQUAL "Clang")/if(${CMAKE_CXX_COMPILER_ID} MATCHES "Clang")/' \
        "$SRC/x265/source/CMakeLists.txt"
    log "building x265 (12-bit, 10-bit, then 8-bit with both linked in)"
    (
        cd "$SRC/x265"
        rm -rf build-multilib
        mkdir -p build-multilib/12bit build-multilib/10bit build-multilib/8bit
        # x265's CMakeLists still declares a 2.8-era minimum, which CMake 4 refuses outright;
        # the policy floor lets it configure, and older CMakes ignore the variable.
        #
        # x265 probes SVE/SVE2 by compiling a test, which Apple's compiler passes for a CPU that
        # has neither; the assembled SVE paths would then be picked at runtime on a chip that
        # cannot execute them. No Apple silicon has SVE, so it is switched off there outright.
        # Linux arm64 and x86 are left to x265's own detection.
        sve_flags=()
        if [ "$OS" = "Darwin" ]; then sve_flags=(-DENABLE_SVE=OFF -DENABLE_SVE2=OFF); fi
        common=(-DCMAKE_BUILD_TYPE=Release -DENABLE_SHARED=OFF -DENABLE_CLI=OFF
                -DCMAKE_POLICY_VERSION_MINIMUM=3.5
                -DCMAKE_C_FLAGS="$TUNE -O3 -fPIC" -DCMAKE_CXX_FLAGS="$TUNE -O3 -fPIC"
                -DCMAKE_INSTALL_PREFIX="$PREFIX"
                ${sve_flags[@]+"${sve_flags[@]}"})
        cd build-multilib/12bit
        cmake ../../source "${common[@]}" -DHIGH_BIT_DEPTH=ON -DEXPORT_C_API=OFF -DMAIN12=ON >"$WORK/x265-12-configure.log" 2>&1 \
            || { cat "$WORK/x265-12-configure.log"; die "x265 12-bit configure failed"; }
        make -j"$JOBS" >"$WORK/x265-12-make.log" 2>&1 || { tail -50 "$WORK/x265-12-make.log"; die "x265 12-bit build failed"; }
        cd ../10bit
        cmake ../../source "${common[@]}" -DHIGH_BIT_DEPTH=ON -DEXPORT_C_API=OFF >"$WORK/x265-10-configure.log" 2>&1 \
            || { cat "$WORK/x265-10-configure.log"; die "x265 10-bit configure failed"; }
        make -j"$JOBS" >"$WORK/x265-10-make.log" 2>&1 || { tail -50 "$WORK/x265-10-make.log"; die "x265 10-bit build failed"; }
        cd ../8bit
        ln -sf ../10bit/libx265.a libx265_main10.a
        ln -sf ../12bit/libx265.a libx265_main12.a
        cmake ../../source "${common[@]}" -DEXTRA_LIB="x265_main10.a;x265_main12.a" -DEXTRA_LINK_FLAGS=-L. \
            -DLINKED_10BIT=ON -DLINKED_12BIT=ON >"$WORK/x265-8-configure.log" 2>&1 \
            || { cat "$WORK/x265-8-configure.log"; die "x265 8-bit configure failed"; }
        make -j"$JOBS" >"$WORK/x265-8-make.log" 2>&1 || { tail -50 "$WORK/x265-8-make.log"; die "x265 8-bit build failed"; }
        mv libx265.a libx265_main.a
        if [ "$OS" = "Darwin" ]; then
            libtool -static -o libx265.a libx265_main.a libx265_main10.a libx265_main12.a 2>/dev/null
        else
            ar -M <<'MRI'
CREATE libx265.a
ADDLIB libx265_main.a
ADDLIB libx265_main10.a
ADDLIB libx265_main12.a
SAVE
END
MRI
        fi
        make install >>"$WORK/x265-8-make.log" 2>&1
        # `make install` installs the 8-bit-only archive it built; the merged one goes over it.
        cp libx265.a "$PREFIX/lib/libx265.a"
    )
    stamp x265 "$X265_VERSION"
fi

# ---------------------------------------------------------------------------------------------
# libass, against the system freetype/fontconfig/harfbuzz/fribidi. Pinned rather than taken from
# the system because the renderer's version decides how a script looks, and two nodes rendering
# the same script differently is a bug that no encoder setting can explain.
# ---------------------------------------------------------------------------------------------

if stamped libass "$LIBASS_VERSION"; then
    log "libass $LIBASS_VERSION already built"
else
    fetch_tar libass 1 "https://github.com/libass/libass/releases/download/${LIBASS_VERSION}/libass-${LIBASS_VERSION}.tar.xz"
    log "building libass"
    (
        cd "$SRC/libass"
        ./configure --prefix="$PREFIX" --enable-static --disable-shared --enable-fontconfig \
            >"$WORK/libass-configure.log" 2>&1 || { cat "$WORK/libass-configure.log"; die "libass configure failed"; }
        make -j"$JOBS" >"$WORK/libass-make.log" 2>&1 || { tail -50 "$WORK/libass-make.log"; die "libass build failed"; }
        make install >>"$WORK/libass-make.log" 2>&1
    )
    stamp libass "$LIBASS_VERSION"
fi

# ---------------------------------------------------------------------------------------------
# NVENC/NVDEC headers, Linux only. Header-only: ffmpeg loads libnvidia-encode at runtime, so a
# machine without an NVIDIA driver builds and runs this exactly as one with it; the encoder just
# fails to open there, which is how `probe_hardware_encoders` already tells the two apart.
# ---------------------------------------------------------------------------------------------

NV_FLAG=()
if [ "$OS" = "Linux" ] && [ "$NVENC" = "1" ]; then
    if stamped nv-codec-headers "$NV_CODEC_HEADERS_VERSION"; then
        log "nv-codec-headers $NV_CODEC_HEADERS_VERSION already installed"
    else
        fetch_tar nv-codec-headers 1 "https://github.com/FFmpeg/nv-codec-headers/archive/refs/tags/${NV_CODEC_HEADERS_VERSION}.tar.gz"
        log "installing nv-codec-headers"
        ( cd "$SRC/nv-codec-headers" && make PREFIX="$PREFIX" install >"$WORK/nv-codec-headers.log" 2>&1 )
        stamp nv-codec-headers "$NV_CODEC_HEADERS_VERSION"
    fi
    NV_FLAG=(--enable-ffnvcodec)
fi

# ---------------------------------------------------------------------------------------------
# ffmpeg itself.
# ---------------------------------------------------------------------------------------------

fetch_tar ffmpeg 1 \
    "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" \
    "https://github.com/FFmpeg/FFmpeg/archive/refs/tags/n${FFMPEG_VERSION}.tar.gz"

for lib in x264 x265 libass; do promote_private "$lib"; done

LTO_FLAG=()
[ "$LTO" = "1" ] && LTO_FLAG=(--enable-lto)

EXTRA_LIBS="-lpthread -lm"
if [ "$OS" = "Darwin" ]; then EXTRA_LIBS="$EXTRA_LIBS -lc++"; else EXTRA_LIBS="$EXTRA_LIBS -lstdc++"; fi

CONFIGURE=(
    --prefix="$PREFIX"
    --extra-cflags="$TUNE -O3 -I$PREFIX/include"
    --extra-cxxflags="$TUNE -O3"
    --extra-ldflags="-L$PREFIX/lib"
    --extra-libs="$EXTRA_LIBS"
    --enable-gpl
    --enable-pthreads
    --enable-libx264
    --enable-libx265
    --enable-libass
    --enable-libfreetype
    --enable-libfontconfig
    --enable-libharfbuzz
    --enable-libfribidi
    --enable-zlib
    --disable-shared
    --enable-static
    --disable-doc
    --disable-debug
    --disable-ffplay
    # Pandora never grabs a screen or opens a window; without these the binary does not pick up
    # whatever X11/SDL development files happen to be installed on the build host.
    --disable-xlib
    --disable-libxcb
    --disable-sdl2
    # The `${arr[@]+"${arr[@]}"}` spelling is for bash 3.2 (macOS), where an empty array
    # expanded under `set -u` is an error rather than nothing.
    ${LTO_FLAG[@]+"${LTO_FLAG[@]}"}
    ${NV_FLAG[@]+"${NV_FLAG[@]}"}
    ${OPTIONAL_FLAGS[@]+"${OPTIONAL_FLAGS[@]}"}
)

log "configuring ffmpeg: ${CONFIGURE[*]}"
(
    cd "$SRC/ffmpeg"
    make distclean >/dev/null 2>&1 || true
    ./configure "${CONFIGURE[@]}" >"$WORK/ffmpeg-configure.log" 2>&1 \
        || { tail -40 "$WORK/ffmpeg-configure.log"; tail -30 ffbuild/config.log 2>/dev/null; die "ffmpeg configure failed (full log: $WORK/ffmpeg-configure.log)"; }
    log "building ffmpeg with $JOBS jobs"
    make -j"$JOBS" >"$WORK/ffmpeg-make.log" 2>&1 || { tail -60 "$WORK/ffmpeg-make.log"; die "ffmpeg build failed (full log: $WORK/ffmpeg-make.log)"; }
    make install >>"$WORK/ffmpeg-make.log" 2>&1
)

# ---------------------------------------------------------------------------------------------
# Smoke test against what Pandora actually asks of it, before the pair replaces whatever is in
# DB/bin: a libx264 encode through the `ass` filter, and a 10-bit libx265 encode.
# ---------------------------------------------------------------------------------------------

FF="$PREFIX/bin/ffmpeg"
FP="$PREFIX/bin/ffprobe"
[ -x "$FF" ] && [ -x "$FP" ] || die "ffmpeg/ffprobe did not get installed into $PREFIX/bin"

cat > "$WORK/smoke.ass" <<'ASS'
[Script Info]
ScriptType: v4.00+
PlayResX: 256
PlayResY: 144

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,native build
ASS

# The point of building x264/x265/libass here was a binary that carries them; a stray -L from a
# system .pc file can quietly link the distribution's shared copy instead, and that is only
# visible in the loader's output.
if [ "$OS" = "Darwin" ]; then linked="$(otool -L "$FF")"; else linked="$(ldd "$FF" 2>/dev/null || true)"; fi
if printf '%s' "$linked" | grep -qE 'lib(x264|x265|ass)[.0-9]*\.(so|dylib)'; then
    printf '%s\n' "$linked" >&2
    die "ffmpeg linked a shared x264/x265/libass instead of the ones built here"
fi

(
    cd "$WORK"
    log "smoke test: libx264 through the ass filter"
    "$FF" -v error -f lavfi -i "testsrc2=size=256x144:rate=24:duration=0.5" -vf "ass=smoke.ass,format=yuv420p" \
        -c:v libx264 -preset ultrafast -f null - || die "the libx264 + ass smoke test failed"
    log "smoke test: libx265 main10"
    "$FF" -v error -f lavfi -i "testsrc2=size=256x144:rate=24:duration=0.5" -vf "format=yuv420p10le" \
        -c:v libx265 -preset ultrafast -profile:v main10 -f null - || die "the libx265 main10 smoke test failed"
    log "smoke test: ffprobe"
    "$FP" -v error -show_entries format=format_name -of csv=p=0 smoke.ass >/dev/null || die "ffprobe smoke test failed"
)

# ---------------------------------------------------------------------------------------------
# Install, and record what was built. The record is what tells `pndc` this is a native build.
# ---------------------------------------------------------------------------------------------

cp "$FF" "$OUT/ffmpeg.tmp" && mv -f "$OUT/ffmpeg.tmp" "$OUT/ffmpeg"
cp "$FP" "$OUT/ffprobe.tmp" && mv -f "$OUT/ffprobe.tmp" "$OUT/ffprobe"
chmod 755 "$OUT/ffmpeg" "$OUT/ffprobe"

X264_COMMIT="$(git -C "$SRC/x264" rev-parse --short HEAD 2>/dev/null || echo "$X264_REF")"
{
    echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "host=$(hostname) ($OS $ARCH)"
    echo "cpu=$CPU_MODEL"
    echo "tuning=$TUNE"
    echo "lto=$LTO"
    echo "ffmpeg=$FFMPEG_VERSION"
    echo "x264=$X264_REF@$X264_COMMIT"
    echo "x265=$X265_VERSION"
    echo "libass=$LIBASS_VERSION"
    echo "optional=${NV_FLAG[*]:-} ${OPTIONAL_FLAGS[*]:-}"
    echo "configure=${CONFIGURE[*]}"
} > "$OUT/ffmpeg.build"

log "installed $OUT/ffmpeg and $OUT/ffprobe"
log "$("$OUT/ffmpeg" -version | head -1)"
log "record: $OUT/ffmpeg.build"
