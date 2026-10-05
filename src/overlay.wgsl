// Draws one monitor's frozen frame: dimmed outside the selection, with a
// border around the selection and crosshair guides before a drag starts.

struct Params {
    // Selection in window pixels: x0, y0, x1, y1 (exclusive).
    sel: vec4<f32>,
    border_color: vec4<f32>,
    // Where the annotation layer sits, in window pixels (same layout as sel).
    layer_rect: vec4<f32>,
    // Cursor position in window pixels.
    cursor: vec2<f32>,
    // Window pixel -> image pixel scale (1.0 unless the window was resized).
    img_scale: vec2<f32>,
    dim: f32,
    border: f32,
    // Bit 0: has selection. Bit 1: draw crosshair. Bit 2: show the annotation
    // layer over the frozen frame. Bit 3: dim everything when there's no
    // selection.
    flags: u32,
    _pad: u32,
}

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var<uniform> p: Params;
// The frozen screen with annotations, covering `layer_rect`.
@group(0) @binding(2) var layer: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the whole viewport.
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn inside(px: vec2<f32>, r: vec4<f32>) -> bool {
    return px.x >= r.x && px.y >= r.y && px.x < r.z && px.y < r.w;
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let px = floor(pos.xy);
    let dims = vec2<f32>(textureDimensions(frame));
    let src = vec2<i32>(min(floor(px * p.img_scale), dims - 1.0));
    var c = textureLoad(frame, src, 0).rgb;
    if (p.flags & 4u) != 0u && inside(px, p.layer_rect) {
        c = textureLoad(layer, vec2<i32>(px - p.layer_rect.xy), 0).rgb;
    }

    let has_sel = (p.flags & 1u) != 0u;
    if has_sel {
        if inside(px, p.sel) {
            return vec4<f32>(c, 1.0);
        }
        c = c * p.dim;
        if inside(px, p.sel + vec4<f32>(-p.border, -p.border, p.border, p.border)) {
            return p.border_color;
        }
    } else if (p.flags & 8u) != 0u {
        c = c * p.dim;
    }
    if (p.flags & 2u) != 0u && (px.x == p.cursor.x || px.y == p.cursor.y) {
        c = mix(c, vec3<f32>(1.0), 0.35);
    }
    return vec4<f32>(c, 1.0);
}
