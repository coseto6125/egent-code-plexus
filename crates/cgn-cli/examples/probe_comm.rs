use cgn_core::graph::ArchivedZeroCopyGraph;
use memmap2::Mmap;
use std::fs::File;

fn main() {
    let path = "/tmp/gnx_ts_test/.gnx/graph.bin";
    let f = File::open(path).unwrap();
    let mmap = unsafe { Mmap::map(&f).unwrap() };
    let g = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&mmap).unwrap();
    println!("process_start={}", g.process_start.to_native());
    println!("total nodes={}", g.nodes.len());
    println!();
    for (i, n) in g.nodes.iter().enumerate() {
        let name = n.name.resolve(&g.string_pool);
        let kind = format!("{:?}", n.kind);
        println!(
            "node[{}] kind={} community={} name={}",
            i,
            kind,
            n.community_id.to_native(),
            name
        );
    }
    println!();
    for k in 0..g.traces_offsets.len().saturating_sub(1) {
        let s = g.traces_offsets[k].to_native() as usize;
        let e = g.traces_offsets[k + 1].to_native() as usize;
        let trace: Vec<u32> = g.traces_data[s..e].iter().map(|x| x.to_native()).collect();
        let names: Vec<String> = trace
            .iter()
            .map(|&i| g.nodes[i as usize].name.resolve(&g.string_pool).to_string())
            .collect();
        let comms: Vec<u16> = trace
            .iter()
            .map(|&i| g.nodes[i as usize].community_id.to_native())
            .collect();
        println!("trace[{}] = {:?} (comms = {:?})", k, names, comms);
    }
}
