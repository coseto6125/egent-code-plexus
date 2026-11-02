//! Framework and fan-out `References` of the incremental overlay, against a
//! fresh full index of the edited tree (`common/strict_parity.rs`).
//!
//! The fragment keeps only symbol fields and imports, so the overlay never
//! re-resolves the `References` a Python file's `fanout_refs` (getattr
//! dispatch) and `framework_refs` (FastAPI route and `Depends`, Celery task)
//! produce: it keeps the base index's edges, or drops them, whatever the
//! edit did to the refs. Each case holds the known difference in
//! `tests/baselines/strict_parity/<scenario>.txt`; see `common/baseline.rs`.
//!
//! The case is a repro, not a guard: its baseline is non-empty until the
//! overlay re-resolves both ref lists (FU-2026-10-08-0ed00615af19, owned by
//! P2b), and that change deletes the baseline lines.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use strict_parity::{check, Edit, Scenario};

const P2B_FRAMEWORK_REFS: &str =
    "FU-2026-10-08-0ed00615af19 (P2b: overlay re-resolves framework_refs and fanout_refs)";

const APP_FILE: &str = "app/main.py";

/// A module-level route (`app` -> `read_user`), a `Depends` chain, a Celery
/// task, and a getattr dispatcher with two same-class candidates.
const APP_BASE: &str = r#"from celery import shared_task
from fastapi import FastAPI, Depends

app = FastAPI()


def get_db():
    return None


def get_current_user(db=Depends(get_db)):
    return None


@app.get("/users/{id}")
def read_user(id: int, user=Depends(get_current_user)):
    return user


@shared_task
def send_email(to: str):
    pass


class Dispatcher:
    def dispatch(self, name):
        return getattr(self, name)()

    def handle_create(self):
        return 1

    def handle_delete(self):
        return 2
"#;

/// Each ref flips once: the route verb changes the module-level ref's
/// reason, the `Depends` target changes, the task decorator goes, and a
/// third candidate joins the fan-out (the confidence is split three ways).
const APP_EDITED: &str = r#"from celery import shared_task
from fastapi import FastAPI, Depends

app = FastAPI()


def get_db():
    return None


def get_current_user(db=Depends(get_db)):
    return None


@app.post("/users/{id}")
def read_user(id: int, user=Depends(get_db)):
    return user


def send_email(to: str):
    pass


class Dispatcher:
    def dispatch(self, name):
        return getattr(self, name)()

    def handle_create(self):
        return 1

    def handle_delete(self):
        return 2

    def handle_update(self):
        return 3
"#;

/// Expected in the baseline, without uids (lines of the edited tree):
/// full-only `References` `app` -> `read_user` reason=fastapi-route-post
/// conf=0.9; `read_user` -> `get_db` reason=fastapi-depends conf=0.6;
/// `dispatch` -> `handle_create`, `handle_delete`, `handle_update` reason=
/// reflection-getattr-fanout conf=0.28867513. Overlay-only, the stale base
/// edges: `app` -> `read_user` fastapi-route-get; `read_user` ->
/// `get_current_user` fastapi-depends; File `app/main.py` -> `send_email`
/// celery-task conf=0.9; `dispatch` -> `handle_create` and `handle_delete`
/// reflection-getattr-fanout conf=0.35355338.
#[test]
fn test_check_python_framework_and_fanout_refs_match_baseline() {
    check(&Scenario {
        name: "python_framework_refs",
        owner: P2B_FRAMEWORK_REFS,
        files: &[(APP_FILE, APP_BASE)],
        edits: &[Edit::Write(APP_FILE, APP_EDITED)],
    });
}
