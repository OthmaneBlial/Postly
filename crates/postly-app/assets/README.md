# Native brand assets

`postly-icon.png` is the 512 px rendering of the geometry in
[`website/logo.svg`](../../../website/logo.svg). The same image is used by the
native navigation header and the window/Dock icon, including direct launches
of `postly-gui`. The macOS bundle uses `packaging/Postly.icns` from the same
renderer; do not fall back to the eframe default logo.

To regenerate on macOS from the repository root:

```sh
brand_dir=$(mktemp -d /tmp/postly-brand.XXXXXX)
swift tools/render-macos-icon.swift "$brand_dir/Postly.iconset"
cp "$brand_dir/Postly.iconset/icon_512x512.png" crates/postly-app/assets/postly-icon.png
iconutil -c icns "$brand_dir/Postly.iconset" -o "$brand_dir/Postly.icns"
cp "$brand_dir/Postly.icns" packaging/Postly.icns
```

The renderer mirrors the SVG's geometry; update both together when changing
the logo. Font sources and their separate license are in `fonts/`.
