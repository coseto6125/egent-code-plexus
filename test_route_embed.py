import json
import subprocess
import time


def run_command(cmd):
    result = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"Error running: {cmd}")
        print(result.stderr)
        return None
    return result.stdout


def print_section(title):
    print("\n" + "=" * 50)
    print(f" {title} ")
    print("=" * 50)


# 1. Analyze Flask Repo (Python)
print_section("1. Analyzing Flask Repo (.sample_repo/Python)")
run_command("cargo run --bin gnx -- analyze --repo .sample_repo/Python")

# 2. Extract Route Map
print_section("2. Extracting API Routes (route_map) from Flask")
routes_json = run_command(
    "cargo run --bin gnx -- route-map --repo .sample_repo/Python --format json"
)
if routes_json:
    try:
        data = json.loads(routes_json)
        results = data.get("results", [])
        print(f"Found {len(results)} routes!")
        for route in results[:10]:  # Print top 10
            print(f"- {route['name']} -> {route['filePath']}:{route['line']}")
    except Exception as e:
        print(f"Failed to parse JSON: {e}")

# 3. Analyze Flask Repo with Embeddings (Swift)
# For the sake of time and LLM resources, we will only do the Flask one with embeddings to see semantic search in action.
print_section("3. Analyzing Flask Repo with Embeddings (BGE-M3)")
start_time = time.time()
run_command("cargo run --bin gnx -- analyze --repo .sample_repo/Python --embeddings")
print(f"Embeddings generation took {time.time() - start_time:.2f} seconds.")

# 4. Semantic Query
print_section("4. Semantic Query: 'cookie and session management'")
query_json = run_command(
    "cargo run --bin gnx -- query --query 'cookie and session management' --repo .sample_repo/Python --format json"
)
if query_json:
    try:
        data = json.loads(query_json)
        results = data.get("results", [])
        print(f"Found {len(results)} results via Semantic Search:")
        for res in results[:5]:  # Print top 5
            print(
                f"- [{res['score']:.4f}] {res['kind']} {res['name']} ({res['filePath']}:{res['line']})"
            )
    except Exception as e:
        print(f"Failed to parse JSON: {e}")
