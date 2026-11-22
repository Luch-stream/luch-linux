fn main() {
    let mut cc = cc::Build::new();
    let libs = ["libavcodec", "libavutil", "libdrm"];
    for lib in libs {
        let l = pkg_config::Config::new()
            .cargo_metadata(false)
            .probe(lib)
            .unwrap_or_else(|e| panic!("{lib}: {e}"));
        for p in l.include_paths {
            cc.include(p);
        }
    }
    cc.file("src/enc.c").flag("-O2").compile("luchenc");
    // Native dependencies must follow the archive that references them for --as-needed.
    for lib in libs {
        pkg_config::probe_library(lib).unwrap_or_else(|e| panic!("{lib}: {e}"));
    }
    println!("cargo:rerun-if-changed=src/enc.c");
}
