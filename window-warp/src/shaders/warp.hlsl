// Rectangle -> disk (polar) warp.
//
// The captured window is wrapped around the disk: the source x axis becomes the
// angle (increasing clockwise from `start_angle`) and the source y axis becomes
// the radius, with the top row of the window on the outer rim and the bottom row
// on the inner hole.

cbuffer WarpParams : register(b0)
{
    float2 output_size;   // render target size in pixels
    float inner_radius;   // hole radius, fraction of outer radius (0..1)
    float outer_radius;   // outer radius, fraction of half the shortest side

    float start_angle;    // radians, clockwise from 12 o'clock, maps to source x = 0
    float angle_span;     // radians of arc covered by the full source width
    float direction;      // +1 clockwise, -1 counter-clockwise
    uint supersample;     // sqrt of the samples per pixel (1 = off)

    float4 background;    // color outside the ring
};

Texture2D<float4> source_tex : register(t0);
SamplerState source_smp : register(s0);

static const float TAU = 6.28318530718;

struct VSOut
{
    float4 pos : SV_POSITION;
    float2 uv : TEXCOORD0;
};

// Fullscreen triangle, no vertex buffer needed.
VSOut vs_main(uint vid : SV_VertexID)
{
    VSOut o;
    o.uv = float2((vid << 1) & 2, vid & 2);
    o.pos = float4(o.uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return o;
}

// Maps one point of the render target to the source window, or returns the
// background when the point falls outside the ring.
float4 sample_disk(float2 pixel)
{
    float2 half_size = output_size * 0.5;
    float radius_px = min(half_size.x, half_size.y) * outer_radius;

    float2 d = pixel - half_size;
    float r = length(d) / radius_px;
    if (r > 1.0 || r < inner_radius)
    {
        return background;
    }

    // Angle measured clockwise from 12 o'clock (y grows downwards on screen).
    float angle = atan2(d.x, -d.y);
    float u = frac((direction * (angle - start_angle) / angle_span) + 1.0);

    // Outer rim is the top of the window, inner hole is its bottom.
    float v = 1.0 - (r - inner_radius) / (1.0 - inner_radius);

    return source_tex.SampleLevel(source_smp, float2(u, v), 0);
}

float4 ps_main(VSOut input) : SV_TARGET
{
    uint n = max(supersample, 1u);
    if (n == 1u)
    {
        return float4(sample_disk(input.pos.xy).rgb, 1.0);
    }

    // Uniform grid of sub-samples inside the pixel footprint: the polar mapping
    // stretches the source heavily near the hole, so plain bilinear aliases badly.
    float4 acc = 0.0;
    float step = 1.0 / float(n);
    for (uint sy = 0u; sy < n; ++sy)
    {
        for (uint sx = 0u; sx < n; ++sx)
        {
            float2 offset = (float2(sx, sy) + 0.5) * step - 0.5;
            acc += sample_disk(input.pos.xy + offset);
        }
    }
    acc /= float(n * n);
    return float4(acc.rgb, 1.0);
}
