struct WarpParams {
    output_size: vec2<f32>,
    inner_radius: f32,
    outer_radius: f32,
    start_angle: f32,
    angle_span: f32,
    direction: f32,
    supersample: u32,
    _padding: f32,
};

@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var source_smp: sampler;
@group(0) @binding(2) var<uniform> params: WarpParams;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0)
    );
    var result: VertexOut;
    result.position = vec4<f32>(positions[index], 0.0, 1.0);
    result.uv = (positions[index] + vec2<f32>(1.0)) * 0.5;
    return result;
}

fn sample_disk(pixel: vec2<f32>) -> vec4<f32> {
    let half_size = params.output_size * 0.5;
    let radius_px = min(half_size.x, half_size.y) * params.outer_radius;
    let delta = pixel - half_size;
    let radius = length(delta) / radius_px;
    if radius > 1.0 || radius < params.inner_radius {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let angle = atan2(delta.x, -delta.y);
    let u = fract(params.direction * (angle - params.start_angle) / params.angle_span + 1.0);
    let v = 1.0 - (radius - params.inner_radius) / (1.0 - params.inner_radius);
    return textureSampleLevel(source_tex, source_smp, vec2<f32>(u, v), 0.0);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let count = max(params.supersample, 1u);
    var color = vec4<f32>(0.0);
    for (var y = 0u; y < count; y += 1u) {
        for (var x = 0u; x < count; x += 1u) {
            let offset = (vec2<f32>(f32(x), f32(y)) + 0.5) / f32(count) - 0.5;
            color += sample_disk(input.position.xy + offset);
        }
    }
    return vec4<f32>((color / f32(count * count)).rgb, 1.0);
}
