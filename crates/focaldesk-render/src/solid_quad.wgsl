struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) local: vec2<f32>,
    @location(3) size: vec2<f32>,
    @location(4) radius: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
    @location(2) size: vec2<f32>,
    @location(3) radius: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    output.local = input.local;
    output.size = input.size;
    output.radius = input.radius;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.radius <= 0.0 {
        return input.color;
    }
    let half_size = input.size * 0.5;
    let q = abs(input.local - half_size) - (half_size - vec2<f32>(input.radius));
    let distance = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - input.radius;
    let coverage = 1.0 - smoothstep(-1.0, 1.0, distance);
    return input.color * coverage;
}
