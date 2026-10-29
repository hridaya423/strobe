var<immediate> frame: u32;

@group(0) @binding(0) var out: texture_storage_2d<bgra8unorm, write>;

const BARS = array<vec3<f32>, 8>(
    vec3<f32>(1.0, 1.0, 1.0),
    vec3<f32>(1.0, 1.0, 0.0),
    vec3<f32>(0.0, 1.0, 1.0),
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(1.0, 0.0, 1.0),
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, 0.0),
);

fn hash(v: u32) -> u32 {
    var x = v;
    x ^= x >> 16u;
    x *= 0x7feb352du;
    x ^= x >> 15u;
    x *= 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

fn pattern(x: u32, y: u32, size: vec2<u32>) -> vec3<f32> {
    if y >= size.y * 4u / 5u {
        let bit = x * 16u / size.x;
        return vec3<f32>(f32((frame >> (15u - bit)) & 1u));
    }
    let noise = (f32(hash(x + y * size.x + frame * 7919u) & 255u) / 255.0 - 0.5) * 0.04;
    var rgb = vec3<f32>(f32(x) / f32(size.x - 1u));
    let box_x = (frame * 8u) % (size.x - 160u);
    if y < size.y * 3u / 5u {
        rgb = BARS[x * 8u / size.x];
    } else if x >= box_x && x < box_x + 160u && y >= size.y * 3u / 5u + 28u && y < size.y * 3u / 5u + 188u {
        rgb = vec3<f32>(1.0, 0.5, 0.0);
    }
    return clamp(rgb + noise, vec3<f32>(0.0), vec3<f32>(1.0));
}

@compute @workgroup_size(8, 8)
fn render(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(out);
    textureStore(out, id.xy, vec4<f32>(pattern(id.x, id.y, size), 1.0));
}
