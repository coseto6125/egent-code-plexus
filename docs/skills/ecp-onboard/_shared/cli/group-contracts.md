List contracts with optional filtering

Usage: ecp group contracts [OPTIONS] <NAME>

Arguments:
  <NAME>  Group name

Options:
      --type <TYPE>    Filter by contract type (http|grpc|thrift|topic|lib|include|custom)
      --repo <REPO>    Filter by repo name
      --unmatched      Show only unmatched contracts
      --json           Emit JSON instead of text
      --graph <GRAPH>  Path to the graph.bin file [default: .ecp/graph.bin]
  -h, --help           Print help
