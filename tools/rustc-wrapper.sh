#!/bin/sh
# Cargo invokes this with the real rustc path as the first argument.
# mupdf-sys always links libmupdf-third. MuPDF omits that archive when every
# third-party library comes from the system, so create it before the link.
rustc=$1
shift

ensure_third() {
    dir=$1
    if [ -f "$dir/libmupdf.a" ] && [ ! -f "$dir/libmupdf-third.a" ]; then
        echo 'void marker_mupdf_third_anchor(void) {}' | cc -c -x c -o "$dir/marker-mupdf-anchor.o" -
        ar crs "$dir/libmupdf-third.a" "$dir/marker-mupdf-anchor.o"
        rm -f "$dir/marker-mupdf-anchor.o"
    fi
}

for arg in "$@"; do
    case "$arg" in
        -Lnative=*)
            ensure_third "${arg#-Lnative=}"
            ;;
        native=*)
            ensure_third "${arg#native=}"
            ;;
    esac
done

exec "$rustc" "$@"
