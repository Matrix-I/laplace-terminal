void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    float s = 0.85 + 0.15 * sin(fragCoord.y * 1.5 + iTime * 4.0);
    fragColor = texture(iChannel0, uv) * s;
}
