# Benchmarks & Datasets

This document provides more context on the data used to derive the performance claims in the README.

## The `.sample_repo` Dataset

To ensure `cgn` remains stable and fast across a wide variety of coding styles and languages, we use a custom-curated dataset called `.sample_repo`. 

- **Total Size**: 2.1 GB on disk.
- **File Count**: 17,903 files (excluding >512KB and gitignored).
- **Lines of Code**: ~2.8 million lines (total lines including comments/blanks).
- **Language Breadth**: 25+ detected languages (including niche ones like Move, Vyper, and Cairo).
- **Project Composition**: A collection of ~40 real-world open-source repositories, selected to cover different paradigms:
    - **LadybugDB** (Node.js/TypeScript): The original graph database from GitNexus.
    - **Large-scale Java**: Spring Framework examples and enterprise-grade boilerplate.
    - **Systems Code**: Portions of the Linux kernel (C), Rust crates, and Zig examples.
    - **Web Frameworks**: Laravel (PHP), Django (Python), Rails (Ruby), and Express (JS).
    - **Mobile**: Swift (iOS) and Dart/Flutter examples.
    - **Infra & DevOps**: A large collection of GitHub Actions, Dockerfiles, and Terraform configs.

### Why this dataset?

Most benchmarks use a single monolingual repo. While `cgn` is extremely fast on those, our goal is **Polyglot Intelligence**. `.sample_repo` forces the tool to handle:
1.  **Context switching**: Switching between 25 different Tree-sitter parsers in parallel.
2.  **Cross-repo resolution**: Testing how the graph handles shared symbols across disparate projects.
3.  **Large-file stress**: Identifying bottlenecks in files > 1MB.

## Hardware Environment

All measurements in the README were taken on the following machine:

- **CPU**: AMD Ryzen 9 9950X (16 Physical Cores, 16 Logical used in WSL2).
- **RAM**: 39.2 GiB.
- **OS**: Linux (WSL2 / Ubuntu 24.04).
- **Disk**: NVMe Gen4 SSD.

## Reproducing Results

You can re-run the scalability suite on your own hardware:

```bash
# 1. Ensure you have the sample repo at the root
ls -d .sample_repo

# 2. Run the benchmark script
python3 scripts/benchmark/benchmark_cgn.py
```

To compare directly with the original Node.js implementation:

```bash
python3 scripts/parity/benchmark_vs_gitnexus.py --repo .source_code/gitnexus
```
