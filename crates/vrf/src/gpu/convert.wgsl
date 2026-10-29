struct Params {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    spec: u32,
    bits: u32,
}

var<immediate> p: Params;

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var y8: texture_storage_2d<r8uint, write>;
@group(1) @binding(1) var c8: texture_storage_2d<rg8uint, write>;
@group(1) @binding(2) var y16: texture_storage_2d<r16uint, write>;
@group(1) @binding(3) var c16: texture_storage_2d<rg16uint, write>;

fn load(x: u32, y: u32) -> vec3<u32> {
    let rgb = textureLoad(src, vec2<i32>(i32(p.x + x), i32(p.y + y)), 0).rgb;
    return vec3<u32>(round(rgb * 255.0));
}

fn luma_swscale(c: vec3<u32>) -> u32 {
    let v = (5983u * c.x + 20127u * c.y + 2032u * c.z + (0x801u << 8u)) >> 9u;
    let shift = 14u - p.bits;
    return (v + (1u << (shift - 1u))) >> shift;
}

fn luma_spec(c: vec3<u32>) -> u32 {
    let m = 1u << (p.bits - 8u);
    let w = 2126u * c.x + 7152u * c.y + 722u * c.z;
    return (219u * m * w + 16u * m * 2550000u + 1275000u) / 2550000u;
}

fn pair_swscale(a: vec3<u32>, b: vec3<u32>) -> vec2<i32> {
    let s = vec3<i32>(a) + vec3<i32>(b);
    let cb = -3298 * s.x - 11094 * s.y + 14392 * s.z;
    let cr = 14392 * s.x - 13073 * s.y - 1320 * s.z;
    return vec2<i32>((cb + (128 << 16u) + 512) >> 10u, (cr + (128 << 16u) + 512) >> 10u);
}

fn chroma_swscale(x0: u32, y0: u32) -> vec2<u32> {
    let top = pair_swscale(load(x0, y0), load(x0 + 1u, y0));
    let bottom = pair_swscale(load(x0, y0 + 1u), load(x0 + 1u, y0 + 1u));
    let shift = 15u - p.bits;
    let o = (top + bottom + vec2<i32>(i32(1u << (shift - 1u)))) >> vec2<u32>(shift);
    return vec2<u32>(o);
}

fn scaled_div(t: u32, k: u32, c: u32, d: u32) -> u32 {
    let hi = t >> 16u;
    let lo = t & 0xffffu;
    let big = k << 16u;
    return hi * (big / d) + (hi * (big % d) + k * lo + c) / d;
}

fn chroma_spec(x0: u32, y0: u32) -> vec2<u32> {
    let xl = select(x0 - 1u, 0u, x0 == 0u);
    var s = vec3<u32>(0u);
    for (var dy = 0u; dy < 2u; dy++) {
        s += load(xl, y0 + dy) + 2u * load(x0, y0 + dy) + load(x0 + 1u, y0 + dy);
    }
    let m = 1u << (p.bits - 8u);
    let tb = 2126u * (2040u - s.x) + 7152u * (2040u - s.y) + 9278u * s.z;
    let tr = 7874u * s.x + 7152u * (2040u - s.y) + 722u * (2040u - s.z);
    return vec2<u32>(
        16u * m + scaled_div(tb, 56u * m, 255u * 18556u, 2u * 255u * 18556u),
        16u * m + scaled_div(tr, 56u * m, 255u * 15748u, 2u * 255u * 15748u),
    );
}

fn luma_at(x: u32, y: u32) -> u32 {
    let c = load(x, y);
    return select(luma_swscale(c), luma_spec(c), p.spec == 1u);
}

fn chroma_at(cx: u32, cy: u32) -> vec2<u32> {
    return select(chroma_swscale(cx * 2u, cy * 2u), chroma_spec(cx * 2u, cy * 2u), p.spec == 1u);
}

@compute @workgroup_size(16, 16)
fn convert8(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x * 2u >= p.width || id.y * 2u >= p.height {
        return;
    }
    for (var dy = 0u; dy < 2u; dy++) {
        for (var dx = 0u; dx < 2u; dx++) {
            let at = vec2<u32>(id.x * 2u + dx, id.y * 2u + dy);
            textureStore(y8, at, vec4<u32>(luma_at(at.x, at.y), 0u, 0u, 0u));
        }
    }
    textureStore(c8, id.xy, vec4<u32>(chroma_at(id.x, id.y), 0u, 0u));
}

@compute @workgroup_size(16, 16)
fn convert16(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x * 2u >= p.width || id.y * 2u >= p.height {
        return;
    }
    for (var dy = 0u; dy < 2u; dy++) {
        for (var dx = 0u; dx < 2u; dx++) {
            let at = vec2<u32>(id.x * 2u + dx, id.y * 2u + dy);
            textureStore(y16, at, vec4<u32>(luma_at(at.x, at.y) << 6u, 0u, 0u, 0u));
        }
    }
    textureStore(c16, id.xy, vec4<u32>(chroma_at(id.x, id.y) << vec2<u32>(6u), 0u, 0u));
}
