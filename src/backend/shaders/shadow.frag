precision highp float;
//_DEFINES

varying vec2 v_coords;
uniform sampler2D tex;
uniform float alpha;
uniform vec2 rect_size;
uniform vec2 caster_size;
uniform vec2 caster_center;
uniform float corner_radius;
uniform float spread;
uniform float shadow_radius;
uniform vec4 shadow_color;

float rounded_rect_sdf(vec2 p, vec2 size, float radius) {
    vec2 half_size = size * 0.5;
    vec2 q = abs(p) - (half_size - vec2(radius));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

float erf_approx(float x) {
    float s = sign(x);
    float a = abs(x);
    float t = 1.0 / (1.0 + 0.3275911 * a);
    float y = 1.0 - (((((1.061405429 * t - 1.453152027) * t)
        + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * exp(-a * a);
    return s * y;
}

void main() {
    vec2 size = max(rect_size, vec2(1.0));
    vec2 caster = max(caster_size, vec2(1.0));
    float radius = min(max(corner_radius, 0.0), min(caster.x, caster.y) * 0.5);
    vec2 p = v_coords * size - caster_center;
    float dist = rounded_rect_sdf(p, caster, radius);

    float blur = max(shadow_radius, 1.0);
    float outset = max(spread, 0.0);
    if (max(dist, 0.0) >= outset + blur * 3.0) {
        discard;
    }

    float sigma = max(blur * 0.5, 0.5);
    float falloff = 0.5 * (
        1.0 - erf_approx((dist - outset) / (sigma * 1.41421356))
    );
    float a = shadow_color.a * alpha * falloff;
    if (a <= 0.003) {
        discard;
    }
    gl_FragColor = vec4(shadow_color.rgb * a, a);
}
