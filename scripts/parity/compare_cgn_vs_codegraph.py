#!/usr/bin/env python3
import json
import sqlite3
import subprocess
import sys
from pathlib import Path

# Kinds to ignore in both systems (like files, imports, modules)
CGN_DROP_KINDS = {"File", "Folder", "Import", "Module"}
CODEGRAPH_DROP_KINDS = {"file", "import", "module", "folder", "namespace"}

def get_codegraph_symbols(db_path: Path):
    print(f"Reading from codegraph DB: {db_path}")
    conn = sqlite3.connect(db_path)
    c = conn.cursor()
    c.execute("SELECT kind, file_path, name FROM nodes")
    
    symbols = set()
    for kind, file_path, name in c.fetchall():
        if kind.lower() in CODEGRAPH_DROP_KINDS:
            continue
        # Normalize kind to lowercase
        norm_kind = kind.lower()
        # Some normalizations for kinds (codegraph uses "constant", cgn uses "Const")
        if norm_kind == "constant":
            norm_kind = "const"
        if norm_kind == "interface":
            norm_kind = "trait" # cgn maps interface to Trait/Class usually
        
        symbols.add((norm_kind, file_path, name))
    
    conn.close()
    return symbols

def get_cgn_symbols(repo_path: Path):
    print(f"Reading from cgn index for: {repo_path}")
    cmd = [
        "cgn",
        "cypher",
        "MATCH (n) RETURN n.kind, n.filePath, n.name",
        "--repo",
        str(repo_path),
        "--format",
        "json"
    ]
    
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, check=True)
    except subprocess.CalledProcessError as e:
        print(f"cgn command failed: {e.stderr}")
        sys.exit(1)
        
    output = proc.stdout.strip()
    
    # Handle l1.refreshed preamble if present
    if "{" in output:
        json_start = output.find("{")
        data = json.loads(output[json_start:])
    else:
        print("Could not parse JSON from cgn output")
        sys.exit(1)
        
    symbols = set()
    for row in data.get("rows", []):
        kind, file_path, name = row
        if kind in CGN_DROP_KINDS:
            continue
        
        # In cgn, n.filePath could be null if it's a global/synthetic node
        if not file_path:
            continue
            
        norm_kind = kind.lower()
        if norm_kind == "class":
            # codegraph differentiates struct/class/interface, cgn might just use Class/Trait
            pass 
        
        symbols.add((norm_kind, file_path, name))
        
    return symbols

def main():
    repo_path = Path(".codegraph")
    db_path = repo_path / ".codegraph" / "codegraph.db"
    
    if not db_path.exists():
        print(f"codegraph DB not found at {db_path}")
        sys.exit(1)
        
    codegraph_symbols = get_codegraph_symbols(db_path)
    cgn_symbols = get_cgn_symbols(repo_path)
    
    # Due to kind mismatching (like Class vs Struct/Trait, Const vs Variable), 
    # we can also compare just (file_path, name) to see pure extraction capability
    cg_pairs = {(f, n) for k, f, n in codegraph_symbols}
    cgn_pairs = {(f, n) for k, f, n in cgn_symbols}
    
    common_pairs = cg_pairs.intersection(cgn_pairs)
    cg_only_pairs = cg_pairs - cgn_pairs
    cgn_only_pairs = cgn_pairs - cg_pairs
    
    print("\n--- Symbol Extraction Parity ---")
    print(f"codegraph total symbols: {len(codegraph_symbols)}")
    print(f"cgn total symbols:       {len(cgn_symbols)}")
    print(f"Common symbols (name+file): {len(common_pairs)}")
    print(f"codegraph ONLY:          {len(cg_only_pairs)}")
    print(f"cgn ONLY:                {len(cgn_only_pairs)}")
    
    with open("scripts/parity/cg_only.txt", "w") as f:
        for p in sorted(list(cg_only_pairs)):
            f.write(f"{p[0]}\t{p[1]}\n")
            
    with open("scripts/parity/cgn_only.txt", "w") as f:
        for p in sorted(list(cgn_only_pairs)):
            f.write(f"{p[0]}\t{p[1]}\n")
            
    print("\nWrote detailed diffs to scripts/parity/cg_only.txt and scripts/parity/cgn_only.txt")

if __name__ == "__main__":
    main()
