struct Params {
    width: u32,
    height: u32,
    kr: f32,
    kb: f32,
    limited: u32,
    swizzle: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var luma: texture_2d<u32>;
@group(0) @binding(2) var chroma: texture_2d<u32>;
@group(0) @binding(3) var<storage, read_write> out: array<u32>;
@group(0) @binding(4) var page: texture_2d<f32>;

fn video(x: u32, y: u32) -> vec3<f32> {
    let yy = f32(textureLoad(luma, vec2<u32>(x, y), 0).r);
    let c = textureLoad(chroma, vec2<u32>(x / 2u, y / 2u), 0);
    var yf: f32;
    var cb: f32;
    var cr: f32;
    if (params.limited == 1u) {
        yf = (yy - 16.0) * (255.0 / 219.0);
        cb = (f32(c.r) - 128.0) * (255.0 / 224.0);
        cr = (f32(c.g) - 128.0) * (255.0 / 224.0);
    } else {
        yf = yy;
        cb = f32(c.r) - 128.0;
        cr = f32(c.g) - 128.0;
    }
    let r = yf + 2.0 * (1.0 - params.kr) * cr;
    let b = yf + 2.0 * (1.0 - params.kb) * cb;
    let g = (yf - params.kr * r - params.kb * b) / (1.0 - params.kr - params.kb);
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(255.0));
}

fn pack_bgra(rgb: vec3<f32>) -> u32 {
    let q = vec3<u32>(floor(rgb + vec3<f32>(0.5)));
    return q.b | (q.g << 8u) | (q.r << 16u) | (255u << 24u);
}

fn pack_rgba(rgb: vec3<f32>) -> u32 {
    let q = vec3<u32>(floor(rgb + vec3<f32>(0.5)));
    return q.r | (q.g << 8u) | (q.b << 16u) | (255u << 24u);
}

@compute @workgroup_size(16, 16)
fn convert(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.width || id.y >= params.height) {
        return;
    }
    let rgb = video(id.x, id.y);
    if (params.swizzle == 0u) {
        out[id.y * params.width + id.x] = pack_bgra(rgb);
    } else {
        out[id.y * params.width + id.x] = pack_rgba(rgb);
    }
}

@compute @workgroup_size(16, 16)
fn convert_bmp(@builtin(global_invocation_id) id: vec3<u32>) {
    let quads = params.width / 4u;
    if (id.x >= quads || id.y >= params.height) {
        return;
    }
    var bytes: array<u32, 12>;
    for (var k = 0u; k < 4u; k = k + 1u) {
        let rgb = vec3<u32>(floor(video(id.x * 4u + k, id.y) + vec3<f32>(0.5)));
        bytes[k * 3u] = rgb.b;
        bytes[k * 3u + 1u] = rgb.g;
        bytes[k * 3u + 2u] = rgb.r;
    }
    let row = params.height - 1u - id.y;
    let base = (row * quads + id.x) * 3u;
    for (var w = 0u; w < 3u; w = w + 1u) {
        out[base + w] = bytes[w * 4u] | (bytes[w * 4u + 1u] << 8u) | (bytes[w * 4u + 2u] << 16u) | (bytes[w * 4u + 3u] << 24u);
    }
}

@compute @workgroup_size(16, 16)
fn composite(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.width || id.y >= params.height) {
        return;
    }
    let under = video(id.x, id.y);
    let over = textureLoad(page, vec2<u32>(id.x, id.y), 0);
    let rgb = over.rgb * 255.0 + (1.0 - over.a) * under;
    out[id.y * params.width + id.x] = pack_bgra(rgb);
}
