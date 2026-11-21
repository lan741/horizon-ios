//
//  Shaders.metal
//  HorizonPad
//
//  Matches Renderer.swift's vertex layout:
//    attribute(0) float3 position
//    attribute(1) float2 uv
//    attribute(2) float3 normal
//    attribute(3) float4 color
//    attribute(4) float  texture slice index
//    attribute(5) float  v_delta  (V offset of the fade target inside the strip)
//    attribute(6) float  blend    (0 = current frame, 1 = next frame)
//
//  Textures live in one `texture2d_array`; a slice index of 65535 (the Rust
//  side's "no texture") falls back to vertex colour.
//
#include <metal_stdlib>
using namespace metal;

struct VertexIn {
    float3 position [[attribute(0)]];
    float2 uv       [[attribute(1)]];
    float3 normal   [[attribute(2)]];
    float4 color    [[attribute(3)]];
    float texture   [[attribute(4)]];
    float v_delta   [[attribute(5)]];
    float blend     [[attribute(6)]];
};

struct CameraUniforms {
    float4x4 viewProjection;
    float4 cameraPosition;
    float4 fogColor;
    float4 params; // x fog start, y fog end, z time, w texture count
};

struct VertexOut {
    float4 position [[position]];
    float2 uv;
    float3 normal;
    float4 color;
    float texture;
    float v_delta;
    float blend;
    float distance;
};

vertex VertexOut hz_vertex(VertexIn in [[stage_in]],
                           constant CameraUniforms &uniforms [[buffer(1)]])
{
    VertexOut out;
    out.position = uniforms.viewProjection * float4(in.position, 1.0);
    out.normal = in.normal;
    out.uv = in.uv;
    out.color = in.color;
    out.texture = in.texture;
    out.v_delta = in.v_delta;
    out.blend = in.blend;
    out.distance = length(in.position - uniforms.cameraPosition.xyz);
    return out;
}

fragment float4 hz_fragment(VertexOut in [[stage_in]],
                            constant CameraUniforms &uniforms [[buffer(1)]],
                            texture2d_array<float> atlas [[texture(0)]],
                            sampler atlasSampler [[sampler(0)]])
{
    // Two-light shading: a sky term and a ground bounce. Per-block variation
    // is already baked into vertex colour by the engine.
    float3 n = normalize(in.normal);
    float sky = max(dot(n, normalize(float3(0.45, 1.0, 0.25))), 0.0);
    float bounce = max(dot(n, normalize(float3(-0.3, -1.0, -0.2))), 0.0) * 0.25;
    float light = clamp(0.42 + sky * 0.62 + bounce, 0.0, 1.35);

    float3 faceTint = float3(1.0);
    if (n.y > 0.5)            { faceTint = float3(1.02, 1.02, 1.0); }
    else if (n.y < -0.5)      { faceTint = float3(0.82, 0.82, 0.84); }
    else if (abs(n.x) > 0.5)  { faceTint = float3(0.92, 0.92, 0.94); }

    float3 rgb = in.color.rgb * faceTint;

    // Texture slice 65535 means the quad has no texture: keep the flat colour.
    uint slice = uint(in.texture + 0.5);
    if (slice < uint(uniforms.params.w) && in.texture < 65000.0) {
        float4 texel = atlas.sample(atlasSampler, in.uv, slice);
        // Animated textures with `interpolate: true`: crossfade toward the next
        // frame. Two nearest samples keep the pixels crisp; the mix is what
        // smooths the transition.
        if (in.blend > 0.0) {
            float2 target = in.uv + float2(0.0, in.v_delta);
            texel = mix(texel, atlas.sample(atlasSampler, target, slice), in.blend);
        }
        // Alpha-cutout behaviour: MC uses binary alpha for leaves, glass, grass.
        if (texel.a < 0.35) {
            discard_fragment();
        }
        rgb *= texel.rgb;
    }

    rgb *= light;

    float fogStart = uniforms.params.x;
    float fogEnd = uniforms.params.y;
    float fog = clamp((in.distance - fogStart) / max(fogEnd - fogStart, 0.001), 0.0, 1.0);
    rgb = mix(rgb, uniforms.fogColor.rgb, fog * fog);

    return float4(rgb, in.color.a);
}