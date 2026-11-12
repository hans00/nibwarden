fn main() {
    // Translations are compiled into the binary (phase2.md §5.5): nothing to install, and
    // the same on every platform. Without a default context, a string is translated the
    // same in every component.
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/main.slint", config).expect("compile ui/main.slint");
    println!("cargo:rerun-if-changed=lang");

    // Windows: the program's icon, in Explorer, the taskbar and Alt-Tab.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = "../../packaging/windows/nibwarden.ico";
        println!("cargo:rerun-if-changed={icon}");
        winresource::WindowsResource::new()
            .set_icon(icon)
            .compile()
            .expect("compile the Windows resources");
    }
}
