#version 450

layout(location = 0) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

void main() {
    vec2 position = (in_uv - 0.5) * 8.0;
    vec2 distance_to_line = abs(mod(position + 0.25, 0.5) - 0.25);
    vec2 antialias = fwidth(position) * 0.5;
    vec2 lines = 1.0 - smoothstep(vec2(0.00075) - antialias,
                                 vec2(0.00075) + antialias, distance_to_line);
    float fade = 1.0 - smoothstep(2.8, 4.0, length(position));
    float alpha = max(lines.x, lines.y) * fade * 0.6;
    out_color = vec4(vec3(0.22) * alpha, alpha);
}
