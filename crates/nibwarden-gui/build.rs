fn main() {
    // Translations are compiled into the binary (phase2.md §5.5): nothing to install, and
    // the same on every platform. Without a default context, a string is translated the
    // same in every component.
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/main.slint", config).expect("compile ui/main.slint");
    println!("cargo:rerun-if-changed=lang");
}
