//! An annotation type (C# attribute class, Java `@interface`, Kotlin
//! `annotation class`) owns its members like a class: `HasMethod` /
//! `HasProperty` start at the annotation type, not at the enclosing
//! namespace or package. A use-site annotation (Dart `@override`) owns
//! nothing; the member stays with its class.
//!
//! Only these four languages emit an `Annotation` node at parse time; the
//! other ten create none before post-processing, so they have no row here.

mod overrides_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::RelType;
use overrides_support::edge_ends;

/// `(owner, member)` names of every `rel` edge, sorted.
fn owned(
    provider: &dyn LanguageProvider,
    path: &str,
    source: &str,
    rel: RelType,
) -> Vec<(String, String)> {
    let graph = overrides_support::graph_of(provider, &[(path, source)]);
    let mut ends: Vec<(String, String)> = edge_ends(&graph, rel)
        .into_iter()
        .map(|(owner, member)| (owner.name, member.name))
        .collect();
    ends.sort();
    ends
}

fn pair(owner: &str, member: &str) -> (String, String) {
    (owner.to_string(), member.to_string())
}

#[test]
fn test_class_membership_csharp_attribute_class_owns_its_members() {
    let src = "namespace Lib\n{\n    public class MarkAttribute : System.Attribute\n    {\n        public string Name { get; set; }\n        public void Helper() {}\n    }\n}\n";
    let provider = CSharpProvider::new().unwrap();
    assert_eq!(
        owned(&provider, "Lib/Mark.cs", src, RelType::HasMethod),
        [pair("MarkAttribute", "Helper")]
    );
    assert_eq!(
        owned(&provider, "Lib/Mark.cs", src, RelType::HasProperty),
        [pair("MarkAttribute", "Name")]
    );
}

#[test]
fn test_class_membership_java_annotation_type_owns_its_elements() {
    let src = "package lib;\n\npublic @interface Mark {\n    String value();\n}\n";
    let provider = JavaProvider::new().unwrap();
    assert_eq!(
        owned(&provider, "lib/Mark.java", src, RelType::HasMethod),
        [pair("Mark", "value")]
    );
}

#[test]
fn test_class_membership_kotlin_annotation_class_owns_its_constructor() {
    let src = "package lib\n\nannotation class Mark(val name: String)\n";
    let provider = KotlinProvider::new().unwrap();
    assert_eq!(
        owned(&provider, "lib/Mark.kt", src, RelType::HasMethod),
        [pair("Mark", "Mark")]
    );
}

#[test]
fn test_class_membership_dart_use_site_annotation_owns_nothing() {
    let src = "class Shape {\n  @override\n  String toString() => 'shape';\n}\n";
    let provider = DartProvider::new().unwrap();
    assert_eq!(
        owned(&provider, "lib/shape.dart", src, RelType::HasMethod),
        [pair("Shape", "toString")]
    );
}
