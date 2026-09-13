struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = positions[vertex_index];

    var output: VertexOutput;
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.uv = position * 0.5 + vec2<f32>(0.5);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let horizon = vec3<f32>(0.02, 0.08, 0.18);
    let zenith = vec3<f32>(0.01, 0.015, 0.045);
    let glow = vec3<f32>(0.0, 0.35, 0.55) * max(0.0, 1.0 - distance(input.uv, vec2<f32>(0.5, 0.78)) * 2.2);
    let background = mix(horizon, zenith, clamp(input.uv.y, 0.0, 1.0));
    return vec4<f32>(background + glow * 0.35, 1.0);
}
