#version 310 es
precision highp float;

layout (location = 0) in vec2 in_uv;
layout (location = 0) out vec4 out_color;

// NV12 straight from the hardware decoder: luma plane, interleaved half-resolution chroma plane.
layout (set = 0, binding = 0) uniform sampler2D in_luma;
layout (set = 1, binding = 0) uniform sampler2D in_chroma;

void main()
{
    // BT.709, limited range (the PC's hardware H.264 encoder output)
    float y = (texture(in_luma, in_uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    vec2 c = (texture(in_chroma, in_uv).rg - vec2(128.0 / 255.0)) * (255.0 / 224.0);
    vec3 rgb = clamp(vec3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x), 0.0, 1.0);

    bvec3 cutoff = lessThan(rgb, vec3(0.04045));
    vec3 higher = pow((rgb + vec3(0.055)) / vec3(1.055), vec3(2.4));
    vec3 lower = rgb / vec3(12.92);

    out_color = vec4(mix(higher, lower, cutoff), 1.0);
}
