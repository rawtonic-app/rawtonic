//! Validate every shipped WGSL module and the export precision variant, without a GPU.
#[test]
fn preview_export_and_display_shaders_validate() {
    let main = include_str!("../../crates/gpu/src/shaders/shader.wgsl");
    let high_precision = main.replace("rgba8unorm, write>", "rgba16float, write>");
    for (name, source) in [
        ("preview", main),
        ("export-f16", high_precision.as_str()),
        (
            "blur",
            include_str!("../../crates/gpu/src/shaders/blur.wgsl"),
        ),
        (
            "flare",
            include_str!("../../crates/gpu/src/shaders/flare.wgsl"),
        ),
        (
            "display",
            include_str!("../../crates/gpu/src/shaders/display.wgsl"),
        ),
    ] {
        let module = wgpu::naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(source)));
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}
