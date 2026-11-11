//! Repository tasks. `afz-xtask schema` regenerates `docs/schema/`; `afz-xtask fakeplugin`
//! runs the scripted plugin on stdio, which is convenient when debugging the protocol by hand.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() {
    let task = std::env::args().nth(1).unwrap_or_else(|| "help".to_string());
    match task.as_str() {
        "schema" => {
            let dir = std::env::args()
                .nth(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| repo_root().join("docs/schema"));
            match afz_xtask::schema::write_schemas(&dir) {
                Ok(files) => {
                    for f in files {
                        println!("wrote {f}");
                    }
                }
                Err(e) => {
                    eprintln!("afz-xtask: {e}");
                    std::process::exit(1);
                }
            }
        }
        "fakeplugin" => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            afz_xtask::fake::serve(
                afz_protocol::buffered(stdin.lock()),
                std::io::BufWriter::new(stdout.lock()),
            );
        }
        other => {
            eprintln!("usage: afz-xtask <schema [dir]|fakeplugin>");
            if other != "help" {
                std::process::exit(2);
            }
        }
    }
}
