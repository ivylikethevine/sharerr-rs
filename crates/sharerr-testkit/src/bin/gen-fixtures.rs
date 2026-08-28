//! Write the synthetic media library to a directory.
//!
//! The compose stack mounts the result as its library, so the live stack and the
//! unit tests share one definition of what the fixtures are. Regenerating is
//! idempotent — the content is seeded, so the same bytes come back every time.
//!
//! ```text
//! cargo run -p sharerr-testkit --bin gen-fixtures -- tests/fixtures/media
//! ```

#![allow(clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sharerr_testkit::Library;

type Builder = fn(&Path) -> std::io::Result<Library>;

const BUILDERS: [(&str, Builder); 3] = [
    ("tv", sharerr_testkit::tv_library),
    ("movie", sharerr_testkit::movie_library),
    ("music", sharerr_testkit::music_library),
];

fn main() -> ExitCode {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: gen-fixtures <directory>");
        eprintln!("       writes a synthetic tv/ and movies/ tree with invented titles");
        return ExitCode::FAILURE;
    };

    let mut libraries = Vec::with_capacity(BUILDERS.len());
    for (name, build) in BUILDERS {
        match build(&root) {
            Ok(library) => libraries.push(library),
            Err(err) => {
                eprintln!("could not write the {name} library: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    println!(
        "wrote {} synthetic file(s) under {}",
        libraries.iter().map(|l| l.files.len()).sum::<usize>(),
        root.display()
    );
    for file in libraries.iter().flat_map(|l| &l.files) {
        println!("  {} ({} bytes)", file.disk_path.display(), file.size);
    }
    println!();
    println!("every title here is invented; nothing corresponds to real content");

    ExitCode::SUCCESS
}
