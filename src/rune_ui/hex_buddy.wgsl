// Calm character from site/src/shout/gpu.js, without the homepage's show effects.
// Art colours are display-encoded; the overlay receives premultiplied alpha.
override linear_target: bool = false;

struct Uniforms {
    output: vec4f,
    rect: vec4f,
    rig: vec4f, // mouth, blink, tilt, bob
    gaze: vec4f,
}
@group(0) @binding(0) var<uniform> u: Uniforms;

struct Vertex {
    @builtin(position) position: vec4f,
    @location(0) local: vec2f,
}

@vertex fn buddy_vertex(@builtin(vertex_index) index: u32) -> Vertex {
    let corners = array<vec2f, 6>(vec2f(0, 0), vec2f(1, 0), vec2f(0, 1), vec2f(0, 1), vec2f(1, 0), vec2f(1, 1));
    let corner = corners[index];
    let point = u.rect.xy + corner * u.rect.zw;
    let radius = max(min(u.rect.z, u.rect.w) * 0.39, 0.001);
    return Vertex(vec4f(point.x / u.output.x * 2.0 - 1.0, 1.0 - point.y / u.output.y * 2.0, 0.0, 1.0), (corner - 0.5) * u.rect.zw / radius);
}

fn step_between(a: f32, b: f32, x: f32) -> f32 {
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
fn cov(distance: f32, aa: f32) -> f32 { return step_between(aa, -aa, distance); }
fn rotate(angle: f32) -> mat2x2f {
    let c = cos(angle); let s = sin(angle);
    return mat2x2f(c, s, -s, c);
}
fn ellipse(p: vec2f, r: vec2f) -> f32 {
    let k0 = length(p / r);
    let k1 = length(p / (r * r));
    return k0 * (k0 - 1.0) / max(k1, 0.0001);
}
fn segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let pa = p - a; let ba = b - a;
    return length(pa - ba * clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0));
}
fn round_box(p: vec2f, b: vec2f, r: f32) -> f32 {
    let q = abs(p) - b + r;
    return length(max(q, vec2f(0))) + min(max(q.x, q.y), 0.0) - r;
}

fn mouth_at(p: vec2f, mouth: f32, aa: f32) -> vec4f {
    let mw = (0.165 + 0.255 * mouth) * mix(1.0, 1.08, mouth);
    let mh = 0.24 * mouth;
    let mux = clamp(p.x / mw, -1.0, 1.0);
    let mid = 0.038 * (1.0 - mux * mux) * (1.0 - step_between(0.0, 0.38, mouth));
    let top = mid - mh * 0.80 * pow(max(0.0, 1.0 - mux * mux), 0.62);
    let bottom = mid + mh * (0.94 + 0.12 * step_between(0.35, 1.0, mouth)) * pow(max(0.0, 1.0 - mux * mux), 0.56);
    let cavity = max(abs(p.x) - mw, max(top - p.y, p.y - bottom)) + 0.016 * (1.0 - step_between(0.0, 0.12, mouth));
    let open = step_between(0.010, 0.048, mouth);
    let line = cov(abs(p.y - mid) - (0.014 + 0.008 * mouth), aa) * step_between(mw + aa, mw - aa, abs(p.x)) * (1.0 - open);
    let inside = cov(cavity, aa) * open;
    var color = mix(vec3f(0.16, 0.03, 0.05), vec3f(0.08, 0.01, 0.015), step_between(bottom + 0.0001, top - 0.0001, p.y));

    let tongue_y = bottom + 0.015 - mh * 0.22;
    let tongue_r = vec2f(mw * 0.46, max(0.03, mh * 0.44));
    let tongue = min(ellipse(p - vec2f(-mw * 0.22, tongue_y), tongue_r), ellipse(p - vec2f(mw * 0.22, tongue_y), tongue_r));
    let groove = exp(-pow(p.x / max(mw * 0.12, 0.01), 2.0)) * step_between(bottom, tongue_y - mh * 0.25 - 0.0001, p.y);
    let tongue_grad = clamp((p.y - (tongue_y - mh * 0.35)) / max(mh * 0.5, 0.02), 0.0, 1.0);
    var tongue_color = mix(vec3f(0.94, 0.34, 0.38), vec3f(0.64, 0.11, 0.15), tongue_grad) * (1.0 - 0.30 * groove);
    let spec = exp(-pow((p.x - mw * 0.14) / max(mw * 0.14, 0.01), 2.0) - pow((p.y - (tongue_y - mh * 0.12)) / max(mh * 0.10, 0.01), 2.0));
    tongue_color += vec3f(1.0, 0.85, 0.86) * spec * 0.35;
    color = mix(color, tongue_color, cov(tongue, aa) * step_between(0.03, 0.12, mouth));

    let tooth_h = min(0.068, mh * 0.32);
    let tooth_notch = 0.005 * pow(abs(sin(mux * 3.14159 * 3.5)), 3.0);
    let tooth_shade = 1.0 - 0.18 * pow(abs(mux), 1.8) - 0.10 * abs(sin(mux * 3.14159 * 3.5));
    color = mix(color, vec3f(0.97, 0.97, 0.96) * tooth_shade, cov(p.y - (top + tooth_h - tooth_notch), aa) * step_between(0.04, 0.12, mouth));
    color *= 0.65 + 0.35 * step_between(0.0, 0.035, -cavity);
    let lip = cov(abs(cavity) - 0.011, aa) * open;
    color = mix(color, vec3f(0.86, 0.86, 0.88), lip * (1.0 - inside * 0.65));
    color = mix(color, vec3f(0.95, 0.95, 0.93), line);
    return vec4f(color, clamp(inside + line + lip * 0.55, 0.0, 1.0));
}

