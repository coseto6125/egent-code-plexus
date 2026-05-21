fn main() {
    let src_dir = std::path::Path::new("src");

    let mut c_config = cc::Build::new();
    c_config.include(src_dir);
    c_config
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs");
    #[cfg(target_env = "msvc")]
    c_config.flag("-utf-8");

    let parser_path = src_dir.join("parser.c");
    c_config.file(&parser_path);
    c_config.compile("tree-sitter-vue-parser");
    println!("cargo:rerun-if-changed={}", parser_path.to_string_lossy());

    // scanner.cc is the external scanner (handles embedded blocks)
    let scanner_path = src_dir.join("scanner.cc");
    let mut cpp_config = cc::Build::new();
    cpp_config.include(src_dir).cpp(true);
    cpp_config
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable");
    cpp_config.file(&scanner_path);
    cpp_config.compile("tree-sitter-vue-scanner");
    println!("cargo:rerun-if-changed={}", scanner_path.to_string_lossy());
}
