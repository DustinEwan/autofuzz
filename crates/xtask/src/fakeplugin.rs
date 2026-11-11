//! The fake harness plugin binary. Speaks JSON-RPC over stdio and takes its `invoke`
//! behavior from `AFZ_FAKE_SCRIPT` / `AFZ_FAKE_SCRIPT_FILE`.

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    afz_xtask::fake::serve(
        afz_protocol::buffered(stdin.lock()),
        std::io::BufWriter::new(stdout.lock()),
    );
}