fn for_target(color: vec3f, alpha: f32) -> vec4f {
    let display = clamp(color, vec3f(0), vec3f(1));
    if (linear_target) {
        let linear = select(pow((display + 0.055) / 1.055, vec3f(2.4)), display / 12.92, display <= vec3f(0.04045));
        return vec4f(linear * alpha, alpha);
    }
    return vec4f(display * alpha, alpha);
}

@fragment fn buddy_fragment(vertex: Vertex) -> @location(0) vec4f {
    let mouth = u.rig.x;
    let sy = 1.0 + mouth * 0.05;
    let scale = vec2f(1.0 / pow(sy, 0.7), sy);
    let p = (rotate(-u.rig.z) * (vertex.local - vec2f(0, u.rig.w))) / scale;
    let aa = max(max(fwidth(p.x), fwidth(p.y)) * 1.15, 0.001);
    let rx = 0.80 * (1.0 - 0.08 * p.y);
    let distance = ellipse(p, vec2f(rx, 1.0));
    let cover = cov(distance, aa);
    let nxy = clamp(p / vec2f(rx, 1.0) * 0.86, vec2f(-0.96), vec2f(0.96));
    let normal = normalize(vec3f(nxy, sqrt(max(0.03, 1.0 - dot(nxy, nxy)))));
    let lift = clamp(0.52 - 0.36 * normal.y - 0.28 * normal.x, 0.0, 1.0);
    var color = mix(vec3f(0.018, 0.018, 0.022), vec3f(0.11, 0.115, 0.135), pow(lift, 1.5));

    let arc = (atan2(p.y, p.x) + 2.08) * 0.76;
    let depth = -distance - 0.115;
    let window = cov(round_box(vec2f(arc, depth), vec2f(0.21, 0.028), 0.026) - 0.0035, 0.0115) * step_between(0.0, -0.18, p.y);
    let catchlight = cov(round_box(vec2f(arc + 0.29, depth), vec2f(0.036, 0.024), 0.022) - 0.003, 0.011) * 0.80;
    let sheen = exp(-arc * arc / 0.14 - depth * depth / 0.012) * 0.20;
    color += vec3f(0.95, 0.97, 1.0) * (window * 0.72 + catchlight * 0.56 + sheen);
    let rim = exp(-pow(abs(distance + 0.018), 2.0) / 0.0014);
    color += vec3f(0.92, 0.94, 0.98) * clamp(0.65 - 0.35 * normal.y, 0.2, 1.0) * 0.78 * rim;

    let eye_open = clamp(1.0 - u.rig.y, 0.04, 1.0);
    for (var side = -1; side <= 1; side += 2) {
        let center = vec2f(f32(side) * 0.275, -0.125);
        let ep = p - center;
        const er = vec2f(0.148, 0.208);
        let top = -er.y + (1.0 - eye_open) * 1.65 * er.y;
        let bottom = er.y - ((1.0 - eye_open) * 0.55 + 0.20) * er.y - ep.x * ep.x * 1.1;
        let eye_cover = cov(max(ellipse(ep, er), max(top - ep.y, ep.y - bottom)), aa);
        let crease = cov(abs(ep.y - (top - 0.024)) - 0.008, aa) * step_between(er.x * 1.05, er.x * 0.5, abs(ep.x)) * (1.0 - eye_cover);
        color = mix(color, color * 0.35, crease * 0.65);
        let sphere = clamp(1.0 - dot(ep / er, ep / er) * 0.28, 0.65, 1.0);
        var eye_color = vec3f(0.97, 0.97, 0.95) * sphere * (0.72 + 0.28 * step_between(top - 0.01, top + 0.07, ep.y));
        let pupil = ep - u.gaze.xy * vec2f(0.062, 0.072);
        eye_color = mix(eye_color, vec3f(0.03, 0.03, 0.04), cov(length(pupil) - 0.076, aa));
        let glint1 = cov(length(pupil - vec2f(-0.022, -0.025)) - 0.019, aa);
        let glint2 = cov(length(pupil - vec2f(0.020, 0.022)) - 0.009, aa);
        eye_color = mix(eye_color, vec3f(1), clamp(glint1 + glint2 * 0.85, 0.0, 1.0));
        color = mix(color, eye_color, eye_cover);

        let bc = vec2f(center.x, -0.43);
        let bp = p - bc;
        let proj = clamp(bp.x / 0.142, -1.0, 1.0);
        let taper = mix(0.044, 0.026, (proj * f32(side) + 1.0) * 0.5);
        let brow = segment(p, bc - vec2f(0.142, 0), bc + vec2f(0.142, 0)) - taper;
        color = mix(color, vec3f(0.97, 0.97, 0.95), cov(brow, aa));
    }
    let lips = mouth_at(p - vec2f(0, 0.31), mouth, aa);
    color = mix(color, lips.rgb, lips.a);
    return for_target(color, cover);
}
