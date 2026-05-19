use cgn_analyzer::{
    c::parser::CProvider, c_sharp::parser::CSharpProvider, cpp::parser::CppProvider,
    dart::parser::DartProvider, go::parser::GoProvider, java::parser::JavaProvider,
    javascript::parser::JavaScriptProvider, kotlin::parser::KotlinProvider,
    php::parser::PhpProvider, python::parser::PythonProvider, ruby::parser::RubyProvider,
    rust::parser::RustProvider, swift::parser::SwiftProvider,
    typescript::parser::TypeScriptProvider,
};

// Test binary 列舉所有 LanguageProvider 構造，Box<dyn Fn> 型別冗長但平鋪直敘為佳。
#[allow(clippy::type_complexity)]
fn main() {
    let providers: Vec<(&str, Box<dyn Fn() -> anyhow::Result<()>>)> = vec![
        ("C", Box::new(|| CProvider::new().map(|_| ()))),
        ("CSharp", Box::new(|| CSharpProvider::new().map(|_| ()))),
        ("Cpp", Box::new(|| CppProvider::new().map(|_| ()))),
        ("Dart", Box::new(|| DartProvider::new().map(|_| ()))),
        ("Go", Box::new(|| GoProvider::new().map(|_| ()))),
        ("Java", Box::new(|| JavaProvider::new().map(|_| ()))),
        (
            "JavaScript",
            Box::new(|| JavaScriptProvider::new().map(|_| ())),
        ),
        ("Kotlin", Box::new(|| KotlinProvider::new().map(|_| ()))),
        ("PHP", Box::new(|| PhpProvider::new().map(|_| ()))),
        ("Python", Box::new(|| PythonProvider::new().map(|_| ()))),
        ("Ruby", Box::new(|| RubyProvider::new().map(|_| ()))),
        ("Rust", Box::new(|| RustProvider::new().map(|_| ()))),
        ("Swift", Box::new(|| SwiftProvider::new().map(|_| ()))),
        (
            "TypeScript",
            Box::new(|| TypeScriptProvider::new().map(|_| ())),
        ),
    ];

    for (name, factory) in providers {
        print!("Testing {}... ", name);
        match factory() {
            Ok(_) => println!("OK"),
            Err(e) => println!("FAIL: {}", e),
        }
    }
}

// Add a dummy method to JS to bypass the fact it wasn't un-commented yet in my mental model
// Wait, I should just check what's actually there.
