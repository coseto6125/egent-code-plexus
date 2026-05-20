//! AST-pattern framework detection for Dart (Flutter / Riverpod).
//!
//! Ported from upstream `_source_code/gitnexus/src/core/ingestion/languages/dart.ts:109-132`.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = DartProvider::new().unwrap();
    let local = provider
        .parse_file("test.dart".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_framework(refs: &[RawFrameworkRef], framework: &str) -> bool {
    refs.iter().any(|r| r.target_name == framework)
}

#[test]
fn flutter_stateless_widget_emits_ref() {
    let src = r#"
        class MyApp extends StatelessWidget {
            @override
            Widget build(BuildContext context) {
                return Container();
            }
        }
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "flutter"));
    assert_eq!(
        refs.iter().filter(|r| r.target_name == "flutter").count(),
        1
    );
}

#[test]
fn flutter_stateful_widget_emits_ref() {
    let src = r#"
        class Counter extends StatefulWidget {
            @override
            State<Counter> createState() => _CounterState();
        }
    "#;
    assert!(has_framework(&parse(src), "flutter"));
}

#[test]
fn flutter_bloc_pattern_emits_ref() {
    let src = r#"
        class CounterCubit extends Cubit<int> {
            CounterCubit() : super(0);
        }
    "#;
    assert!(has_framework(&parse(src), "flutter"));
}

#[test]
fn riverpod_annotation_emits_ref() {
    let src = r#"
        @riverpod
        int counter(CounterRef ref) => 0;
    "#;
    assert!(has_framework(&parse(src), "riverpod"));
}

#[test]
fn riverpod_async_notifier_emits_ref() {
    let src = r#"
        class UserNotifier extends AsyncNotifier<User> {
            @override
            Future<User> build() => Future.value(User());
        }
    "#;
    assert!(has_framework(&parse(src), "riverpod"));
}

#[test]
fn no_framework_patterns_no_refs() {
    let src = r#"
        class Plain {
            int add(int a, int b) => a + b;
        }
    "#;
    assert!(parse(src).is_empty());
}

#[test]
fn flutter_and_riverpod_coexist() {
    let src = r#"
        @riverpod
        class HomePage extends ConsumerWidget {
            @override
            Widget build(BuildContext ctx, WidgetRef ref) => Container();
        }
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "flutter"));
    assert!(has_framework(&refs, "riverpod"));
}
