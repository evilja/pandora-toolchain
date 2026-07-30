# kagami-trace

`kagami-trace` is a deterministic, pure-Rust raster-to-vector tracing library. It accepts encoded images or raw RGBA pixels and returns a portable data model containing a palette, layered closed paths, line segments, and cubic Bézier segments. It has no dependency on Pandora, PNdc, libkagami, a browser, or a system graphics library.

The crate lives in its own workspace so this directory can be moved to a separate repository later without reorganizing its source. Pandora's ASS adapter stays outside the crate at `src/libkagami/tracing.rs`.

## Library use

```rust
use kagami_trace::{TraceOptions, trace_image};

let image = std::fs::read("logo.png")?;
let trace = trace_image(&image, &TraceOptions {
    color_count: 10,
    path_simplify: 1.0,
    ..TraceOptions::default()
})?;

println!("{} colors, {} paths", trace.palette.len(), trace.path_count());
std::fs::write("logo.svg", trace.to_svg())?;
std::fs::write("logo.trace.json", trace.to_json_pretty()?)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

For already-decoded data, `trace_rgba(width, height, rgba, options)` avoids the optional image decoder. Build with `default-features = false` when only that API is needed.

## Output model

- `Trace` records the source and sampled dimensions, palette, and color layers.
- `TraceLayer` points into the palette and contains all contours for that color.
- `Path` is closed implicitly and records whether its winding is a hole.
- `Segment` is either a line or a cubic Bézier.
- `schema_version` is currently `1`. `Trace::validate` rejects unsupported or malformed portable traces.

Coordinates always use the original image's coordinate space, even when `max_dimension` lowers the internal tracing resolution. JSON and SVG serialization are deterministic for the same pixels and options.

## Options

| Field | Default | Meaning |
| --- | ---: | --- |
| `color_count` | 12 | Maximum perceptual palette size, 1–512 |
| `preserve_gradients` | false | Preserve exact low-complexity RGBA ramps instead of the bounded histogram |
| `color_smoothing` | 1 | Edge-preserving cleanup passes, 0–4 |
| `path_simplify` | 1.0 | VisionCortex polygon reduction tolerance in output pixels |
| `curve_fit` | 0.65 | VisionCortex spline subdivision strength; 0 keeps polygonal paths |
| `corner_threshold` | 55° | Turns at or above this angle stay sharp |
| `min_area` | 4 | Reassign connected color regions smaller than this sampled area |
| `alpha_threshold` | 8 | Treat pixels at or below this alpha as transparent |
| `max_dimension` | 1024 | Longest internal sampled side; 0 traces at source resolution |

Use `TraceOptions::for_preset(...)` with `TracePreset::LogoUi`, `TracePreset::Illustration`, `TracePreset::Photo`, or `TracePreset::Gradient` for content-aware starting points. Logo/UI preserves sharp tiny geometry with no curve fitting; Illustration favors clean curved shapes; Photo keeps more tonal regions; Gradient preserves subtle color ramps with 128 palette slots, source-space palette reconstruction, and geometry that does not displace color-band boundaries. ASS has no native gradient fill, so Gradient approximates ramps with more flat-color layers and can produce larger files. More colors are not always higher quality: on screenshots and compressed images they can preserve antialiasing and noise as thousands of tiny vector regions.

The tracer applies optional edge-aware color cleanup and Oklab clustering, then reconstructs output colors as weighted means of the original sRGB samples instead of round-tripping Oklab centroids, then traces each connected palette region with the established VisionCortex 0.9.1 path walker, staircase-aware polygon simplification, corner-preserving subdivision, and its `flo_curves`-backed error-bounded cubic fitter. VisionCortex is pinned exactly and is pure Rust under MIT/Apache-2.0. Standard presets use the bounded reduced-precision histogram to suppress raster noise; the Gradient preset enables `preserve_gradients`, retaining exact RGBA histogram entries while the image has at most 8,192 distinct colors and falling back safely for more complex sources.

Encoded input is capped at 32 MiB, dimensions at 8192 pixels per side, and decoded images at 32 megapixels.

## Development page

Run Pandora's tracing-only development server:

```text
cargo run --bin pntrace
```

Then open <http://127.0.0.1:8788>. Pandora also bakes the page into `pndc` at `/trace`; there, tracing and ASS export use the normal bearer-protected `/api/v1/trace` routes while standalone `pntrace` remains unauthenticated on loopback. The page supports drag-and-drop, Logo/UI, Illustration, Photo, and Gradient presets, live tracing controls, raster/vector comparison, per-color visibility, SVG/JSON downloads, and libkagami ASS conversion. SVG presets use a small, adjustable same-color underlap to hide antialiasing cracks between independently fitted regions: 0px for Logo/UI, 0.25px for Illustration and Gradient, and 0.5px for Photo. Library callers can opt in with `Trace::to_svg_with_seam_overlap`. ASS is only offered as a ZIP containing one `.ass` file, with a same-color 0.5px region overlap by default. `pntrace` does not start or connect to PNdc. Use `--host` and `--port` to change the listener, for example:

```text
cargo run --bin pntrace -- --port 9000
```

The Pandora-side server keeps libkagami out of this extraction-ready vector crate. It binds to loopback by default and should not be treated as a production upload service.

## Checks

```text
cargo test --manifest-path kagami-trace/Cargo.toml --all-features
```

## License

GNU General Public License, version 3 or later. See `LICENSE`.
