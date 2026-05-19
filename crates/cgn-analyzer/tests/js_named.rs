//! JavaScript parity tests — named node emission covering 3 dimensions
//! from the 7-round audit against ref-gitnexus:
//!   1. Const — const declarations emit NodeKind::Const
//!   2. Function (object-property) — { key: function(){} } emits Function nodes
//!   3. Route — app.use('/path', ...) emits Route

use graph_nexus_analyzer::javascript::parser::JavaScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::graph::NodeKind;

fn provider() -> JavaScriptProvider { JavaScriptProvider::new().unwrap() }

fn parse(src: &str) -> graph_nexus_core::analyzer::types::LocalGraph {
    provider().parse_file("test.js".as_ref(), src.as_bytes()).unwrap()
}

// --- Round 1: Const vs Variable ---

#[test]
fn const_declaration_emits_const_kind() {
    let local = parse("const accept = req.headers['accept'];\nconst opts = {};\n");
    assert!(
        local.nodes.iter().all(|n| n.kind == NodeKind::Const),
        "all const decls should be Const, got: {:?}",
        local.nodes.iter().map(|n| format!("{:?}:{}", n.kind, n.name)).collect::<Vec<_>>()
    );
}

#[test]
fn var_declaration_emits_variable_kind() {
    let local = parse("var x = 1;\nlet y = 2;\n");
    assert!(
        local.nodes.iter().all(|n| n.kind == NodeKind::Variable),
        "var/let should be Variable, got: {:?}",
        local.nodes.iter().map(|n| format!("{:?}:{}", n.kind, n.name)).collect::<Vec<_>>()
    );
}

// --- Round 2: Object property functions ---

#[test]
fn object_property_function_emits_function_node() {
    let local = parse(r#"
app.use(function(req, res, next){
  res.format({
    html: function(){ res.send('<p>hey</p>'); },
    json: function(){ res.json({}); },
    default: function(){ res.send('default'); }
  });
});
"#);
    let fns: Vec<_> = local.nodes.iter().filter(|n| n.kind == NodeKind::Function).collect();
    let names: Vec<&str> = fns.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"html"), "expected html fn, got: {:?}", names);
    assert!(names.contains(&"json"), "expected json fn, got: {:?}", names);
    assert!(names.contains(&"default"), "expected default fn, got: {:?}", names);
}

#[test]
fn object_method_controller_emits_function_nodes() {
    let local = parse(r#"
var User = {
  index: function(req, res){ res.send(users); },
  show: function(req, res){ res.send(users[req.params.id]); },
  destroy: function(req, res, id){ delete users[id]; res.send('ok'); },
  range: function(req, res, a, b, format){ res.send(range); }
};
"#);
    let names: Vec<&str> = local.nodes.iter()
        .filter(|n| n.kind == NodeKind::Function)
        .map(|n| n.name.as_str())
        .collect();
    for expected in ["index", "show", "destroy", "range"] {
        assert!(names.contains(&expected), "expected {} fn, got: {:?}", expected, names);
    }
}

// --- Round 3: app.use routes ---

#[test]
fn app_use_with_path_emits_route() {
    let local = parse(r#"
app.use('/blog', blog);
app.use('/forum', forum);
app.use('/post/:article', fn1, fn2);
"#);
    let paths: Vec<&str> = local.routes.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"/blog"), "expected /blog route, got: {:?}", paths);
    assert!(paths.contains(&"/forum"), "expected /forum route, got: {:?}", paths);
    assert!(paths.contains(&"/post/:article"), "expected /post/:article route, got: {:?}", paths);
}

#[test]
fn router_use_with_path_emits_route() {
    let local = parse(r#"
router.use('/:user/bob/', sub);
router.use('/foo/:ms/', new Router());
"#);
    let paths: Vec<&str> = local.routes.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"/:user/bob/"), "expected /:user/bob/ route, got: {:?}", paths);
    assert!(paths.contains(&"/foo/:ms/"), "expected /foo/:ms/ route, got: {:?}", paths);
}

// Regression: existing framework tests must still pass
#[test]
fn express_use_no_path_not_a_route() {
    // app.use(fn) without a path string — must not emit a route
    let local = parse(r#"
app.use(function(req, res, next){ next(); });
app.use(logger('dev'));
"#);
    // No routes (no path string as first arg)
    assert!(local.routes.is_empty(), "app.use(fn) without path should not emit route, got: {:?}",
        local.routes.iter().map(|r| r.path.as_str()).collect::<Vec<_>>());
}
