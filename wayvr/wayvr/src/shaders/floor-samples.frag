#version 450
layout(location = 0) in vec3 in_tint;
layout(location = 0) out vec4 out_color;
void main() {
    out_color = vec4(in_tint * 0.85, 0.85);
}
