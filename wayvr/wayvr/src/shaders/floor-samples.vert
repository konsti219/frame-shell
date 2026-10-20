#version 450
layout(location = 0) in vec2 in_pos;
layout(location = 0) out vec3 out_tint;
layout(set = 0, binding = 0) uniform Samples { vec4 points[256]; } samples;
void main() {
    vec4 point = samples.points[gl_InstanceIndex];
    vec2 centre = floor((point.xy / 8.0 + 0.5) * 1024.0) + 0.5;
    vec2 pixel = centre + (in_pos - 0.5) * 3.0;
    gl_Position = vec4(pixel / 512.0 - 1.0, 0.0, 1.0);
    out_tint = point.z > 1.5 ? vec3(0.05, 0.8, 0.15)
             : point.z > 0.5 ? vec3(1.0, 0.35, 0.02) : vec3(0.7, 0.03, 0.03);
}
