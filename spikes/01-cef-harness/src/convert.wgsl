struct Params {
    origin: vec2<u32>,
    pad: vec2<u32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> p: Params;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4<f32>(corners[i], 0.0, 1.0);
}

fn rgb(x: u32, y: u32) -> vec3<u32> {
    let c = textureLoad(src, vec2<i32>(i32(p.origin.x + x), i32(p.origin.y + y)), 0);
    return vec3<u32>(round(c.rgb * 255.0));
}

fn luma(c: vec3<u32>) -> u32 {
    let v = (5983u * c.x + 20127u * c.y + 2032u * c.z + 524544u) >> 9u;
    return (v + 32u) >> 6u;
}

fn chroma_pair(a: vec3<u32>, b: vec3<u32>) -> vec2<i32> {
    let s = vec3<i32>(a) + vec3<i32>(b);
    let cb = -3298 * s.x - 11094 * s.y + 14392 * s.z;
    let cr = 14392 * s.x - 13073 * s.y - 1320 * s.z;
    return vec2<i32>((cb + 8388608 + 512) >> 10u, (cr + 8388608 + 512) >> 10u);
}

@fragment
fn fs_y(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = rgb(u32(pos.x), u32(pos.y));
    return vec4<f32>(f32(luma(c)) / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_c(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = u32(pos.x) * 2u;
    let y = u32(pos.y) * 2u;
    let sum = chroma_pair(rgb(x, y), rgb(x + 1u, y)) + chroma_pair(rgb(x, y + 1u), rgb(x + 1u, y + 1u));
    let o = (sum + vec2<i32>(64, 64)) >> vec2<u32>(7u, 7u);
    return vec4<f32>(f32(o.x) / 255.0, f32(o.y) / 255.0, 0.0, 1.0);
}
