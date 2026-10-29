struct Params {
    width: u32,
    height: u32,
    kr: f32,
    kb: f32,
    limited: u32,
    swizzle: u32,
    chroma_mode: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var luma: texture_2d<u32>;
@group(0) @binding(2) var chroma: texture_2d<u32>;
@group(0) @binding(3) var<storage, read_write> out: array<u32>;
@group(0) @binding(4) var page: texture_2d<f32>;

fn chroma_at(cx: i32, cy: i32) -> vec2<f32> {
    let dims = vec2<i32>(textureDimensions(chroma));
    let p = clamp(vec2<i32>(cx, cy), vec2<i32>(0), dims - vec2<i32>(1));
    let c = textureLoad(chroma, vec2<u32>(p), 0);
    return vec2<f32>(f32(c.r), f32(c.g));
}

fn chroma_upsampled(x: u32, y: u32) -> vec2<f32> {
    let ix = i32(x);
    let iy = i32(y);
    let cx = ix / 2;
    let cy = iy / 2;
    if (params.chroma_mode == 0u) {
        return chroma_at(cx, cy);
    }
    let odd_y = (iy & 1) == 1;
    let vy = select(cy - 1, cy + 1, odd_y);
    var h0: vec2<f32>;
    var h1: vec2<f32>;
    if (params.chroma_mode == 1u) {
        let odd_x = (ix & 1) == 1;
        let a = chroma_at(cx, cy);
        let b = chroma_at(cx, vy);
        let a1 = chroma_at(cx + 1, cy);
        let b1 = chroma_at(cx + 1, vy);
        if (odd_x) {
            h0 = 0.5 * (a + a1);
            h1 = 0.5 * (b + b1);
        } else {
            h0 = a;
            h1 = b;
        }
    } else if (params.chroma_mode == 3u) {
        h0 = chroma_at(cx, cy);
        h1 = chroma_at(cx, vy);
    } else {
        let odd_x = (ix & 1) == 1;
        let hx = select(cx - 1, cx + 1, odd_x);
        h0 = 0.75 * chroma_at(cx, cy) + 0.25 * chroma_at(hx, cy);
        h1 = 0.75 * chroma_at(cx, vy) + 0.25 * chroma_at(hx, vy);
    }
    return 0.75 * h0 + 0.25 * h1;
}

fn video(x: u32, y: u32) -> vec3<f32> {
    let yy = f32(textureLoad(luma, vec2<u32>(x, y), 0).r);
    let c = chroma_upsampled(x, y);
    var yf: f32;
    var cb: f32;
    var cr: f32;
    if (params.limited == 1u) {
        yf = (yy - 16.0) * (255.0 / 219.0);
        cb = (c.x - 128.0) * (255.0 / 224.0);
        cr = (c.y - 128.0) * (255.0 / 224.0);
    } else {
        yf = yy;
        cb = c.x - 128.0;
        cr = c.y - 128.0;
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
