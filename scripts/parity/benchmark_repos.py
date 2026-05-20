#!/usr/bin/env python3
import subprocess
import time
from pathlib import Path

REPOS = {
    "JavaScript": "https://github.com/expressjs/express.git",
    "TypeScript": "https://github.com/nestjs/nest.git",
    "Python": "https://github.com/pallets/flask.git",
    "Go": "https://github.com/gin-gonic/gin.git",
    "Rust": "https://github.com/tokio-rs/tokio.git",
    "Java": "https://github.com/google/guava.git",
    "Kotlin": "https://github.com/square/retrofit.git",
    "CSharp": "https://github.com/JamesNK/Newtonsoft.Json.git",
    "C": "https://github.com/redis/redis.git",
    "Cpp": "https://github.com/nlohmann/json.git",
    "PHP": "https://github.com/laravel/framework.git",
    "Ruby": "https://github.com/sinatra/sinatra.git",
    "Swift": "https://github.com/Alamofire/Alamofire.git",
    "Dart": "https://github.com/felangel/bloc.git",
}

SAMPLE_DIR = Path(".sample_repo")


def clone_repo(name: str, url: str) -> Path:
    repo_path = SAMPLE_DIR / name
    if not repo_path.exists():
        print(f"[\u2193] Cloning {name} ({url})...")
        subprocess.run(
            ["git", "clone", "--depth", "1", url, str(repo_path)], check=True, capture_output=True
        )
    return repo_path


def benchmark_analyze(repo_path: Path):
    workspace = Path.cwd()

    # Run ref gitnexus (upstream Python)
    print("  \u251c\u2500 Running ref gitnexus admin index...")
    start_time = time.time()
    subprocess.run(
        ["gitnexus", "admin", "index", "--repo", str(repo_path)], cwd=workspace, capture_output=True
    )
    ref_time = time.time() - start_time

    # Run egent-code-plexus
    print("  \u2514\u2500 Running egent-code-plexus admin index...")
    start_time = time.time()
    subprocess.run(
        [
            "cargo",
            "run",
            "--release",
            "--bin",
            "ecp",
            "--",
            "admin",
            "index",
            "--repo",
            str(repo_path),
        ],
        cwd=workspace,
        capture_output=True,
    )
    ecp_time = time.time() - start_time

    return ref_time, ecp_time


def main():
    SAMPLE_DIR.mkdir(exist_ok=True)

    print("==================================================")
    print("   egent-code-plexus Real-World Performance Benchmark   ")
    print("==================================================\n")

    # Compile in release mode once before benchmarking
    print("[*] Pre-compiling egent-code-plexus in release mode...")
    subprocess.run(
        ["cargo", "build", "--release", "-p", "ecp-cli"], capture_output=True, check=True
    )

    results = []

    # For testing right now, let's just pick 3 representative repos so we don't wait 10 minutes.
    # We can expand to all 14 later.
    test_subset = ["Python", "Rust", "JavaScript"]

    for lang in test_subset:
        url = REPOS[lang]
        try:
            repo_path = clone_repo(lang, url)
            print(f"[\u25b6] Benchmarking {lang} ({repo_path.name})...")

            ref_time, ecp_time = benchmark_analyze(repo_path)

            speedup = ref_time / ecp_time if ecp_time > 0 else float("inf")
            print(
                f"    \u2713 ref gitnexus: {ref_time:.2f}s | egent-code-plexus: {ecp_time:.2f}s | Speedup: {speedup:.1f}x\n"
            )

            results.append((lang, ref_time, ecp_time, speedup))

        except Exception as e:
            print(f"    \u274c Failed: {e}\n")

    print("=== SUMMARY ===")
    for lang, t1, t2, speedup in results:
        print(f"{lang:12}: {t1:6.2f}s -> {t2:6.2f}s ({speedup:.1f}x faster)")


if __name__ == "__main__":
    main()
