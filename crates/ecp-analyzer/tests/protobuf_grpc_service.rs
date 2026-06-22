//! gRPC `service { rpc … }` extraction — the protobuf provider emits one
//! `RawRoute` (method `GRPC`, path `/<package.>Service/Method`) per rpc so the
//! graph builder finalizes it into a `NodeKind::Route`, closing the
//! graph-completeness gap where service contracts were previously invisible
//! (only `message` fields were captured).
//!
//! Config/IaC-style detector (single grammar, `.proto` only), so the
//! 14-mainstream-language coverage rule does not apply — gRPC is a protobuf
//! construct with no per-language variants.

use ecp_analyzer::protobuf::ProtobufProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn routes(src: &str) -> Vec<(String, String)> {
    let provider = ProtobufProvider::new().expect("provider");
    let lg = provider
        .parse_file(Path::new("svc.proto"), src.as_bytes())
        .expect("parse");
    lg.routes.into_iter().map(|r| (r.method, r.path)).collect()
}

#[test]
fn grpc_service_emits_route_per_rpc_with_package_prefix() {
    let proto = "\
syntax = \"proto3\";
package helloworld;

message HelloRequest { string name = 1; }
message HelloReply { string message = 1; }

service Greeter {
  rpc SayHello (HelloRequest) returns (HelloReply);
  rpc SayHelloAgain (HelloRequest) returns (HelloReply);
}
";
    let r = routes(proto);
    assert_eq!(r.len(), 2);
    assert!(r.iter().all(|(m, _)| m == "GRPC"));
    assert_eq!(r[0].1, "/helloworld.Greeter/SayHello");
    assert_eq!(r[1].1, "/helloworld.Greeter/SayHelloAgain");
}

#[test]
fn grpc_streaming_rpc_captured() {
    let proto = "\
package route_guide;
service RouteGuide {
  rpc GetFeature(Point) returns (Feature) {}
  rpc ListFeatures(Rectangle) returns (stream Feature) {}
  rpc RecordRoute(stream Point) returns (RouteSummary) {}
  rpc RouteChat(stream RouteNote) returns (stream RouteNote) {}
}
";
    let r = routes(proto);
    assert_eq!(r.len(), 4);
    assert_eq!(r[0].1, "/route_guide.RouteGuide/GetFeature");
    assert_eq!(r[3].1, "/route_guide.RouteGuide/RouteChat");
}

#[test]
fn proto_without_service_emits_no_routes() {
    // A pure message file (the pre-existing schema-field case) must not gain
    // spurious gRPC routes.
    let proto = "\
package m;
message User {
  string name = 1;
  repeated int32 ids = 2;
}
";
    assert!(routes(proto).is_empty());
}

#[test]
fn proto_still_emits_message_schema_fields() {
    // Regression: adding service extraction must not break the original
    // message-field path. Both coexist in one file.
    let provider = ProtobufProvider::new().expect("provider");
    let proto = "\
package api;
message Req {
  string id = 1;
}
service Svc {
  rpc Do(Req) returns (Req);
}
";
    let lg = provider
        .parse_file(Path::new("api.proto"), proto.as_bytes())
        .expect("parse");
    assert_eq!(lg.routes.len(), 1, "one rpc route");
    let fields = lg.schema_fields.expect("schema fields present");
    assert!(
        fields.iter().any(|f| &*f.name == "id"),
        "message field `id` still extracted"
    );
}
