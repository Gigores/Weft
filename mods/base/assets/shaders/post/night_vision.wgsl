// Night vision: the dark lifted as if every place were lit; what is bright already stays.
fn apply(p: PostPixel) -> vec4<f32> {
    let c = max(p.color.rgb, vec3(0.0));
    let lifted = max(c, sqrt(c) * 0.8);
    return vec4(mix(c, lifted, p.strength), 1.0);
}
