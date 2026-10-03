#!/bin/sh
# Assembles a variant of the spike plasmoid that carries the compiled QML
# module inside its own package and imports it by relative directory path.
# Tests whether a plasmoid can load a native QML plugin with no import-path
# setup in the hosting shell's environment.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out="$here/build/plasmoid-bundled"
rm -rf "$out"
mkdir -p "$out/contents/ui" "$out/contents/lib/traytrayspike"
cp "$here/build/qml/org/traytray/spike/qmldir" \
   "$here/build/qml/org/traytray/spike/libtraytrayspike.so" \
   "$here/build/qml/org/traytray/spike/traytrayspike.qmltypes" \
   "$out/contents/lib/traytrayspike/"
sed 's/"Id": "org.traytray.spike.m0"/"Id": "org.traytray.spike.m0bundled"/; s/"Name": "Traytray M0 spike"/"Name": "Traytray M0 spike (bundled module)"/' \
    "$here/plasmoid/metadata.json" > "$out/metadata.json"
sed 's#^import org.traytray.spike$#import "../lib/traytrayspike"#' \
    "$here/plasmoid/contents/ui/main.qml" > "$out/contents/ui/main.qml"
grep -n 'import "../lib' "$out/contents/ui/main.qml"
