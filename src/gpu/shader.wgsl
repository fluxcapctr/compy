// Compositing passes. Every pass draws one full-screen triangle into a BGRA8 target, reading the previous
// state of that target from `dest` and one source texture placed by an affine map from device pixels.
// Colors are premultiplied throughout, as Cairo's ARGB32 is.

struct Uniforms {
    // Device pixel center -> source texel (row 0: x' = a x + b y + c; row 1: y').
    to_layer0: vec4<f32>,
    to_layer1: vec4<f32>,
    to_mask0: vec4<f32>,
    to_mask1: vec4<f32>,
    layer_size: vec2<f32>,
    mask_size: vec2<f32>,
    op: u32,
    blend: u32,
    flags: u32,
    opacity: f32,
    target_size: vec2<f32>,
    mask_outside: f32,
    lut_luma: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var dest_tex: texture_2d<f32>;
@group(0) @binding(2) var layer_tex: texture_2d<f32>;
@group(0) @binding(3) var mask_tex: texture_2d<f32>;
@group(0) @binding(4) var lin: sampler;
@group(0) @binding(5) var near: sampler;

const OP_COMPOSITE: u32 = 0u;
const OP_ATOP: u32 = 1u;
const OP_MODULATE: u32 = 2u;
const OP_UNPREMULTIPLY: u32 = 3u;
const OP_RESTORE: u32 = 4u;
const OP_ADJUST: u32 = 5u;
const OP_DOWNSAMPLE: u32 = 6u;
const OP_COPY: u32 = 7u;
const OP_BACKGROUND: u32 = 8u;

const FLAG_MASK: u32 = 1u;
const FLAG_NEAREST: u32 = 2u;
const FLAG_LAYER: u32 = 4u;
const FLAG_MASK_DEVICE: u32 = 8u;   // the mask is a device-sized coverage sampled 1:1
const FLAG_LAYER_DEVICE: u32 = 16u; // the source is device-sized and sampled 1:1

struct Out { @builtin(position) pos: vec4<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: Out;
    o.pos = vec4(p[i], 0.0, 1.0);
    return o;
}

fn affine(r0: vec4<f32>, r1: vec4<f32>, p: vec2<f32>) -> vec2<f32> {
    return vec2(r0.x * p.x + r0.y * p.y + r0.z, r1.x * p.x + r1.y * p.y + r1.z);
}

// Cairo clips a placed pattern to its rectangle and antialiases that edge: coverage falls off over the
// last texel-width around the rectangle, measured in source texels.
fn rect_coverage(p: vec2<f32>, size: vec2<f32>, scale: vec2<f32>) -> f32 {
    let inside = min(min(p.x, size.x - p.x) * scale.x, min(p.y, size.y - p.y) * scale.y);
    return clamp(inside + 0.5, 0.0, 1.0);
}

fn sample_layer(frag: vec2<f32>) -> vec4<f32> {
    if ((u.flags & FLAG_LAYER_DEVICE) != 0u) {
        let uv = frag / u.target_size;
        return textureSampleLevel(layer_tex, near, uv, 0.0);
    }
    let p = affine(u.to_layer0, u.to_layer1, frag);
    // Texels per device pixel along each axis, for the edge falloff.
    let sx = length(vec2(u.to_layer0.x, u.to_layer1.x));
    let sy = length(vec2(u.to_layer0.y, u.to_layer1.y));
    let cov = rect_coverage(p, u.layer_size, vec2(1.0 / max(sx, 1e-6), 1.0 / max(sy, 1e-6)));
    if (cov <= 0.0) { return vec4(0.0); }
    let uv = p / u.layer_size;
    var c: vec4<f32>;
    if ((u.flags & FLAG_NEAREST) != 0u) { c = textureSampleLevel(layer_tex, near, uv, 0.0); } else { c = textureSample(layer_tex, lin, uv); }
    return c * cov;
}

fn sample_mask(frag: vec2<f32>) -> f32 {
    if ((u.flags & FLAG_MASK) == 0u) { return 1.0; }
    if ((u.flags & FLAG_MASK_DEVICE) != 0u) {
        return textureSampleLevel(mask_tex, near, frag / u.target_size, 0.0).r;
    }
    let p = affine(u.to_mask0, u.to_mask1, frag);
    if (p.x < 0.0 || p.y < 0.0 || p.x > u.mask_size.x || p.y > u.mask_size.y) { return u.mask_outside; }
    let uv = p / u.mask_size;
    if ((u.flags & FLAG_NEAREST) != 0u) { return textureSampleLevel(mask_tex, near, uv, 0.0).r; }
    return textureSample(mask_tex, lin, uv).r;
}

// PDF separable blend functions on straight colors.
fn blend_channel(cb: f32, cs: f32, mode: u32) -> f32 {
    switch mode {
        case 1u: { return cb * cs; }                                   // multiply
        case 2u: { return cb + cs - cb * cs; }                          // screen
        case 3u: { if (cb <= 0.5) { return cs * 2.0 * cb; } return cs + (2.0 * cb - 1.0) - cs * (2.0 * cb - 1.0); } // overlay = hardlight(cs, cb)
        case 4u: { return min(cb, cs); }                                // darken
        case 5u: { return max(cb, cs); }                                // lighten
        case 6u: { return abs(cb - cs); }                               // difference
        case 7u: { if (cb <= 0.0) { return 0.0; } if (cs >= 1.0) { return 1.0; } return min(1.0, cb / (1.0 - cs)); } // color dodge
        case 8u: { if (cb >= 1.0) { return 1.0; } if (cs <= 0.0) { return 0.0; } return 1.0 - min(1.0, (1.0 - cb) / cs); } // color burn
        default: { return cs; }
    }
}

fn lum(c: vec3<f32>) -> f32 { return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b; }

fn clip_color(c: vec3<f32>) -> vec3<f32> {
    let l = lum(c);
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    var o = c;
    if (n < 0.0) { o = l + (c - l) * l / max(l - n, 1e-6); }
    if (x > 1.0) { o = l + (o - l) * (1.0 - l) / max(x - l, 1e-6); }
    return o;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    let d = l - lum(c);
    return clip_color(c + vec3(d));
}

fn sat(c: vec3<f32>) -> f32 { return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b)); }

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    if (mx <= mn) { return vec3(0.0); }
    return (c - mn) * s / (mx - mn);
}

