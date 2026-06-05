fn main() {
    // Tell cargo to re-run if the NVENC header changes
    println!("cargo:rerun-if-changed=/nix/store/gn3639hjpilimk48gk7fclgyiqymy273-nv-codec-headers-9.1.23.1/include/ffnvcodec/nvEncodeAPI.h");
}
