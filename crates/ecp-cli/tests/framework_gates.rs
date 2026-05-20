//! Integration test: framework gates — only emit framework refs when relevant
//! imports are present. Reflection fan-out and blind_spots are NOT gated, so
//! they must keep emitting even when no framework import exists.

use ecp_analyzer::python::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;

#[test]
fn fastapi_depends_without_import_does_not_emit() {
    // No `from fastapi import Depends` — `Depends` is just a local helper.
    let src = r#"
def Depends(callable):
    return callable

def get_db():
    return None

def handler(db = Depends(get_db)):
    return db
"#;
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    let fastapi_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason.starts_with("fastapi-"))
        .collect();
    assert!(
        fastapi_refs.is_empty(),
        "no `from fastapi` import → must not emit fastapi refs. got: {:?}",
        fastapi_refs
    );
}

#[test]
fn fastapi_depends_with_import_emits() {
    let src = r#"
from fastapi import Depends

def get_db():
    return None

def handler(db = Depends(get_db)):
    return db
"#;
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    let fastapi_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "fastapi-depends")
        .collect();
    assert_eq!(
        fastapi_refs.len(),
        1,
        "with import → should emit. got: {:?}",
        local.framework_refs
    );
}

#[test]
fn django_urlpatterns_without_import_does_not_emit() {
    // Local `path` callable, list named `urlpatterns`, but no django import.
    let src = r#"
def path(p, h):
    return (p, h)

def view():
    return "hi"

urlpatterns = [path("/x", view)]
"#;
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("urls.py".as_ref(), src.as_bytes())
        .unwrap();

    let django_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "django-url-path")
        .collect();
    assert!(
        django_refs.is_empty(),
        "no django.urls import → must not emit. got: {:?}",
        django_refs
    );
}

#[test]
fn celery_task_without_import_does_not_emit() {
    let src = r#"
def shared_task(fn):
    return fn

@shared_task
def my_task():
    pass
"#;
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("tasks.py".as_ref(), src.as_bytes())
        .unwrap();

    let celery_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "celery-task")
        .collect();
    assert!(
        celery_refs.is_empty(),
        "no celery import → must not emit. got: {:?}",
        celery_refs
    );
}

#[test]
fn reflection_fanout_not_gated() {
    // getattr fan-out is a language-level pattern, not framework-specific.
    let src = r#"
class Dispatcher:
    def dispatch(self, name):
        return getattr(self, name)()

    def handle_a(self):
        pass

    def handle_b(self):
        pass
"#;
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("d.py".as_ref(), src.as_bytes())
        .unwrap();

    assert!(
        !local.fanout_refs.is_empty(),
        "fan-out is not framework-specific, should not be gated"
    );
}

#[test]
fn blind_spots_not_gated() {
    // eval() is a blind spot regardless of any framework import.
    let src = r#"
def runtime_eval(x):
    return eval(x)
"#;
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("e.py".as_ref(), src.as_bytes())
        .unwrap();

    let eval_spots: Vec<_> = local
        .blind_spots
        .iter()
        .filter(|bs| bs.kind == "python-eval")
        .collect();
    assert!(!eval_spots.is_empty(), "blind_spots not gated, should emit");
}