fn blend_rgb(cb: vec3<f32>, cs: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 9u: { return set_lum(set_sat(cs, sat(cb)), lum(cb)); }   // hue
        case 10u: { return set_lum(set_sat(cb, sat(cs)), lum(cb)); }  // saturation
        case 11u: { return set_lum(cs, lum(cb)); }                    // color
        case 12u: { return set_lum(cb, lum(cs)); }                    // luminosity
        default: { return vec3(blend_channel(cb.r, cs.r, mode), blend_channel(cb.g, cs.g, mode), blend_channel(cb.b, cs.b, mode)); }
    }
}

// Premultiplied source over/blended with premultiplied dest (PDF compositing with a blend function).
fn composite(dest: vec4<f32>, src: vec4<f32>, mode: u32) -> vec4<f32> {
    if (mode == 0u) { return src + dest * (1.0 - src.a); }
    let ab = dest.a;
    let as_ = src.a;
    var cb = vec3(0.0);
    if (ab > 0.0) { cb = dest.rgb / ab; }
    var cs = vec3(0.0);
    if (as_ > 0.0) { cs = src.rgb / as_; }
    let b = blend_rgb(cb, cs, mode);
    let rgb = (1.0 - ab) * src.rgb + (1.0 - as_) * dest.rgb + as_ * ab * b;
    let a = as_ + ab - as_ * ab;
    return vec4(rgb, a);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let frag = pos.xy;
    let dest = textureLoad(dest_tex, vec2<i32>(frag), 0);
    switch u.op {
        case OP_COMPOSITE: {
            var src = vec4(0.0);
            if ((u.flags & FLAG_LAYER) != 0u) { src = sample_layer(frag); }
            src = src * sample_mask(frag) * u.opacity;
            return composite(dest, src, u.blend);
        }
        case OP_ATOP: {
            let src = sample_layer(frag) * u.opacity;
            return src * dest.a + dest * (1.0 - src.a);
        }
        case OP_MODULATE: {
            return dest * sample_mask(frag);
        }
        case OP_UNPREMULTIPLY: {
            if (dest.a <= 0.0) { return vec4(0.0, 0.0, 0.0, 1.0); }
            return vec4(dest.rgb / dest.a, 1.0);
        }
        case OP_RESTORE: {
            // The base's alpha, kept in the source texture, comes back over the straight colors.
            let cov = textureLoad(layer_tex, vec2<i32>(frag), 0).a;
            return vec4(dest.rgb * cov, cov);
        }
        case OP_ADJUST: {
            // A per-channel (or luminance-indexed) lookup on the straight color, blended and faded, then
            // laid down through the coverage held in the mask texture.
            let cov = sample_mask(frag);
            if (dest.a <= 0.0 || cov <= 0.0) { return dest; }
            let straight = clamp(dest.rgb / dest.a, vec3(0.0), vec3(1.0));
            var adjusted: vec3<f32>;
            if (u.lut_luma > 0.5) {
                let y = clamp(0.2126 * straight.r + 0.7152 * straight.g + 0.0722 * straight.b, 0.0, 1.0);
                adjusted = textureSampleLevel(layer_tex, near, vec2((y * 255.0 + 0.5) / 256.0, 0.5), 0.0).rgb;
            } else {
                adjusted = vec3(
                    textureSampleLevel(layer_tex, near, vec2((straight.r * 255.0 + 0.5) / 256.0, 0.5), 0.0).r,
                    textureSampleLevel(layer_tex, near, vec2((straight.g * 255.0 + 0.5) / 256.0, 0.5), 0.0).g,
                    textureSampleLevel(layer_tex, near, vec2((straight.b * 255.0 + 0.5) / 256.0, 0.5), 0.0).b);
            }
            var blended = adjusted;
            if (u.blend != 0u) { blended = blend_rgb(straight, adjusted, u.blend); }
            let faded = mix(straight, blended, u.opacity);
            let result = vec4(faded * dest.a, dest.a);
            return mix(dest, result, cov);
        }
        case OP_BACKGROUND: {
            // The surround, the document's soft shadow (eight rings, shifted down), and the checkerboard
            // inside the document's rectangle, as the CPU frame draws them.
            let rect = vec4(u.to_layer0.x, u.to_layer0.y, u.to_layer0.z, u.to_layer0.w);
            let tile = max(u.to_layer1.x, 1.0);
            let shadow = u.to_layer1.y;
            var color = vec4(u.to_mask0.x, u.to_mask0.y, u.to_mask0.z, 1.0);
            let inside = frag.x >= rect.x && frag.y >= rect.y && frag.x < rect.x + rect.z && frag.y < rect.y + rect.w;
            if (inside) {
                let cell = floor((frag - rect.xy) / tile);
                let odd = (i32(cell.x) + i32(cell.y)) % 2 == 0;
                let g = select(0.30, 0.35, odd);
                return vec4(g, g, g, 1.0);
            }
            if (shadow > 0.5) {
                let ppx = u.mask_size.x;
                let dx = max(max(rect.x - frag.x, frag.x - (rect.x + rect.z)), 0.0);
                let dy = max(max((rect.y + 3.0 * ppx) - frag.y, frag.y - (rect.y + rect.w + 3.0 * ppx)), 0.0);
                var a = 0.0;
                for (var i = 1; i <= 8; i++) {
                    let spread = f32(i) * 1.8 * ppx;
                    if (dx <= spread && dy <= spread) { a = a + 0.055 * f32(9 - i) / 8.0 * (1.0 - a); }
                }
                color = vec4(color.rgb * (1.0 - a), 1.0);
            }
            return color;
        }
        case OP_DOWNSAMPLE: {
            let base = vec2<i32>(frag) * 2;
            let s = textureLoad(layer_tex, base, 0) + textureLoad(layer_tex, base + vec2(1, 0), 0) + textureLoad(layer_tex, base + vec2(0, 1), 0) + textureLoad(layer_tex, base + vec2(1, 1), 0);
            return s * 0.25;
        }
        default: {
            return textureLoad(layer_tex, vec2<i32>(frag), 0);
        }
    }
}
